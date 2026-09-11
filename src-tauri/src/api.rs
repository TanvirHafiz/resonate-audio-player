use crate::db::Db;
use crate::history;
use crate::ingest;
use crate::models::{NowPlaying, Track};
use crate::playback::{self, Player};
use crate::streaming::{self, PodcastEpisode};
use rusqlite::params;
#[cfg(feature = "pro")]
use tauri::Manager;
use tauri::State;

pub(crate) fn track_from_row(row: &rusqlite::Row) -> rusqlite::Result<Track> {
    Ok(Track {
        id: row.get(0)?,
        file_path: row.get(1)?,
        stream_url: row.get(2)?,
        title: row.get(3)?,
        artist: row.get(4)?,
        album: row.get(5)?,
        year: row.get(6)?,
        duration_seconds: row.get(7)?,
        source_type: row.get(8)?,
    })
}

pub(crate) const TRACK_COLUMNS: &str =
    "id, file_path, stream_url, title, artist, album, year, duration_seconds, source_type";

#[allow(unused_variables)]
fn enqueue_for_analysis(app: &tauri::AppHandle, track_ids: &[i64]) {
    #[cfg(feature = "pro")]
    if let Some(queue) = app.try_state::<crate::pro::worker::FeatureQueue>() {
        queue.enqueue_all(track_ids.iter().copied());
    }
}

#[tauri::command]
pub fn scan_folder(app: tauri::AppHandle, db: State<Db>, folder: String) -> Result<usize, String> {
    let ids = ingest::scan_folder(&db, &folder)?;
    enqueue_for_analysis(&app, &ids);
    Ok(ids.len())
}

#[tauri::command]
pub fn add_files(app: tauri::AppHandle, db: State<Db>, paths: Vec<String>) -> Result<usize, String> {
    let ids = ingest::add_paths(&db, &paths)?;
    enqueue_for_analysis(&app, &ids);
    Ok(ids.len())
}

#[tauri::command]
pub fn remove_tracks(db: State<Db>, track_ids: Vec<i64>) -> Result<usize, String> {
    ingest::remove_tracks(&db, &track_ids)
}

#[tauri::command]
pub fn add_stream(
    db: State<Db>,
    url: String,
    title: String,
    source_type: String,
) -> Result<i64, String> {
    ingest::add_stream(&db, &url, &title, &source_type)
}

#[tauri::command]
pub fn fetch_podcast_feed(feed_url: String) -> Result<Vec<PodcastEpisode>, String> {
    streaming::fetch_podcast_episodes(&feed_url)
}

#[tauri::command]
pub fn debug_log(message: String) {
    eprintln!("[frontend] {message}");
}

/// Reports whether this build includes the paid "pro" tier (Phase 3+:
/// mood/genre intelligence, recommendations, and other advanced features).
/// The frontend uses this to decide whether to show that functionality.
#[tauri::command]
pub fn app_tier() -> &'static str {
    if cfg!(feature = "pro") {
        "pro"
    } else {
        "free"
    }
}

#[tauri::command]
pub fn get_library_folders(db: State<Db>) -> Result<Vec<String>, String> {
    ingest::library_folders(&db)
}

#[tauri::command]
pub fn get_tracks(db: State<Db>) -> Result<Vec<Track>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let sql =
        format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE in_library = 1 ORDER BY artist, album, title");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], track_from_row)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn get_track_by_id(db: &Db, track_id: i64) -> Result<Option<Track>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let sql = format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE id = ?1");
    conn.query_row(&sql, params![track_id], track_from_row)
        .map(Some)
        .or_else(|e| {
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(e.to_string())
            }
        })
}

fn start_track(
    db: &Db,
    player: &Player,
    track_id: i64,
) -> Result<Option<Track>, String> {
    let Some(track) = get_track_by_id(db, track_id)? else {
        return Ok(None);
    };
    let result = match (&track.file_path, &track.stream_url) {
        (Some(path), _) => playback::play_file(player, path),
        (None, Some(url)) => playback::play_stream(player, url),
        (None, None) => Err("track has neither a file nor a stream URL".into()),
    };
    if let Err(e) = &result {
        eprintln!("start_track failed for track {track_id} ({:?}): {e}", track.file_path);
    }
    result?;
    Ok(Some(track))
}

