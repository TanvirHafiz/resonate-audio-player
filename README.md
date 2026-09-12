# Resonate

A local-first desktop audio player built with Tauri (Rust) + vanilla HTML/CSS/JS — not a Winamp clone, more of a "Spotify for local audio": local library playback, internet radio/podcasts, and a from-scratch mood/recommendation engine that runs entirely on-device.

Ships as two tiers from the same codebase, gated by a Cargo feature flag:

- **Free** — full-featured local/streaming player.
- **Pro** — adds the "intelligence" layer (mood tagging, recommendations, affinity tracking) plus playlists, an equalizer, themes, search, and metadata enrichment.

See [plan.md](plan.md) for the original design/build plan and backlog.

## Feature list

### Free tier

- **Local library** — recursive folder scan or drag-and-drop import (MP3, FLAC, OGG, WAV, AAC, M4A), with ID3/Vorbis metadata (title/artist/album/year/duration) read via `lofty`.
- **Playback** — decode/output via `rodio` + `symphonia`; queue with next/previous, play/pause, volume, and a draggable/clickable scrubber for seeking.
- **Streaming** — internet radio (any direct stream URL), podcasts (paste an RSS feed, browse and add individual episodes), and arbitrary audio URLs. Streams download progressively so playback can start before the file finishes fetching.
- **Listening history** — every play/pause/skip is logged (`play_events`), including session grouping and skip position — the raw data the pro tier's affinity engine consumes.
- **Library management** — multi-select (Ctrl/Shift-click), remove tracks or whole folders. Removing a track that's still referenced by a pro-tier playlist only hides it from the library view rather than deleting it outright.
- **"Living Hi-Fi" UI** — frameless, rounded, dark window with a custom titlebar (shows the current track), a mini-player mode you can dock to a screen corner (always-on-top, draggable), and a VU-meter-styled now-playing deck.
- **Free-tier upsell** — a persistent titlebar link opens a popup listing what Pro unlocks.

### Pro tier (everything above, plus)

- **Audio feature extraction** — lightweight time-domain DSP per track (no ML, no FFT): tempo via autocorrelation of the energy envelope, energy/loudness from RMS and peak, brightness from zero-crossing rate, acousticness from crest factor. Runs on the first 90 seconds of a track in a background worker so ingest never blocks; skips anything over 20 minutes (audiobooks/podcasts).
- **Mood & genre tagging** — rule-based mood labels (Energetic, Melancholic, Chill, Driving, Acoustic, Warm, Balanced) derived from the feature vector, shown as badges in the library; genre is read straight from file metadata.
- **Affinity engine** — rebuilds a per-track "how much do you like this" score from listening history, with a 14-day exponential recency decay and a skip penalty.
- **Recommendations** — "More Like This" (cosine similarity over the feature vector + shared tags) and a "For You" queue (recent-listening centroid + affinity + recency penalty + time-of-day match + an exploration slice for discovery).
- **Playlists** — create, rename, delete; add/remove tracks; browse from the sidebar.
- **Settings** — manage library folders (including removal), pick the audio output device, choose an EQ preset, and pick a theme, all from one modal.
- **Equalizer** — a real 3-band (bass/mid/treble) biquad EQ applied live to whatever's playing. Presets: Flat, Rock, Pop, Disco, Soft, Bass Boost, Vocal Boost.
- **Themes** — "Living" (default) drives the ambient glow's color from the currently playing track's mood; four static presets (Noir, Copper, Verdant, Slate) for a fixed look. Choice persists locally.
- **Library search** — text search across title/artist/album, plus a mood-tag filter.
- **Metadata enrichment** — Lyrics (lyrics.ovh), Artist Bio (Wikipedia's free summary API), and a one-click YouTube search for the current track — all keyless, no API tokens required.

## Tech stack

- **Shell**: Tauri 2 (Rust backend + system webview frontend)
- **Audio**: `rodio` (output/mixing) + `symphonia` (decoding, via rodio)
- **Storage**: SQLite via `rusqlite`, WAL mode (so the pro-tier background analysis worker can read/write concurrently with the main connection)
- **Frontend**: plain HTML/CSS/JS, no framework/bundler
- **Networking** (pro): `ureq` for podcast/radio/enrichment HTTP calls

## Development

```bash
npm install
npm run tauri dev              # free tier
npm run tauri dev -- --features pro   # pro tier
```

## Building installers

```bash
# Free tier
npx tauri build --bundles nsis

# Pro tier — distinct product name/identifier so both can be installed side by side
npx tauri build --features pro --bundles nsis --config '{"productName":"Resonate Pro","identifier":"com.steam.resonate.pro"}'
```

Installers land in `src-tauri/target/release/bundle/nsis/`. They're unsigned, so Windows SmartScreen will warn on first run ("More info" → "Run anyway").

## Project structure

```
src-tauri/src/
  api.rs, db.rs, ingest.rs, playback.rs,   # free tier — always compiled
  streaming.rs, history.rs, eq.rs, models.rs
  pro/                                     # pro tier — only compiled with --features pro
    api.rs, feature_extraction.rs, mood_tagging.rs,
    affinity_engine.rs, recommendation_engine.rs,
    playlists.rs, settings.rs, enrichment.rs, worker.rs
src/
  index.html, main.js, styles.css          # single frontend for both tiers;
                                            # UI gates on the `app_tier` command's result
```

## Known limitations / backlog

- Multi-output audio routing (playing to multiple devices at once) is deferred — see plan.md section 8.
- The "Get Resonate Pro" button in the free-tier upsell popup is intentionally disabled — there's no purchase/distribution link yet.
- Installers are unsigned.
