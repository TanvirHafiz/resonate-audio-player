use super::{feature_extraction, mood_tagging};
use crate::db;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread;

/// Background queue that extracts audio features and assigns mood/genre
/// tags for newly-added local tracks without blocking library ingest.
/// Runs on its own dedicated SQLite connection (WAL mode lets it operate
/// concurrently with the main connection).
pub struct FeatureQueue {
    sender: Sender<i64>,
}

impl FeatureQueue {
    pub fn spawn(app_data_dir: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel::<i64>();
        thread::spawn(move || {
            let conn = db::open_connection(&app_data_dir);
            for track_id in receiver {
                if let Err(e) = process_track(&conn, track_id) {
                    eprintln!("[pro] feature extraction failed for track {track_id}: {e}");
                }
            }
        });
        FeatureQueue { sender }
    }

    pub fn enqueue(&self, track_id: i64) {
        let _ = self.sender.send(track_id);
    }

    pub fn enqueue_all(&self, track_ids: impl IntoIterator<Item = i64>) {
        for id in track_ids {
            self.enqueue(id);
        }
    }
}

/// Tracks longer than this are treated as audiobooks/podcasts rather than
/// music and skipped outright — mood/tempo analysis doesn't mean much for
/// them, and there's no reason to even open a multi-hour file.
const MAX_TRACK_DURATION_SECONDS: f64 = 20.0 * 60.0;

fn process_track(conn: &Connection, track_id: i64) -> Result<(), String> {
    let (file_path, duration): (Option<String>, f64) = conn
        .query_row(
            "SELECT file_path, duration_seconds FROM tracks WHERE id = ?1",
            [track_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?;

    let Some(path) = file_path else {
        return Ok(()); // stream track, nothing local to analyze
    };

    if duration > MAX_TRACK_DURATION_SECONDS {
        return Ok(());
    }

    let features = feature_extraction::analyze(&path)?;

    conn.execute(
        "INSERT INTO track_features (track_id, tempo_bpm, energy, loudness, brightness, acousticness, valence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(track_id) DO UPDATE SET
            tempo_bpm = excluded.tempo_bpm,
            energy = excluded.energy,
            loudness = excluded.loudness,
            brightness = excluded.brightness,
            acousticness = excluded.acousticness,
            valence = excluded.valence,
            extracted_at = CURRENT_TIMESTAMP",
        rusqlite::params![
            track_id,
            features.tempo_bpm,
            features.energy,
            features.loudness,
            features.brightness,
            features.acousticness,
            features.valence
        ],
    )
    .map_err(|e| e.to_string())?;

    let genre = mood_tagging::read_genre(&path);
    mood_tagging::apply_tags(conn, track_id, &features, genre.as_deref())?;

    Ok(())
}

/// Finds local tracks that don't have a `track_features` row yet — used at
/// startup to backfill analysis for a library that predates the pro tier
/// (or any track that failed analysis previously).
pub fn find_unanalyzed_tracks(conn: &Connection) -> Vec<i64> {
    let mut stmt = match conn.prepare(
        "SELECT t.id FROM tracks t
         WHERE t.source_type = 'local' AND t.file_path IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM track_features tf WHERE tf.track_id = t.id)",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| row.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}
