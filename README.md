# Resonate

A local-first desktop audio player built with Tauri (Rust) + vanilla HTML/CSS/JS — not a Winamp clone, more of a "Spotify for local audio": local library playback, internet radio/podcasts, and a from-scratch mood/recommendation engine that runs entirely on-device.

Free and open for everyone: the full app, including the "intelligence" layer (mood tagging, recommendations, affinity tracking), playlists, an equalizer, themes, search, and metadata enrichment.

See [plan.md](plan.md) for the original design/build plan and backlog.

## Feature list

### Player

- **Local library** — recursive folder scan or drag-and-drop import (MP3, FLAC, OGG, WAV, AAC, M4A), with ID3/Vorbis metadata (title/artist/album/year/duration) read via `lofty`.
- **Playback** — decode/output via `rodio` + `symphonia`; queue with next/previous, play/pause, volume, and a draggable/clickable scrubber for seeking.
- **Streaming** — internet radio (any direct stream URL), podcasts (paste an RSS feed, browse and add individual episodes), and arbitrary audio URLs. Streams download progressively so playback can start before the file finishes fetching.
- **YouTube (audio only)** — a YouTube tab to search YouTube (or paste a video link) and play the audio without any video. Results play as a queue; the ♡ button saves a track into the library, where it behaves like any other track (queue, history, playlists, a small `YT` badge). Paste a playlist link to import the whole playlist. One-time setup downloads `yt-dlp` and Deno (~60 MB) from GitHub into the app data folder, and yt-dlp self-updates on every launch. Only the AAC audio stream is fetched; fully played tracks are cached (2 GB cap, least recently played evicted first) so replays are instant and work offline. See "YouTube source" below.
- **Listening history** — every play/pause/skip is logged (`play_events`), including session grouping and skip position — the raw data the affinity engine consumes.
- **Library management** — multi-select (Ctrl/Shift-click), remove tracks or whole folders. Removing a track that's still referenced by a playlist only hides it from the library view rather than deleting it outright.
- **"Living Hi-Fi" UI** — frameless, rounded, dark window with a custom titlebar (shows the current track), a mini-player mode you can dock to a screen corner (always-on-top, draggable), and a VU-meter-styled now-playing deck.

### Intelligence & extras

- **Audio feature extraction** — lightweight time-domain DSP per track (no ML, no FFT): tempo via autocorrelation of the energy envelope, energy/loudness from RMS and peak, brightness from zero-crossing rate, acousticness from crest factor. Runs on the first 90 seconds of a track in a background worker so ingest never blocks; skips anything over 20 minutes (audiobooks/podcasts).
- **Mood & genre tagging** — rule-based mood labels (Energetic, Melancholic, Chill, Driving, Acoustic, Warm, Balanced) derived from the feature vector, shown as badges in the library; genre is read straight from file metadata.
- **Affinity engine** — rebuilds a per-track "how much do you like this" score from listening history, with a 14-day exponential recency decay and a skip penalty.
- **YouTube in the intelligence layer** — cached YouTube tracks get the same feature extraction and mood tags as local files, and saved YouTube tracks are candidates for "For You" and "More Like This". Importing a YouTube playlist also creates a matching Resonate playlist.
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
- **Storage**: SQLite via `rusqlite`, WAL mode (so the background analysis worker can read/write concurrently with the main connection)
- **Frontend**: plain HTML/CSS/JS, no framework/bundler
- **Networking**: `ureq` for podcast/radio/enrichment HTTP calls

## Development

```bash
npm install
npm run tauri dev
```

## Building an installer

```bash
npx tauri build --bundles nsis
```

The installer lands in `src-tauri/target/release/bundle/nsis/`. It's unsigned, so Windows SmartScreen will warn on first run ("More info" → "Run anyway").

## Project structure

```
src-tauri/src/
  api.rs, db.rs, ingest.rs, playback.rs,   # core player
  streaming.rs, history.rs, eq.rs, models.rs,
  youtube.rs                               # YouTube search/playback via yt-dlp
  pro/                                     # intelligence layer — on by default (the `pro` Cargo feature)
    api.rs, feature_extraction.rs, mood_tagging.rs,
    affinity_engine.rs, recommendation_engine.rs,
    playlists.rs, settings.rs, enrichment.rs, worker.rs
src/
  index.html, main.js, styles.css          # single frontend;
                                            # UI gates on the `app_tier` command's result
```

## YouTube source

How it fits together (details in the `youtube.rs` header comment):

- **Tools**: `yt-dlp.exe` + `deno.exe` live in `%APPDATA%\com.steam.resonate\tools\`. Nothing is bundled into the installer. If YouTube playback breaks, yt-dlp needs an update; restarting the app triggers one.
- **Storage**: YouTube tracks are ordinary `tracks` rows with `source_type = 'url'` and `stream_url = https://www.youtube.com/watch?v=<id>`; no schema migration. Search results that were played but not saved are kept as hidden rows (`in_library = 0`) so their listening history counts.
- **Playback**: yt-dlp pipes the AAC (format 140) stream into the same progressive buffer used for radio/podcasts. Loading happens on a background thread (`playback::play_async`) so the UI never blocks; `get_now_playing` reports `loading` and a one-shot `error`.
- **Cache**: `%APPDATA%\...\youtube_cache\<id>.m4a`, written once a track has fully downloaded.
- **Terms of use**: streaming audio this way is against YouTube's Terms of Service. Fine for personal/open-source use; don't ship it in a store listing or a paid product as-is.

## Known limitations / backlog

- Multi-output audio routing (playing to multiple devices at once) is deferred — see plan.md section 8.
- Installers are unsigned.
- YouTube: no account sign-in (age-restricted or members-only videos won't play), and live streams are filtered out of search.
