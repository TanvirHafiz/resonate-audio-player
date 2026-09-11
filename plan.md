# Local Audio Player — Build Plan (v1)

## 1. Project Summary

A modern desktop audio player for Windows, built with Tauri (Rust backend + web frontend). Not a Winamp clone — a fresh "Spotify for local audio" concept:

- Unified playback queue across local files and open streaming sources (internet radio, podcasts, user-added URLs — no licensed catalog streaming)
- Local audio-feature extraction + mood/genre tagging at ingest time
- Personal recommendation engine based on content similarity + implicit listening-habit signals (no collaborative filtering — single user, local-first)
- Two recommendation modes: "More like this" (seed-track similarity) and "For you" queue (habit-weighted with exploration)

Explicitly out of scope for v1: AI DJ/beatmatching/crossfade, bitmap skin engine, third-party plugin system, licensed streaming catalogs, collaborative filtering.

## 2. Tech Stack

- **Shell/runtime**: Tauri (Rust core + webview frontend)
- **Audio playback/decoding**: `rodio` or `cpal` for output, `symphonia` for decoding (MP3/FLAC/OGG/WAV/AAC)
- **Storage**: SQLite (via `rusqlite`), local file, no external DB server
- **Feature extraction**: batch Rust module at ingest time (see Module 3); mood/genre classification via a lightweight pretrained audio-tagging model run locally, not the LLM
- **Frontend**: HTML/CSS/JS (or a lightweight framework) inside the Tauri webview

## 3. Database Schema (SQLite)

```sql
-- Core track table
CREATE TABLE tracks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path TEXT UNIQUE,            -- NULL for stream-only entries
    stream_url TEXT,                  -- NULL for local files
    title TEXT,
    artist TEXT,
    album TEXT,
    year INTEGER,
    duration_seconds REAL,
    date_added TEXT DEFAULT CURRENT_TIMESTAMP,
    source_type TEXT CHECK(source_type IN ('local','radio','podcast','url')) NOT NULL
);

-- Audio-derived feature vector (one row per track, populated at ingest)
CREATE TABLE track_features (
    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    tempo_bpm REAL,
    energy REAL,             -- normalized 0-1
    loudness REAL,           -- normalized 0-1
    brightness REAL,         -- spectral centroid, normalized 0-1
    acousticness REAL,       -- percussive/harmonic ratio, normalized 0-1
    valence REAL,            -- estimated mood positivity, 0-1
    extracted_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- Mood/genre tags (many-to-many; a track can have multiple tags with confidence)
CREATE TABLE tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT UNIQUE NOT NULL,
    kind TEXT CHECK(kind IN ('mood','genre')) NOT NULL
);

CREATE TABLE track_tags (
    track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    tag_id INTEGER REFERENCES tags(id) ON DELETE CASCADE,
    confidence REAL DEFAULT 1.0,
    PRIMARY KEY (track_id, tag_id)
);

-- Implicit listening history (one row per play event)
CREATE TABLE play_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    played_at TEXT DEFAULT CURRENT_TIMESTAMP,
    played_seconds REAL,          -- how long they listened
    skipped BOOLEAN DEFAULT 0,
    skip_point_seconds REAL,      -- NULL if not skipped
    session_id INTEGER            -- groups plays within one listening session
);

-- Aggregated affinity per track, recomputed periodically (materialized, not live-queried each time)
CREATE TABLE track_affinity (
    track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    play_count INTEGER DEFAULT 0,
    skip_count INTEGER DEFAULT 0,
    completion_rate REAL DEFAULT 0,   -- avg played_seconds / duration
    last_played_at TEXT,
    affinity_score REAL DEFAULT 0,    -- decayed composite score, recomputed on a schedule
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- Playlists (user-created, ordinary)
CREATE TABLE playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE playlist_tracks (
    playlist_id INTEGER REFERENCES playlists(id) ON DELETE CASCADE,
    track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
    position INTEGER,
    PRIMARY KEY (playlist_id, track_id)
);

CREATE INDEX idx_play_events_track ON play_events(track_id);
CREATE INDEX idx_play_events_played_at ON play_events(played_at);
CREATE INDEX idx_track_tags_tag ON track_tags(tag_id);
```

