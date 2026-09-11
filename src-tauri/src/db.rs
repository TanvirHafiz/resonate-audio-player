use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

pub struct Db(pub Mutex<Connection>);

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tracks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path TEXT UNIQUE,
    stream_url TEXT UNIQUE,
    title TEXT,
    artist TEXT,
    album TEXT,
    year INTEGER,
    duration_seconds REAL,
    date_added TEXT DEFAULT CURRENT_TIMESTAMP,
    source_type TEXT CHECK(source_type IN ('local','radio','podcast','url')) NOT NULL
);

CREATE TABLE IF NOT EXISTS track_features (
    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    tempo_bpm REAL,
    energy REAL,
    loudness REAL,
    brightness REAL,
    acousticness REAL,
    valence REAL,
    extracted_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT UNIQUE NOT NULL,
    kind TEXT CHECK(kind IN ('mood','genre')) NOT NULL
);

CREATE TABLE IF NOT EXISTS track_tags (
    track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    tag_id INTEGER REFERENCES tags(id) ON DELETE CASCADE,
    confidence REAL DEFAULT 1.0,
    PRIMARY KEY (track_id, tag_id)
);

CREATE TABLE IF NOT EXISTS play_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    played_at TEXT DEFAULT CURRENT_TIMESTAMP,
    played_seconds REAL,
    skipped BOOLEAN DEFAULT 0,
    skip_point_seconds REAL,
    session_id INTEGER
);

CREATE TABLE IF NOT EXISTS track_affinity (
    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    play_count INTEGER DEFAULT 0,
    skip_count INTEGER DEFAULT 0,
    completion_rate REAL DEFAULT 0,
    last_played_at TEXT,
    affinity_score REAL DEFAULT 0,
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS playlist_tracks (
    playlist_id INTEGER REFERENCES playlists(id) ON DELETE CASCADE,
    track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    position INTEGER,
    PRIMARY KEY (playlist_id, track_id)
);

CREATE TABLE IF NOT EXISTS library_folders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    path TEXT UNIQUE NOT NULL,
    added_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_play_events_track ON play_events(track_id);
CREATE INDEX IF NOT EXISTS idx_play_events_played_at ON play_events(played_at);
CREATE INDEX IF NOT EXISTS idx_track_tags_tag ON track_tags(tag_id);
"#;

pub fn init(app_data_dir: &Path) -> Connection {
    std::fs::create_dir_all(app_data_dir).expect("failed to create app data dir");
    let db_path = app_data_dir.join("resonate.db");
    let conn = Connection::open(db_path).expect("failed to open database");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("failed to enable foreign keys");
    conn.execute_batch(SCHEMA).expect("failed to run migrations");
    conn
}
