//! YouTube as an audio-only streaming source.
//!
//! Search, playlist import and audio extraction all go through `yt-dlp`,
//! which needs a JavaScript runtime (Deno) to solve YouTube's player
//! challenges. Neither is bundled with the installer: on first use the app
//! downloads both into `<app data>/tools/` (see `setup`), and refreshes
//! yt-dlp in the background on every launch (YouTube changes often and old
//! yt-dlp builds stop working).
//!
//! Only the AAC audio stream (format 140, ~128 kbps M4A) is ever fetched —
//! never video — because the existing rodio/symphonia pipeline decodes AAC
//! in MP4 out of the box. Audio is piped straight out of yt-dlp into the
//! same progressive buffer the radio/podcast streams use, and once a track
//! has fully downloaded it's kept in `<app data>/youtube_cache/` so replays
//! start instantly, work offline, and can be analyzed by the pro tier's
//! feature extractor like any local file.
//!
//! YouTube tracks are stored as ordinary `tracks` rows with
//! `source_type = 'url'` and `stream_url = https://www.youtube.com/watch?v=ID`
//! (the table's CHECK constraint predates this feature, and a URL-based
//! marker avoids a risky table rebuild); `video_id_from_url` is how the rest
//! of the app recognizes them.

use crate::playback::{self, LoadedReader, Player};
use crate::streaming::{OnComplete, StreamReader};
use once_cell::sync::OnceCell;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::SystemTime;
use tauri::{AppHandle, Emitter};

/// `stream_url` prefix every YouTube track is stored with.
pub const URL_PREFIX: &str = "https://www.youtube.com/watch?v=";

/// Cached audio beyond this is evicted, least recently played first.
const CACHE_LIMIT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

static APP_DATA_DIR: OnceCell<PathBuf> = OnceCell::new();
type CachedHook = Box<dyn Fn(&str) + Send + Sync>;
static ON_CACHED: OnceCell<CachedHook> = OnceCell::new();
static SETUP_LOCK: Mutex<()> = Mutex::new(());

// ---------- Paths ----------

#[cfg(windows)]
mod platform {
    pub const YTDLP_NAME: &str = "yt-dlp.exe";
    pub const DENO_NAME: &str = "deno.exe";
    pub const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
    pub const DENO_URL: &str =
        "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip";
}

#[cfg(target_os = "macos")]
mod platform {
    pub const YTDLP_NAME: &str = "yt-dlp";
    pub const DENO_NAME: &str = "deno";
    pub const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos";
    pub const DENO_URL: &str =
        "https://github.com/denoland/deno/releases/latest/download/deno-aarch64-apple-darwin.zip";
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    pub const YTDLP_NAME: &str = "yt-dlp";
    pub const DENO_NAME: &str = "deno";
    pub const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux";
    pub const DENO_URL: &str =
        "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip";
}

fn app_data_dir() -> Result<&'static PathBuf, String> {
    APP_DATA_DIR
        .get()
        .ok_or_else(|| "YouTube module not initialized".to_string())
}

fn tools_dir() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("tools"))
}

fn ytdlp_path() -> Result<PathBuf, String> {
    Ok(tools_dir()?.join(platform::YTDLP_NAME))
}

fn deno_path() -> Result<PathBuf, String> {
    Ok(tools_dir()?.join(platform::DENO_NAME))
}

fn cache_dir() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("youtube_cache"))
}

fn cached_file(video_id: &str) -> Result<PathBuf, String> {
    Ok(cache_dir()?.join(format!("{video_id}.m4a")))
}

/// Path of the fully-downloaded audio for `video_id`, if it's cached.
pub fn cached_path(video_id: &str) -> Option<PathBuf> {
    cached_file(video_id).ok().filter(|p| p.is_file())
}

/// Called once at startup. Records where tools/cache live and, if the
/// tools are already installed, refreshes yt-dlp in the background.
pub fn init(app_data_dir: &Path) {
    let _ = APP_DATA_DIR.set(app_data_dir.to_path_buf());
    if is_ready() {
        thread::spawn(|| {
            if let Err(e) = update_ytdlp() {
                eprintln!("[youtube] yt-dlp update check failed: {e}");
            }
        });
    }
}

