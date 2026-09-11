use super::feature_extraction::Features;
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
use lofty::tag::Accessor;
use rusqlite::{params, Connection};

/// Rule-based mood tags derived from the (heuristic) feature vector. A real
/// pretrained audio-tagging model was explicitly deferred for now; this
/// fills the same role with simple, explainable thresholds instead.
fn mood_labels(f: &Features) -> Vec<&'static str> {
    let mut labels = Vec::new();

    if f.energy > 0.6 && f.valence > 0.6 {
        labels.push("Energetic");
    }
    if f.energy < 0.35 && f.valence < 0.45 {
        labels.push("Melancholic");
    }
    if f.tempo_bpm > 0.0 && f.tempo_bpm < 95.0 && f.energy < 0.5 {
        labels.push("Chill");
    }
    if f.tempo_bpm >= 120.0 && f.energy >= 0.5 {
        labels.push("Driving");
    }
    if f.acousticness > 0.65 {
        labels.push("Acoustic");
    }
    if f.valence >= 0.55 && f.energy < 0.55 {
        labels.push("Warm");
    }
    if labels.is_empty() {
        labels.push("Balanced");
    }
    labels
}

/// Reads the ID3/Vorbis genre field, if present, as a fallback source of
/// genre metadata (no audio-based genre classification is attempted).
pub fn read_genre(path: &str) -> Option<String> {
    let tagged_file = Probe::open(path).ok()?.read().ok()?;
    let tag = tagged_file.primary_tag().or_else(|| tagged_file.first_tag())?;
    tag.genre().map(|g| g.trim().to_string()).filter(|g| !g.is_empty())
}

fn upsert_tag(conn: &Connection, name: &str, kind: &str) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO tags (name, kind) VALUES (?1, ?2)",
        params![name, kind],
    )?;
    conn.query_row(
        "SELECT id FROM tags WHERE name = ?1",
        params![name],
        |row| row.get(0),
    )
}

pub fn apply_tags(
    conn: &Connection,
    track_id: i64,
    features: &Features,
    genre: Option<&str>,
) -> Result<(), String> {
    conn.execute("DELETE FROM track_tags WHERE track_id = ?1", params![track_id])
        .map_err(|e| e.to_string())?;

    for label in mood_labels(features) {
        let tag_id = upsert_tag(conn, label, "mood").map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO track_tags (track_id, tag_id, confidence) VALUES (?1, ?2, 1.0)",
            params![track_id, tag_id],
        )
        .map_err(|e| e.to_string())?;
    }

    if let Some(genre) = genre {
        let tag_id = upsert_tag(conn, genre, "genre").map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO track_tags (track_id, tag_id, confidence) VALUES (?1, ?2, 1.0)",
            params![track_id, tag_id],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(())
}
