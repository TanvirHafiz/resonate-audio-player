use chrono::{NaiveDateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;

const HALF_LIFE_DAYS: f64 = 14.0;
const SKIP_PENALTY: f64 = 0.35;
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

struct EventRow {
    track_id: i64,
    played_seconds: f64,
    skipped: bool,
    played_at: String,
    duration: Option<f64>,
}

struct Agg {
    play_count: i64,
    skip_count: i64,
    completion_sum: f64,
    completion_n: i64,
    last_played_at: Option<String>,
    score: f64,
}

/// Rebuilds `track_affinity` from `play_events` using an exponential
/// recency decay (14-day half-life) so recent listening matters more than
/// old plays, and a flat penalty for skips. Called after every play event
/// records, and once at app startup — cheap enough at this scale to just
/// recompute in full rather than maintain incremental state.
pub fn recompute(conn: &Connection) -> Result<(), String> {
    let now = Utc::now().naive_utc();

    let mut stmt = conn
        .prepare(
            "SELECT pe.track_id, pe.played_seconds, pe.skipped, pe.played_at, t.duration_seconds
             FROM play_events pe
             JOIN tracks t ON t.id = pe.track_id",
        )
        .map_err(|e| e.to_string())?;

    let rows: Vec<EventRow> = stmt
        .query_map([], |row| {
            Ok(EventRow {
                track_id: row.get(0)?,
                played_seconds: row.get(1)?,
                skipped: row.get(2)?,
                played_at: row.get(3)?,
                duration: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    let mut agg: HashMap<i64, Agg> = HashMap::new();

    for row in &rows {
        let entry = agg.entry(row.track_id).or_insert(Agg {
            play_count: 0,
            skip_count: 0,
            completion_sum: 0.0,
            completion_n: 0,
            last_played_at: None,
            score: 0.0,
        });

        entry.play_count += 1;
        if row.skipped {
            entry.skip_count += 1;
        }

        let completion = row
            .duration
            .filter(|d| *d > 0.0)
            .map(|d| (row.played_seconds / d).clamp(0.0, 1.0));

        if let Some(c) = completion {
            entry.completion_sum += c;
            entry.completion_n += 1;
        }

        if entry
            .last_played_at
            .as_deref()
            .is_none_or(|last| row.played_at.as_str() > last)
        {
            entry.last_played_at = Some(row.played_at.clone());
        }

        let days_ago = NaiveDateTime::parse_from_str(&row.played_at, TIMESTAMP_FORMAT)
            .map(|dt| (now - dt).num_minutes() as f64 / 1440.0)
            .unwrap_or(0.0)
            .max(0.0);
        let decay = 0.5f64.powf(days_ago / HALF_LIFE_DAYS);
        let event_value = if row.skipped {
            -SKIP_PENALTY
        } else {
            completion.unwrap_or(0.5)
        };
        entry.score += decay * event_value;
    }

    conn.execute("DELETE FROM track_affinity", [])
        .map_err(|e| e.to_string())?;

    for (track_id, a) in agg {
        let completion_rate = if a.completion_n > 0 {
            a.completion_sum / a.completion_n as f64
        } else {
            0.0
        };
        conn.execute(
            "INSERT INTO track_affinity
                (track_id, play_count, skip_count, completion_rate, last_played_at, affinity_score)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                track_id,
                a.play_count,
                a.skip_count,
                completion_rate,
                a.last_played_at,
                a.score
            ],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(())
}