/// Registers a hook that runs whenever a track finishes downloading into
/// the cache (the pro tier uses it to queue the file for audio analysis).
#[cfg_attr(not(feature = "pro"), allow(dead_code))]
pub fn set_on_cached(hook: impl Fn(&str) + Send + Sync + 'static) {
    let _ = ON_CACHED.set(Box::new(hook));
}

pub fn is_ready() -> bool {
    matches!((ytdlp_path(), deno_path()), (Ok(y), Ok(d)) if y.is_file() && d.is_file())
}

// ---------- URLs ----------

pub fn video_url(video_id: &str) -> String {
    format!("{URL_PREFIX}{video_id}")
}

fn is_valid_video_id(id: &str) -> bool {
    id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Extracts the video id from any common YouTube / YouTube Music video URL
/// (`watch?v=`, `youtu.be/`, `/shorts/`), or `None` if `url` isn't one.
pub fn video_id_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let host = host.trim_start_matches("www.").trim_start_matches("m.");

    let candidate = match host {
        "youtube.com" | "music.youtube.com" => {
            if let Some(shorts) = path.strip_prefix("shorts/") {
                shorts.split(['?', '&', '/', '#']).next()?.to_string()
            } else {
                let query = path.strip_prefix("watch?")?;
                query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("v="))?
                    .split('#')
                    .next()?
                    .to_string()
            }
        }
        "youtu.be" => path.split(['?', '&', '/', '#']).next()?.to_string(),
        _ => return None,
    };
    is_valid_video_id(&candidate).then_some(candidate)
}

// ---------- Running yt-dlp ----------

/// A `Command` that won't flash a console window on Windows.
fn hidden_command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn ytdlp_command() -> Result<Command, String> {
    if !is_ready() {
        return Err("YouTube isn't set up yet. Open the YouTube tab to set it up.".into());
    }
    let mut cmd = hidden_command(&ytdlp_path()?);
    cmd.arg("--ignore-config")
        .arg("--no-warnings")
        .arg("--no-progress")
        .arg("--js-runtimes")
        .arg(format!("deno:{}", deno_path()?.display()))
        .arg("--cache-dir")
        .arg(tools_dir()?.join("cache"))
        .stdin(Stdio::null());
    Ok(cmd)
}

/// Turns yt-dlp's stderr into a short user-facing message.
fn friendly_error(stderr: &str) -> String {
    let line = stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("ERROR:"))
        .or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("yt-dlp failed")
        .trim_start_matches("ERROR:")
        .trim();
    let lower = line.to_lowercase();
    if lower.contains("requested format is not available") {
        "This video has no standard audio stream Resonate can play.".into()
    } else if lower.contains("sign in to confirm") {
        "YouTube is asking for a bot check. Wait a little and try again.".into()
    } else if lower.contains("private video") || lower.contains("video unavailable") {
        "This video is unavailable.".into()
    } else if lower.contains("unable to download") || lower.contains("getaddrinfo") || lower.contains("timed out") {
        "Couldn't reach YouTube. Check your connection.".into()
    } else {
        line.to_string()
    }
}

/// Runs yt-dlp with `args` and parses its `-J` JSON output. Also returns
/// stderr: some failures (e.g. a search that couldn't reach YouTube) still
/// exit successfully with an empty result list.
fn run_json(args: &[&str]) -> Result<(Value, String), String> {
    let output = ytdlp_command()?
        .args(args)
        .output()
        .map_err(|e| format!("couldn't run yt-dlp: {e}"))?;
    if !output.status.success() {
        return Err(friendly_error(&String::from_utf8_lossy(&output.stderr)));
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let json = serde_json::from_slice(&output.stdout).map_err(|e| format!("unexpected yt-dlp output: {e}"))?;
    Ok((json, stderr))
}

/// An empty result list plus an error on stderr means the lookup failed,
/// not that nothing matched.
fn fail_if_empty_with_error(results: &[YoutubeResult], stderr: &str) -> Result<(), String> {
    if results.is_empty() && stderr.contains("ERROR:") {
        Err(friendly_error(stderr))
    } else {
        Ok(())
    }
}

// ---------- Search & metadata ----------

/// One playable YouTube result, as shown in the YouTube tab.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct YoutubeResult {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub duration_seconds: f64,
    pub thumbnail: String,
    /// Filled in by the API layer: the matching `tracks` row, if any.
    #[serde(default)]
    pub track_id: Option<i64>,
    /// Filled in by the API layer: whether it's saved in the library.
    #[serde(default)]
    pub in_library: bool,
}

