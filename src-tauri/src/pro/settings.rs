use rusqlite::{params, Connection};

/// Removes a library folder entry and hides every local track under that
/// path from the library. A track still referenced by a playlist is only
/// hidden (`in_library = 0`), not deleted, so playlists keep it; tracks not
/// in any playlist are deleted outright.
pub fn remove_library_folder(conn: &Connection, path: &str) -> Result<(), String> {
    let like_pattern = format!("{path}%");

    let ids: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM tracks WHERE source_type = 'local' AND file_path LIKE ?1")
            .map_err(|e| e.to_string())?;
        let result: Vec<i64> = stmt
            .query_map(params![like_pattern], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        result
    };

    for id in ids {
        let in_playlist: bool = conn
            .query_row(
                "SELECT 1 FROM playlist_tracks WHERE track_id = ?1 LIMIT 1",
                params![id],
                |_| Ok(true),
            )
            .unwrap_or(false);

        let sql = if in_playlist {
            "UPDATE tracks SET in_library = 0 WHERE id = ?1"
        } else {
            "DELETE FROM tracks WHERE id = ?1"
        };
        conn.execute(sql, params![id]).map_err(|e| e.to_string())?;
    }

    conn.execute("DELETE FROM library_folders WHERE path = ?1", params![path])
        .map_err(|e| e.to_string())?;
    Ok(())
}
