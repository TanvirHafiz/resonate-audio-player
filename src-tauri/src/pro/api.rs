use super::enrichment;
use super::playlists::{self, Playlist};
use super::recommendation_engine;
use super::worker::FeatureQueue;
use crate::api::{track_from_row, TRACK_COLUMNS};
use crate::db::Db;
use crate::models::Track;
use crate::playback::{self, Player};
use rusqlite::params;
use serde::Serialize;
use tauri::State;

fn tracks_by_ids(db: &Db, ids: &[i64]) -> Result<Vec<Track>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE id IN ({placeholders})");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
    let rows: std::collections::HashMap<i64, Track> = stmt
        .query_map(params.as_slice(), track_from_row)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|t| (t.id, t))
        .collect();
    // Preserve the recommendation engine's ranking order.
    Ok(ids.iter().filter_map(|id| rows.get(id).cloned()).collect())
}

#[tauri::command]
pub fn more_like_this(db: State<Db>, track_id: i64, limit: usize) -> Result<Vec<Track>, String> {
    let ids = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        recommendation_engine::more_like_this(&conn, track_id, limit)?
    };
    tracks_by_ids(&db, &ids)
}

#[tauri::command]
pub fn for_you_queue(db: State<Db>, limit: usize) -> Result<Vec<Track>, String> {
    let ids = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        recommendation_engine::for_you_queue(&conn, limit)?
    };
    tracks_by_ids(&db, &ids)
}

#[derive(Serialize)]
pub struct TrackTags {
    pub track_id: i64,
    pub mood: Vec<String>,
    pub genre: Vec<String>,
}

/// Bulk tag lookup for the whole library, so the frontend can badge every
/// row from one call instead of one round trip per track.
#[tauri::command]
pub fn get_all_track_tags(db: State<Db>) -> Result<Vec<TrackTags>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT tt.track_id, t.name, t.kind
             FROM track_tags tt
             JOIN tags t ON t.id = tt.tag_id
             ORDER BY tt.track_id",
        )
        .map_err(|e| e.to_string())?;

    let rows: Vec<(i64, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    let mut by_track: std::collections::HashMap<i64, TrackTags> = std::collections::HashMap::new();
    for (track_id, name, kind) in rows {
        let entry = by_track.entry(track_id).or_insert(TrackTags {
            track_id,
            mood: Vec::new(),
            genre: Vec::new(),
        });
        if kind == "mood" {
            entry.mood.push(name);
        } else {
            entry.genre.push(name);
        }
    }

    Ok(by_track.into_values().collect())
}

/// Whether `track_id` has completed audio analysis yet (feature extraction
/// runs in the background, so a just-added track may briefly have none).
#[tauri::command]
pub fn is_track_analyzed(db: State<Db>, track_id: i64) -> Result<bool, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT 1 FROM track_features WHERE track_id = ?1",
        params![track_id],
        |_| Ok(true),
    )
    .or(Ok(false))
}

#[tauri::command]
pub fn requeue_unanalyzed_tracks(db: State<Db>, queue: State<FeatureQueue>) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let ids = super::worker::find_unanalyzed_tracks(&conn);
    let count = ids.len();
    queue.enqueue_all(ids);
    Ok(count)
}

// ---------- Playlists ----------

#[tauri::command]
pub fn create_playlist(db: State<Db>, name: String) -> Result<i64, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    playlists::create(&conn, &name)
}

#[tauri::command]
pub fn rename_playlist(db: State<Db>, playlist_id: i64, name: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    playlists::rename(&conn, playlist_id, &name)
}

#[tauri::command]
pub fn delete_playlist(db: State<Db>, playlist_id: i64) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    playlists::delete(&conn, playlist_id)
}

#[tauri::command]
pub fn list_playlists(db: State<Db>) -> Result<Vec<Playlist>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    playlists::list(&conn)
}

#[tauri::command]
pub fn get_playlist_tracks(db: State<Db>, playlist_id: i64) -> Result<Vec<Track>, String> {
    let ids = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        playlists::tracks(&conn, playlist_id)?
    };
    tracks_by_ids(&db, &ids)
}

#[tauri::command]
pub fn add_track_to_playlist(db: State<Db>, playlist_id: i64, track_id: i64) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    playlists::add_track(&conn, playlist_id, track_id)
}

#[tauri::command]
pub fn remove_track_from_playlist(db: State<Db>, playlist_id: i64, track_id: i64) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    playlists::remove_track(&conn, playlist_id, track_id)
}

// ---------- Settings ----------

#[tauri::command]
pub fn remove_library_folder(db: State<Db>, path: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    super::settings::remove_library_folder(&conn, &path)
}

#[tauri::command]
pub fn list_output_devices() -> Vec<String> {
    playback::list_output_devices()
}

#[tauri::command]
pub fn get_current_output_device(player: State<Player>) -> String {
    playback::current_output_device(&player)
}

#[tauri::command]
pub fn set_output_device(player: State<Player>, device_name: String) -> Result<(), String> {
    playback::set_output_device(&player, &device_name)
}

// ---------- Equalizer ----------

#[tauri::command]
pub fn list_eq_presets() -> Vec<&'static str> {
    playback::eq_preset_names()
}

#[tauri::command]
pub fn get_current_eq_preset(player: State<Player>) -> String {
    playback::current_eq_preset(&player)
}

#[tauri::command]
pub fn set_eq_preset(player: State<Player>, name: String) {
    playback::set_eq_preset(&player, &name);
}

// ---------- Enrichment (lyrics / artist bio) ----------

#[tauri::command]
pub fn fetch_lyrics(artist: String, title: String) -> Result<Option<String>, String> {
    enrichment::fetch_lyrics(&artist, &title)
}

#[derive(Serialize)]
pub struct ArtistBioResponse {
    pub summary: String,
    pub source_url: Option<String>,
}

#[tauri::command]
pub fn fetch_artist_bio(artist: String) -> Result<Option<ArtistBioResponse>, String> {
    Ok(enrichment::fetch_artist_bio(&artist)?.map(|bio| ArtistBioResponse {
        summary: bio.summary,
        source_url: bio.source_url,
    }))
}
