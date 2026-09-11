use crate::db::Db;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey};
use rusqlite::params;
use std::path::Path;
use walkdir::WalkDir;

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "wav", "m4a", "aac"];

struct ParsedTrack {
    file_path: String,
    title: String,
    artist: String,
    album: String,
    year: Option<i64>,
    duration_seconds: f64,
}

fn parse_track(path: &Path) -> ParsedTrack {
    let fallback_title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown Title")
        .to_string();

    let probed = Probe::open(path).ok().and_then(|p| p.read().ok());

    let mut title = fallback_title.clone();
    let mut artist = "Unknown Artist".to_string();
    let mut album = "Unknown Album".to_string();
    let mut year: Option<i64> = None;
    let mut duration_seconds = 0.0;

    if let Some(tagged_file) = probed {
        duration_seconds = tagged_file.properties().duration().as_secs_f64();

        let tag = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag());

        if let Some(tag) = tag {
            if let Some(t) = tag.title() {
                title = t.to_string();
            }
            if let Some(a) = tag.artist() {
                artist = a.to_string();
            }
            if let Some(al) = tag.album() {
                album = al.to_string();
            }
            let year_str = tag
                .get_string(ItemKey::Year)
                .or_else(|| tag.get_string(ItemKey::RecordingDate));
            if let Some(y) = year_str.and_then(|s| s.get(0..4)).and_then(|s| s.parse::<i64>().ok()) {
                year = Some(y);
            }
        }
    }

    ParsedTrack {
        file_path: path.to_string_lossy().to_string(),
        title,
        artist,
        album,
        year,
        duration_seconds,
    }
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn insert_if_new(conn: &rusqlite::Connection, path: &Path) -> Result<Option<i64>, String> {
    let file_path = path.to_string_lossy().to_string();
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM tracks WHERE file_path = ?1",
            params![file_path],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if exists {
        return Ok(None);
    }

    let parsed = parse_track(path);
    conn.execute(
        "INSERT INTO tracks (file_path, title, artist, album, year, duration_seconds, source_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'local')",
        params![
            parsed.file_path,
            parsed.title,
            parsed.artist,
            parsed.album,
            parsed.year,
            parsed.duration_seconds
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Recursively scans a folder for audio files, parses metadata, and inserts
/// new tracks into the database (existing file_path entries are skipped).
/// Returns the ids of newly added tracks.
pub fn scan_folder(db: &Db, folder: &str) -> Result<Vec<i64>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT OR IGNORE INTO library_folders (path) VALUES (?1)",
        params![folder],
    )
    .map_err(|e| e.to_string())?;

    let mut added = Vec::new();

    for entry in WalkDir::new(folder)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        if !is_audio_file(path) {
            continue;
        }
        if let Some(id) = insert_if_new(&conn, path)? {
            added.push(id);
        }
    }

    Ok(added)
}

/// Adds individual dropped paths (files and/or folders) to the library.
/// Folders are scanned recursively; files are added directly if they look
/// like audio. Existing file_path entries are skipped. Returns the ids of
/// newly added tracks.
pub fn add_paths(db: &Db, paths: &[String]) -> Result<Vec<i64>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut added = Vec::new();

    for raw in paths {
        let path = Path::new(raw);
        if path.is_dir() {
            for entry in WalkDir::new(path)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
            {
                let entry_path = entry.path();
                if is_audio_file(entry_path) {
                    if let Some(id) = insert_if_new(&conn, entry_path)? {
                        added.push(id);
                    }
                }
            }
        } else if is_audio_file(path) {
            if let Some(id) = insert_if_new(&conn, path)? {
                added.push(id);
            }
        }
    }

    Ok(added)
}

/// Adds a streaming source (internet radio, podcast episode, or arbitrary
/// URL) to the library. Returns the existing track id if this URL was
/// already added.
pub fn add_stream(
    db: &Db,
    url: &str,
    title: &str,
    source_type: &str,
) -> Result<i64, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    if let Ok(existing_id) = conn.query_row(
        "SELECT id FROM tracks WHERE stream_url = ?1",
        params![url],
        |row| row.get::<_, i64>(0),
    ) {
        return Ok(existing_id);
    }

    let title = if title.trim().is_empty() { url } else { title };
    conn.execute(
        "INSERT INTO tracks (stream_url, title, artist, album, duration_seconds, source_type)
         VALUES (?1, ?2, '', '', 0, ?3)",
        params![url, title, source_type],
    )
    .map_err(|e| e.to_string())?;

    Ok(conn.last_insert_rowid())
}

/// Removes tracks (and their derived rows, via ON DELETE CASCADE) from the
/// library. Does not touch the underlying files on disk.
pub fn remove_tracks(db: &Db, track_ids: &[i64]) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut removed = 0usize;
    for id in track_ids {
        removed += conn
            .execute("DELETE FROM tracks WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

pub fn library_folders(db: &Db) -> Result<Vec<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT path FROM library_folders ORDER BY added_at")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}
