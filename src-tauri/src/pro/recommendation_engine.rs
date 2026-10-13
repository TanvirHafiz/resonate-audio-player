use chrono::{Local, NaiveDateTime, Utc};
use rand::seq::SliceRandom;
use rusqlite::{params, Connection};

const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

#[derive(Clone, Copy)]
struct Vector {
    tempo: f64,
    energy: f64,
    loudness: f64,
    brightness: f64,
    acousticness: f64,
    valence: f64,
}

impl Vector {
    fn as_array(&self) -> [f64; 6] {
        [
            self.tempo,
            self.energy,
            self.loudness,
            self.brightness,
            self.acousticness,
            self.valence,
        ]
    }
}

fn cosine_similarity(a: &Vector, b: &Vector) -> f64 {
    let (a, b) = (a.as_array(), b.as_array());
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let mag_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }
    dot / (mag_a * mag_b)
}

fn fetch_vector(conn: &Connection, track_id: i64) -> Option<Vector> {
    conn.query_row(
        "SELECT tempo_bpm, energy, loudness, brightness, acousticness, valence
         FROM track_features WHERE track_id = ?1",
        params![track_id],
        |row| {
            Ok(Vector {
                tempo: (row.get::<_, f64>(0)? / 200.0).clamp(0.0, 1.0),
                energy: row.get(1)?,
                loudness: row.get(2)?,
                brightness: row.get(3)?,
                acousticness: row.get(4)?,
                valence: row.get(5)?,
            })
        },
    )
    .ok()
}

fn shared_tag_count(conn: &Connection, a: i64, b: i64) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM track_tags ta
         JOIN track_tags tb ON ta.tag_id = tb.tag_id
         WHERE ta.track_id = ?1 AND tb.track_id = ?2",
        params![a, b],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// Seed-track similarity: cosine similarity over the feature vector,
/// boosted a little for each mood/genre tag two tracks share.
pub fn more_like_this(conn: &Connection, seed_track_id: i64, limit: usize) -> Result<Vec<i64>, String> {
    let Some(seed_vec) = fetch_vector(conn, seed_track_id) else {
        return Ok(Vec::new());
    };

    let mut stmt = conn
        .prepare(
            "SELECT tf.track_id FROM track_features tf
             JOIN tracks t ON t.id = tf.track_id
             WHERE tf.track_id != ?1 AND t.in_library = 1",
        )
        .map_err(|e| e.to_string())?;
    let candidate_ids: Vec<i64> = stmt
        .query_map(params![seed_track_id], |row| row.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    let mut scored: Vec<(i64, f64)> = candidate_ids
        .into_iter()
        .filter_map(|id| {
            let vec = fetch_vector(conn, id)?;
            let sim = cosine_similarity(&seed_vec, &vec);
            let tag_bonus = 0.05 * shared_tag_count(conn, seed_track_id, id) as f64;
            Some((id, sim + tag_bonus))
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(scored.into_iter().take(limit).map(|(id, _)| id).collect())
}

/// Habit-weighted "For You" queue: blends similarity to a centroid of
/// recent listening, decayed affinity, a recency penalty (so the same
/// tracks don't repeat constantly), and a time-of-day energy match, plus a
/// slice of lower-affinity tracks for exploration.
pub fn for_you_queue(conn: &Connection, limit: usize) -> Result<Vec<i64>, String> {
    let recent_ids: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT track_id FROM play_events ORDER BY played_at DESC LIMIT 20")
            .map_err(|e| e.to_string())?;
        let result: Vec<i64> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        result
    };

    let recent_vectors: Vec<Vector> = recent_ids.iter().filter_map(|id| fetch_vector(conn, *id)).collect();
    let centroid = centroid_of(&recent_vectors);

    let current_hour = Local::now().format("%H").to_string();
    let hour_avg_energy: Option<f64> = conn
        .query_row(
            "SELECT AVG(tf.energy) FROM play_events pe
             JOIN track_features tf ON tf.track_id = pe.track_id
             WHERE strftime('%H', pe.played_at) = ?1",
            params![current_hour],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    struct Candidate {
        id: i64,
        affinity_score: f64,
        last_played_at: Option<String>,
    }

    let candidates: Vec<Candidate> = {
        let mut stmt = conn
            .prepare(
                "SELECT t.id, COALESCE(a.affinity_score, 0.0), a.last_played_at
                 FROM tracks t
                 JOIN track_features tf ON tf.track_id = t.id
                 LEFT JOIN track_affinity a ON a.track_id = t.id
                 WHERE (t.source_type = 'local' OR t.stream_url LIKE 'https://www.youtube.com/watch?v=%')
                   AND t.in_library = 1",
            )
            .map_err(|e| e.to_string())?;
        let result: Vec<Candidate> = stmt
            .query_map([], |row| {
                Ok(Candidate {
                    id: row.get(0)?,
                    affinity_score: row.get(1)?,
                    last_played_at: row.get(2)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        result
    };

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let max_affinity = candidates
        .iter()
        .map(|c| c.affinity_score)
        .fold(f64::MIN, f64::max)
        .max(0.0001);
    let now = Utc::now().naive_utc();

    let mut scored: Vec<(i64, f64)> = candidates
        .iter()
        .filter_map(|c| {
            let vec = fetch_vector(conn, c.id)?;
            let sim = centroid.map(|cen| cosine_similarity(&cen, &vec)).unwrap_or(0.5);
            let affinity_norm = (c.affinity_score / max_affinity).clamp(-1.0, 1.0);
            let recency_penalty = c
                .last_played_at
                .as_deref()
                .and_then(|s| NaiveDateTime::parse_from_str(s, TIMESTAMP_FORMAT).ok())
                .map(|dt| {
                    let hours_ago = ((now - dt).num_minutes() as f64 / 60.0).max(0.0);
                    (1.0 / (1.0 + hours_ago / 24.0)).clamp(0.0, 1.0)
                })
                .unwrap_or(0.0);
            let time_match = hour_avg_energy
                .map(|avg| 1.0 - (vec.energy - avg).abs())
                .unwrap_or(0.5);

            let score = 0.4 * sim + 0.3 * affinity_norm - 0.2 * recency_penalty + 0.1 * time_match;
            Some((c.id, score))
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let explore_count = ((limit as f64) * 0.12).round() as usize;
    let main_count = limit.saturating_sub(explore_count);

    let mut result: Vec<i64> = scored.iter().take(main_count).map(|(id, _)| *id).collect();

    let mut remainder: Vec<i64> = scored.iter().skip(main_count).map(|(id, _)| *id).collect();
    if explore_count > 0 && !remainder.is_empty() {
        let mut rng = rand::rng();
        remainder.shuffle(&mut rng);
        result.extend(remainder.into_iter().take(explore_count));
    }

    Ok(result)
}

fn centroid_of(vectors: &[Vector]) -> Option<Vector> {
    if vectors.is_empty() {
        return None;
    }
    let n = vectors.len() as f64;
    let sums = vectors.iter().fold([0.0; 6], |mut acc, v| {
        for (a, x) in acc.iter_mut().zip(v.as_array()) {
            *a += x;
        }
        acc
    });
    Some(Vector {
        tempo: sums[0] / n,
        energy: sums[1] / n,
        loudness: sums[2] / n,
        brightness: sums[3] / n,
        acousticness: sums[4] / n,
        valence: sums[5] / n,
    })
}