## 4. Module Breakdown (Rust/Tauri backend)

1. **`db` module** — SQLite connection pool, migrations, typed query layer (CRUD for tracks, features, tags, play_events, affinity, playlists).

2. **`ingest` module** — library scan (walk directories), metadata parsing (ID3/Vorbis comments via `lofty` or `symphonia`'s metadata reader), dedup by file path, inserts into `tracks`.

3. **`feature_extraction` module** — runs once per new track: computes tempo/energy/loudness/brightness/acousticness (via a Rust DSP crate or an FFI/subprocess call to a Python `librosa`-based helper if needed for v1 speed), writes to `track_features`.

4. **`mood_tagging` module** — loads a lightweight pretrained audio-tagging model (e.g., ONNX-exported), runs inference on extracted audio, writes results to `tags`/`track_tags`. Falls back to metadata-based inference (genre from ID3, or LLM-assisted inference from artist/album text) only when audio tagging is inconclusive.

5. **`playback` module** — decode (`symphonia`) + output (`rodio`/`cpal`), queue management, gapless transitions, multi-output routing (simultaneous speaker/monitor/Bluetooth output).

6. **`streaming` module** — internet radio stream handling, podcast RSS parsing, arbitrary URL playback. Explicitly not licensed catalog integration.

7. **`history_tracker` module** — listens to playback events (play start/stop/skip), writes `play_events` rows in real time.

8. **`affinity_engine` module** — scheduled job (e.g., on app start + periodically) that recomputes `track_affinity` from `play_events` using a decay-weighted formula.

9. **`recommendation_engine` module** — implements the two modes:
   - `more_like_this(seed_track_id) -> Vec<track_id>`: cosine similarity over `track_features` vectors, filtered/boosted by shared tags.
   - `for_you_queue(limit) -> Vec<track_id>`: blends `track_affinity.affinity_score`, similarity to recent listening centroid, recency penalty, time-of-day context match, plus a configurable exploration slice (10-15% low-affinity tracks).

10. **`api` module** — Tauri commands exposed to the frontend (scan library, get queue, play/pause/skip, get recommendations, rate/skip feedback, manage playlists).

11. **`frontend`** — playback UI, library browser, queue view, "For You" and "More Like This" surfaces, minimal settings (audio output routing, library folders).

## 5. Recommendation Scoring (reference formula)

```
score(T) = w1 * cosine_similarity(T.feature_vector, seed.feature_vector)
         + w2 * track_affinity.affinity_score
         - w3 * recency_penalty(T)
         + w4 * time_of_day_context_match(T)
```
Starting weights: w1=0.4, w2=0.3, w3=0.2, w4=0.1 — tune by ear during dogfooding, not by formal optimization in v1.

## 6. Build Phases

**Phase 1 — Core playback**
- `db` schema + migrations
- `ingest` (local file scan + metadata)
- `playback` (local file decode/output, basic queue)
- Minimal frontend: library list, play/pause/skip, now-playing

**Phase 2 — Habit tracking + streaming**
- `history_tracker` + `play_events` logging
- `streaming` module (radio/podcast/URL)
- Multi-output audio routing

**Phase 3 — Intelligence layer**
- `feature_extraction` + `mood_tagging`
- `affinity_engine`
- `recommendation_engine` (both modes)
- Frontend: "More Like This," "For You" queue

**Phase 4 — Polish**
- Playlists
- Settings (library folders, output devices)
- Performance pass on large libraries (50k+ tracks)

## 7. UI / Design Direction — "Living Hi-Fi"

**Concept**: The interface is not a static shell around a playlist — it visibly reacts to the mood/energy data the recommendation engine already computes. This is the core differentiator: design and data model are the same system, not decoration bolted on top.

**Aesthetic**: Dark, warm, premium hi-fi component — closer to a boutique amplifier than a SaaS dashboard. Rejects both Spotify's flat corporate black/green and Winamp's dense skeuomorphic clutter.

- **Color**: Near-black base (`#0e0c0a`). No fixed accent — the ambient glow behind the now-playing view shifts hue/intensity based on the current track's `valence`/`energy`/mood-tag values (warm amber/coral for upbeat, deep violet/blue for dark or calm tracks). This mapping is literally driven by the `track_features` and `track_tags` data.
- **Typography**: A serif display face (e.g. Fraunces) for track titles — warm, editorial, not generic-app — paired with a monospace face (e.g. Space Mono) for metadata/labels/timers, evoking hi-fi LED displays, plus a clean sans for body text. No default system fonts.
- **Motion**: A subtle "breathing" ambient glow (not strobing), a VU-meter-style bar visualizer reacting to playback, and smooth color-morph transitions between tracks rather than hard cuts.
- **Layout**: Centered now-playing "deck" (art, title, VU visualizer, scrubber, transport controls) with the "For You" queue as a scannable list below it, each item mood-tagged with a small badge — the recommendation engine's output made visible and legible, not hidden behind a black box.
- **What's deliberately avoided**: skeuomorphic knobs/sliders, dense right-click menus, generic purple-gradient SaaS look, default system fonts, static single-color theming.

A visual mockup (`design-mockup.html`) demonstrates this direction: mood-reactive ambient glow, serif/mono type pairing, VU visualizer, "For You" queue with mood badges, a utility control strip, and a theme picker. Frontend implementation should treat mood-reactive theming as a first-class CSS variable system (e.g. `--mood-a`/`--mood-b` driven by the currently playing track's feature vector), not a one-off static gradient.

### 7.1 Control surface (utility strip)

Sits below the now-playing deck, above the queue:
- **Volume** — inline slider, always visible (not buried in a menu)
- **Output source selector** — click to expand a panel listing available audio devices (Bluetooth, desktop speakers, analog line-out, etc.); supports multi-select for simultaneous multi-output routing, which maps directly to the `playback` module's multi-output requirement from section 4
- **EQ** — opens a dedicated equalizer view/panel (band sliders + presets); not designed in this mockup pass but reserved as a first-class control, not a hidden menu item

### 7.2 Theme system

Mood-reactive theming is the default ("Living" theme) but not mandatory — some users want one consistent look rather than a shifting one. Ship a small set of presets alongside it:
- **Living** (default) — dynamic, mood-reactive per track, as described above
- **Noir** — static near-monochrome dark theme, minimal color
- **Copper** — static warm amber/copper accent, no reactive shifting
- **Verdant** — static cool green accent
- **Slate** — static cool blue accent

All presets share the same layout, typography, and motion language — only the accent color source (dynamic vs. fixed) changes. Theme selection lives as a simple swatch row in settings/now-playing view, not buried in a preferences dialog.

## 8. Open Decisions for Claude Code to Flag, Not Assume

- Which pretrained audio-tagging model to bundle/use for mood/genre (licensing, size, ONNX runtime availability on Windows)
- Whether feature extraction runs synchronously on ingest or as a background queue for large libraries
- Exact multi-output audio routing approach on Windows (WASAPI shared mode fan-out vs. per-device streams)

## 9. Backlog (requested, not yet built)

- **Library search bar** — basic substring search over title/artist/album, in addition to filtering by mood tag (pro tier, since mood tags only exist there)
- **Lyrics fetcher, YouTube link, artist bio** buttons on the now-playing view (external metadata lookups; not tied to a specific phase — build after Phase 3 core intelligence work)
- Multi-output audio routing (deferred from Phase 2 — see section 8)
- **App icon** — currently the default Tauri scaffold icon; needs real branded icon art (window/taskbar icon, installer icon) matching the "Living Hi-Fi" aesthetic
- **Free-tier upsell in titlebar** — persistent "Free mode · click here for full version features" text in the titlebar (free build only), opening a popup listing pro-tier features plus a link to get the full version (link destination TBD)