/// Records a play_events row for whatever is currently loaded (if anything),
/// using its position at the moment of the call. Must be called *before*
/// the queue/current-track state is mutated, since it reads that state to
/// know what was just playing.
pub fn record_history_for_current(db: &Db, player: &Player) -> Result<(), String> {
    let (queue, queue_index) = playback::current_queue_snapshot(player);
    let Some(track_id) = queue_index.and_then(|i| queue.get(i)).copied() else {
        return Ok(());
    };
    let position = playback::position_seconds(player);
    history::record(db, track_id, position, playback::session_id(player))?;

    #[cfg(feature = "pro")]
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        crate::pro::affinity_engine::recompute(&conn)?;
    }

    Ok(())
}

/// Plays `track_id`, setting the full visible track list as the queue
/// (in list order) so next/previous can walk through it.
#[tauri::command]
pub fn play_track(
    db: State<Db>,
    player: State<Player>,
    track_id: i64,
) -> Result<Option<Track>, String> {
    record_history_for_current(&db, &player)?;
    let all_tracks = get_tracks(db.clone())?;
    let index = all_tracks.iter().position(|t| t.id == track_id).unwrap_or(0);
    let ids: Vec<i64> = all_tracks.iter().map(|t| t.id).collect();
    playback::set_queue(&player, ids, index);
    start_track(&db, &player, track_id)
}

#[tauri::command]
pub fn toggle_play_pause(player: State<Player>) -> bool {
    if playback::is_playing(&player) {
        playback::pause(&player);
    } else {
        playback::resume(&player);
    }
    playback::is_playing(&player)
}

#[tauri::command]
pub fn stop_playback(db: State<Db>, player: State<Player>) {
    let _ = record_history_for_current(&db, &player);
    playback::stop(&player);
}

#[tauri::command]
pub fn next_track(db: State<Db>, player: State<Player>) -> Result<Option<Track>, String> {
    record_history_for_current(&db, &player)?;
    match playback::advance_queue(&player, 1) {
        Some(track_id) => start_track(&db, &player, track_id),
        None => {
            playback::stop(&player);
            Ok(None)
        }
    }
}

#[tauri::command]
pub fn previous_track(db: State<Db>, player: State<Player>) -> Result<Option<Track>, String> {
    record_history_for_current(&db, &player)?;
    match playback::advance_queue(&player, -1) {
        Some(track_id) => start_track(&db, &player, track_id),
        None => Ok(None),
    }
}

#[tauri::command]
pub fn set_volume(player: State<Player>, volume: f32) {
    playback::set_volume(&player, volume);
}

#[tauri::command]
pub fn seek_to(player: State<Player>, position_seconds: f64) -> Result<(), String> {
    playback::seek(&player, position_seconds)
}

#[tauri::command]
pub fn get_now_playing(db: State<Db>, player: State<Player>) -> Result<NowPlaying, String> {
    let (queue, queue_index) = playback::current_queue_snapshot(&player);
    let current_track_id = queue_index.and_then(|i| queue.get(i)).copied();

    // Auto-advance when the current track has finished playing.
    if current_track_id.is_some() && playback::sink_empty(&player) {
        record_history_for_current(&db, &player)?;
        if let Some(next_id) = playback::advance_queue(&player, 1) {
            start_track(&db, &player, next_id)?;
        } else {
            playback::stop(&player);
        }
    }

    let (queue, queue_index) = playback::current_queue_snapshot(&player);
    let current_track_id = queue_index.and_then(|i| queue.get(i)).copied();
    let track = match current_track_id {
        Some(id) => get_track_by_id(&db, id)?,
        None => None,
    };

    Ok(NowPlaying {
        track,
        is_playing: playback::is_playing(&player),
        position_seconds: playback::position_seconds(&player),
        volume: playback::get_volume(&player),
        queue_index,
        queue_len: queue.len(),
    })
}