/// Removes "(Official Video)", "[Lyrics]", "(HD)" and similar noise from a
/// video title, keeping meaningful brackets like "(Live)" or "(Remix)".
fn strip_noise_brackets(title: &str) -> String {
    const NOISE: &[&str] = &[
        "official", "video", "audio", "lyric", "visuali", "hd", "4k", "mv", "m/v", "full song", "hq",
    ];
    let mut out = String::with_capacity(title.len());
    let mut chars = title.chars().peekable();
    while let Some(c) = chars.next() {
        let close = match c {
            '(' => ')',
            '[' => ']',
            _ => {
                out.push(c);
                continue;
            }
        };
        let mut inner = String::new();
        let mut closed = false;
        for nc in chars.by_ref() {
            if nc == close {
                closed = true;
                break;
            }
            inner.push(nc);
        }
        let lower = inner.to_lowercase();
        let is_noise = lower
            .split(|ch: char| !ch.is_alphanumeric() && ch != '/')
            .any(|word| NOISE.iter().any(|n| word.starts_with(n)))
            || NOISE.iter().any(|n| n.contains(' ') && lower.contains(n));
        if !(closed && is_noise) {
            out.push(c);
            out.push_str(&inner);
            if closed {
                out.push(close);
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_channel(channel: &str) -> String {
    let c = channel.trim();
    let c = c.strip_suffix(" - Topic").unwrap_or(c);
    let c = c.strip_suffix("VEVO").unwrap_or(c);
    c.trim().to_string()
}

/// Best-effort "Artist - Title" split. Music uploads overwhelmingly use
/// that convention; otherwise the channel name stands in for the artist
/// (which is exactly right for YouTube Music's auto-generated
/// "Artist - Topic" channels).
fn split_artist_title(raw_title: &str, channel: &str) -> (String, String) {
    let title = strip_noise_brackets(raw_title);
    for sep in [" - ", " – ", " — "] {
        if let Some((artist, song)) = title.split_once(sep) {
            let (artist, song) = (artist.trim(), song.trim());
            if !artist.is_empty() && !song.is_empty() {
                return (artist.to_string(), song.to_string());
            }
        }
    }
    (clean_channel(channel), title)
}

fn result_from_entry(entry: &Value) -> Option<YoutubeResult> {
    let video_id = entry.get("id")?.as_str()?.to_string();
    if !is_valid_video_id(&video_id) {
        return None; // channels/playlists mixed into search results
    }
    let raw_title = entry.get("title")?.as_str()?;
    if raw_title.starts_with("[Private") || raw_title.starts_with("[Deleted") {
        return None;
    }
    // Live streams have no duration and can't be fetched as a single file.
    let duration_seconds = entry.get("duration").and_then(Value::as_f64)?;
    if entry.get("live_status").and_then(Value::as_str) == Some("is_live") {
        return None;
    }
    let channel = ["channel", "uploader", "artist"]
        .iter()
        .find_map(|k| entry.get(*k).and_then(Value::as_str))
        .unwrap_or("");
    let (artist, title) = split_artist_title(raw_title, channel);
    Some(YoutubeResult {
        thumbnail: format!("https://i.ytimg.com/vi/{video_id}/mqdefault.jpg"),
        video_id,
        title,
        artist,
        duration_seconds,
        track_id: None,
        in_library: false,
    })
}

fn results_from_json(json: &Value) -> Vec<YoutubeResult> {
    match json.get("entries").and_then(Value::as_array) {
        Some(entries) => entries.iter().filter_map(result_from_entry).collect(),
        None => result_from_entry(json).into_iter().collect(),
    }
}

/// Searches YouTube for `query`. A pasted video URL returns just that video.
pub fn search(query: &str, limit: usize) -> Result<Vec<YoutubeResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let (json, stderr) = if let Some(id) = video_id_from_url(query) {
        run_json(&["-J", "--flat-playlist", "--no-playlist", &video_url(&id)])?
    } else {
        run_json(&["-J", "--flat-playlist", &format!("ytsearch{limit}:{query}")])?
    };
    let results = results_from_json(&json);
    fail_if_empty_with_error(&results, &stderr)?;
    Ok(results)
}

/// Fetches a playlist's title and its playable entries. Returns the
/// number of entries that had to be skipped (private/deleted/live).
pub fn fetch_playlist(url: &str) -> Result<(String, Vec<YoutubeResult>, usize), String> {
    let (json, stderr) = run_json(&["-J", "--flat-playlist", "--yes-playlist", url.trim()])?;
    let title = json
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("YouTube playlist")
        .to_string();
    let total = json
        .get("entries")
        .and_then(Value::as_array)
        .map(|e| e.len())
        .unwrap_or(0);
    let results = results_from_json(&json);
    fail_if_empty_with_error(&results, &stderr)?;
    let skipped = total.saturating_sub(results.len());
    Ok((title, results, skipped))
}

// ---------- Library rows ----------

/// Finds or creates the `tracks` row for a YouTube result and returns its
/// id. `save` puts it in the library; otherwise a new row is created
/// hidden (`in_library = 0`) — still playable, queueable and tracked in
/// listening history, just not listed until saved. Never un-saves.
pub fn upsert_track(conn: &Connection, r: &YoutubeResult, save: bool) -> Result<i64, String> {
    let url = video_url(&r.video_id);
    if let Ok((id, in_library)) = conn.query_row(
        "SELECT id, in_library FROM tracks WHERE stream_url = ?1",
        params![url],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    ) {
        if save && in_library == 0 {
            conn.execute("UPDATE tracks SET in_library = 1 WHERE id = ?1", params![id])
                .map_err(|e| e.to_string())?;
        }
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO tracks (stream_url, title, artist, album, duration_seconds, source_type, in_library)
         VALUES (?1, ?2, ?3, 'YouTube', ?4, 'url', ?5)",
        params![url, r.title, r.artist, r.duration_seconds, save as i64],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

/// Fills in `track_id` / `in_library` on results that already have rows.
pub fn annotate(conn: &Connection, results: &mut [YoutubeResult]) {
    for r in results.iter_mut() {
        if let Ok((id, in_library)) = conn.query_row(
            "SELECT id, in_library FROM tracks WHERE stream_url = ?1",
            params![video_url(&r.video_id)],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        ) {
            r.track_id = Some(id);
            r.in_library = in_library == 1;
        }
    }
}

// ---------- Playback ----------

/// Plays a YouTube video's audio: instantly from cache if it's been played
/// to the end before, otherwise streamed through yt-dlp in the background
/// (the player reports `loading` until audio starts).
pub fn play(player: &Player, video_id: &str) -> Result<(), String> {
    if let Some(path) = cached_path(video_id) {
        touch(&path);
        return playback::play_file(player, &path.to_string_lossy());
    }
    // Fail fast (synchronously) if the tools aren't installed.
    ytdlp_command()?;
    let id = video_id.to_string();
    playback::play_async(player, move || open_stream(&id));
    Ok(())
}

/// yt-dlp's stdout as a `Read`, which turns a failed run (non-zero exit)
/// into a read error carrying yt-dlp's message instead of a silent EOF.
struct ChildAudio {
    child: Child,
    stdout: ChildStdout,
    stderr: Option<JoinHandle<String>>,
}

impl Read for ChildAudio {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.stdout.read(buf)?;
        if n == 0 {
            let status = self.child.wait()?;
            let stderr = self.stderr.take().and_then(|h| h.join().ok()).unwrap_or_default();
            if !status.success() {
                return Err(std::io::Error::other(friendly_error(&stderr)));
            }
        }
        Ok(n)
    }
}

impl Drop for ChildAudio {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn drain_stderr(mut stderr: ChildStderr) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    })
}

fn open_stream(video_id: &str) -> Result<LoadedReader, String> {
    let mut child = ytdlp_command()?
        // 140 = AAC 128 kbps in M4A, the one audio format nearly every
        // video has and that symphonia can decode (Opus/WebM it can't).
        .args(["-f", "140/bestaudio[ext=m4a]", "--no-playlist", "--no-part", "--quiet", "-o", "-"])
        .arg(video_url(video_id))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run yt-dlp: {e}"))?;

    let stdout = child.stdout.take().ok_or("yt-dlp produced no output")?;
    let stderr = child.stderr.take().map(drain_stderr);
    let source = ChildAudio { child, stdout, stderr };

    let id = video_id.to_string();
    let on_complete: OnComplete = Box::new(move |outcome| {
        if let Ok(data) = outcome {
            if !data.is_empty() {
                match write_cache(&id, data) {
                    Ok(()) => {
                        if let Some(hook) = ON_CACHED.get() {
                            hook(&id);
                        }
                    }
                    Err(e) => eprintln!("[youtube] couldn't cache {id}: {e}"),
                }
            }
        }
    });

    let reader = StreamReader::from_reader(source, Some(on_complete));
    // Surface "video unavailable" etc. as a clean message rather than a
    // decoder probe error.
    reader.wait_for_data()?;
    Ok(Box::new(reader))
}

// ---------- Cache ----------

fn touch(path: &Path) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

fn write_cache(video_id: &str, data: &[u8]) -> Result<(), String> {
    let dir = cache_dir()?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let final_path = cached_file(video_id)?;
    let tmp_path = dir.join(format!("{video_id}.part"));
    {
        let mut f = fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
        f.write_all(data).map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp_path, &final_path).map_err(|e| e.to_string())?;
    prune_cache(CACHE_LIMIT_BYTES);
    Ok(())
}

fn cache_entries() -> Vec<(PathBuf, u64, SystemTime)> {
    let Ok(dir) = cache_dir() else { return Vec::new() };
    let Ok(read) = fs::read_dir(dir) else { return Vec::new() };
    read.filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "m4a"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((e.path(), meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect()
}

fn prune_cache(limit: u64) {
    let mut entries = cache_entries();
    let mut total: u64 = entries.iter().map(|e| e.1).sum();
    entries.sort_by_key(|e| e.2); // oldest first
    for (path, size, _) in entries {
        if total <= limit {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

pub fn cache_size_bytes() -> u64 {
    cache_entries().iter().map(|e| e.1).sum()
}

pub fn clear_cache() -> Result<(), String> {
    for (path, _, _) in cache_entries() {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

// ---------- One-time setup & updates ----------

#[derive(Serialize, Clone)]
pub struct SetupProgress {
    pub stage: String,
    pub downloaded: u64,
    pub total: Option<u64>,
}

fn download(app: &AppHandle, url: &str, dest: &Path, stage: &str) -> Result<(), String> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("download failed ({stage}): {e}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut body = response.into_body().into_reader();

    let tmp = dest.with_extension("download");
    let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut last_emit: u64 = 0;
    loop {
        let n = body.read(&mut buf).map_err(|e| format!("download failed ({stage}): {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        downloaded += n as u64;
        if downloaded - last_emit >= 512 * 1024 {
            last_emit = downloaded;
            let _ = app.emit(
                "youtube-setup-progress",
                SetupProgress { stage: stage.into(), downloaded, total },
            );
        }
    }
    drop(file);
    fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(())
}

/// Unzips `zip` into `dest_dir` using the OS's own tools (Windows 10+
/// ships bsdtar as tar.exe, with PowerShell as a fallback), so no zip
/// crate is needed just for this.
fn extract_zip(zip: &Path, dest_dir: &Path) -> Result<(), String> {
    let mut attempts: Vec<Command> = Vec::new();
    #[cfg(windows)]
    {
        let mut tar = hidden_command(Path::new("tar"));
        tar.arg("-xf").arg(zip).arg("-C").arg(dest_dir);
        attempts.push(tar);
        let mut ps = hidden_command(Path::new("powershell"));
        ps.args(["-NoProfile", "-NonInteractive", "-Command"]).arg(format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            zip.display(),
            dest_dir.display()
        ));
        attempts.push(ps);
    }
    #[cfg(not(windows))]
    {
        let mut unzip = Command::new("unzip");
        unzip.arg("-o").arg(zip).arg("-d").arg(dest_dir);
        attempts.push(unzip);
        let mut tar = Command::new("tar");
        tar.arg("-xf").arg(zip).arg("-C").arg(dest_dir);
        attempts.push(tar);
    }
    for mut cmd in attempts {
        if cmd.stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false) {
            return Ok(());
        }
    }
    Err("couldn't unpack the Deno download".into())
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// Downloads yt-dlp and Deno into the tools folder (skipping whichever is
/// already there). Emits `youtube-setup-progress` events as it goes.
pub fn setup(app: &AppHandle) -> Result<(), String> {
    let _guard = SETUP_LOCK
        .try_lock()
        .map_err(|_| "Setup is already running.".to_string())?;
    let dir = tools_dir()?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let ytdlp = ytdlp_path()?;
    if !ytdlp.is_file() {
        download(app, platform::YTDLP_URL, &ytdlp, "yt-dlp")?;
        make_executable(&ytdlp);
    }

    let deno = deno_path()?;
    if !deno.is_file() {
        let zip = dir.join("deno.zip");
        download(app, platform::DENO_URL, &zip, "Deno")?;
        let _ = app.emit(
            "youtube-setup-progress",
            SetupProgress { stage: "Unpacking".into(), downloaded: 0, total: None },
        );
        let result = extract_zip(&zip, &dir);
        let _ = fs::remove_file(&zip);
        result?;
        if !deno.is_file() {
            return Err("Deno download didn't contain the expected program".into());
        }
        make_executable(&deno);
    }
    Ok(())
}

/// Self-updates yt-dlp (`yt-dlp -U`). Cheap when already current.
pub fn update_ytdlp() -> Result<String, String> {
    let _guard = SETUP_LOCK
        .try_lock()
        .map_err(|_| "Setup is already running.".to_string())?;
    let output = hidden_command(&ytdlp_path()?)
        .arg("-U")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() {
        Ok(text.lines().last().unwrap_or("yt-dlp is up to date").to_string())
    } else {
        Err(friendly_error(&String::from_utf8_lossy(&output.stderr)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video_ids() {
        let id = Some("dQw4w9WgXcQ".to_string());
        assert_eq!(video_id_from_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), id);
        assert_eq!(video_id_from_url("https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RDAMVM"), id);
        assert_eq!(video_id_from_url("https://youtu.be/dQw4w9WgXcQ?si=abc"), id);
        assert_eq!(video_id_from_url("https://youtube.com/shorts/dQw4w9WgXcQ"), id);
        assert_eq!(video_id_from_url("https://m.youtube.com/watch?feature=x&v=dQw4w9WgXcQ"), id);
        assert_eq!(video_id_from_url("https://example.com/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(video_id_from_url("https://www.youtube.com/playlist?list=PL123"), None);
        assert_eq!(video_id_from_url("not a url"), None);
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(
            split_artist_title("Arijit Singh - Tum Hi Ho (Official Video) [HD]", "T-Series"),
            ("Arijit Singh".into(), "Tum Hi Ho".into())
        );
        assert_eq!(
            split_artist_title("Bohemian Rhapsody (Remastered 2011)", "Queen - Topic"),
            ("Queen".into(), "Bohemian Rhapsody (Remastered 2011)".into())
        );
        assert_eq!(
            split_artist_title("Song Name (Live) (Lyric Video)", "ArtistVEVO"),
            ("Artist".into(), "Song Name (Live)".into())
        );
        assert_eq!(
            split_artist_title("Track [Official Music Video]", "Band"),
            ("Band".into(), "Track".into())
        );
    }

    #[test]
    fn parses_search_json() {
        let json: Value = serde_json::from_str(
            r#"{"entries":[
                {"id":"dQw4w9WgXcQ","title":"Rick Astley - Never Gonna Give You Up (Official Music Video)","duration":213.0,"channel":"Rick Astley"},
                {"id":"UCxxxxxxxxxxxxxxxxxxxxxx","title":"A channel","duration":null},
                {"id":"abcdefghijk","title":"Live now","duration":null,"live_status":"is_live"},
                {"id":"ABCDEFGHIJK","title":"[Deleted video]","duration":10}
            ]}"#,
        )
        .unwrap();
        let results = results_from_json(&json);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artist, "Rick Astley");
        assert_eq!(results[0].title, "Never Gonna Give You Up");
        assert_eq!(results[0].duration_seconds, 213.0);
    }
}

