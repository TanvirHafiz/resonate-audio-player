use crate::db::Db;
use rusqlite::params;

/// Records a play_events row for a track that has just stopped being the
/// active one (either it finished naturally, the user skipped past it, or
/// playback was stopped). Called at every playback transition point so the
/// listening history stays live rather than being reconstructed later.
///
/// Plays under 1 second are ignored as noise (e.g. hitting "next" before
/// audio has audibly started).
pub fn record(db: &Db, track_id: i64, position_seconds: f64, session_id: i64) -> Result<(), String> {
    if position_seconds < 1.0 {
        return Ok(());
    }

    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let duration: f64 = conn
        .query_row(
            "SELECT duration_seconds FROM tracks WHERE id = ?1",
            params![track_id],
            |row| row.get(0),
        )
        .unwrap_or(0.0);

    // Streams report 0 duration (unknown/live) so we never call them "skipped".
    let skipped = duration > 0.0 && position_seconds < duration - 2.0;

    conn.execute(
        "INSERT INTO play_events (track_id, played_seconds, skipped, skip_point_seconds, session_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            track_id,
            position_seconds,
            skipped,
            if skipped { Some(position_seconds) } else { None },
            session_id
        ],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}
