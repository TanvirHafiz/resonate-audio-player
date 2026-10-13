use crate::eq::{EqHandle, EqSource};
use crate::streaming::StreamReader;
use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::stream::{DeviceSinkBuilder, MixerDeviceSink};
use rodio::{Decoder, Player as RodioPlayer, Source};
use std::fs::File;
use std::io::BufReader;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct PlaybackState {
    _sink_device: MixerDeviceSink,
    player: RodioPlayer,
    pub queue: Vec<i64>,
    pub queue_index: Option<usize>,
    pub is_playing: bool,
    session_id: i64,
    device_name: String,
    eq: EqHandle,
    /// Bumped every time a new track starts loading, so a slow background
    /// load (e.g. a YouTube stream) that finishes after the user has
    /// already moved on can tell it's stale and discard itself.
    load_generation: u64,
    /// True while a background load is in flight. Keeps the auto-advance
    /// check (`sink_empty`) from treating "not loaded yet" as "finished".
    pub loading: bool,
    /// Last background-load failure, handed to the frontend once via
    /// `take_last_error` so it can show a message.
    last_error: Option<String>,
}

/// Shared behind an `Arc` so background loaders (see `play_async`) can
/// reach the player from their own thread once their audio is ready.
pub struct Player(pub Arc<Mutex<PlaybackState>>);

impl Player {
    pub fn new() -> Self {
        let sink_device =
            DeviceSinkBuilder::open_default_sink().expect("failed to open default audio output");
        let player = RodioPlayer::connect_new(sink_device.mixer());
        let session_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let device_name = cpal::default_host()
            .default_output_device()
            .and_then(|d| d.description().ok().map(|desc| desc.name().to_string()))
            .unwrap_or_else(|| "Default".to_string());
        Player(Arc::new(Mutex::new(PlaybackState {
            _sink_device: sink_device,
            player,
            queue: Vec::new(),
            queue_index: None,
            is_playing: false,
            session_id,
            device_name,
            eq: EqHandle::new(),
            load_generation: 0,
            loading: false,
            last_error: None,
        })))
    }
}

/// Names of the built-in EQ presets ("Flat", "Rock", "Disco", ...), used by
/// the pro-tier settings panel.
pub fn eq_preset_names() -> Vec<&'static str> {
    crate::eq::preset_names()
}

pub fn current_eq_preset(player: &Player) -> String {
    player.0.lock().unwrap().eq.current_preset()
}

/// Applies an EQ preset by name, live — takes effect on whatever's playing
/// right now as well as anything played next (the handle is shared with
/// the currently-loaded `EqSource`, if any).
pub fn set_eq_preset(player: &Player, name: &str) {
    player.0.lock().unwrap().eq.set_preset(name);
}

/// Lists the names of available audio output devices (used by the
/// pro-tier settings panel; the capability lives here since only this
/// module can touch `PlaybackState`'s internals).
pub fn list_output_devices() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|devices| {
            devices
                .filter_map(|d| d.description().ok().map(|desc| desc.name().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn current_output_device(player: &Player) -> String {
    player.0.lock().unwrap().device_name.clone()
}

/// Switches audio output to the named device, rebuilding the sink/mixer.
/// This stops whatever is currently playing (the queue position is kept,
/// so the frontend can just hit play again); there is no in-place device
/// hot-swap without interrupting the audio stream.
pub fn set_output_device(player: &Player, device_name: &str) -> Result<(), String> {
    let device = cpal::default_host()
        .output_devices()
        .map_err(|e| e.to_string())?
        .find(|d| {
            d.description()
                .map(|desc| desc.name() == device_name)
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("output device '{device_name}' not found"))?;

    let sink_device = DeviceSinkBuilder::from_device(device)
        .map_err(|e| e.to_string())?
        .open_stream()
        .map_err(|e| e.to_string())?;
    let new_player = RodioPlayer::connect_new(sink_device.mixer());

    let mut state = player.0.lock().unwrap();
    state._sink_device = sink_device;
    state.player = new_player;
    state.is_playing = false;
    state.loading = false;
    state.load_generation += 1;
    state.device_name = device_name.to_string();
    Ok(())
}

pub fn session_id(player: &Player) -> i64 {
    player.0.lock().unwrap().session_id
}

fn start_source(state: &mut PlaybackState, source: impl Source<Item = f32> + Send + 'static) {
    // Any background load still in flight is now stale.
    state.load_generation += 1;
    state.loading = false;
    state.player.stop();
    state.player.append(source);
    state.player.play();
    state.is_playing = true;
}

pub fn set_queue(player: &Player, track_ids: Vec<i64>, start_index: usize) {
    let mut state = player.0.lock().unwrap();
    state.queue = track_ids;
    state.queue_index = if state.queue.is_empty() {
        None
    } else {
        Some(start_index.min(state.queue.len() - 1))
    };
}

pub fn play_file(player: &Player, file_path: &str) -> Result<(), String> {
    let file = File::open(file_path).map_err(|e| e.to_string())?;
    let source = Decoder::new(BufReader::new(file)).map_err(|e| e.to_string())?;

    let mut state = player.0.lock().unwrap();
    let source = EqSource::new(source, state.eq.clone());
    start_source(&mut state, source);
    Ok(())
}

/// Plays an internet radio stream, podcast episode, or arbitrary audio URL.
/// The stream downloads progressively in a background thread (see
/// `streaming::StreamReader`) rather than needing to be fully fetched first.
pub fn play_stream(player: &Player, url: &str) -> Result<(), String> {
    let reader = StreamReader::open(url)?;
    let source = Decoder::new(reader).map_err(|e| e.to_string())?;

    let mut state = player.0.lock().unwrap();
    let source = EqSource::new(source, state.eq.clone());
    start_source(&mut state, source);
    Ok(())
}

/// A `Read + Seek` byte source that rodio can decode from, produced on a
/// background thread by `play_async`'s loader.
pub type LoadedReader = Box<dyn ReadSeekSend>;

pub trait ReadSeekSend: std::io::Read + std::io::Seek + Send + Sync {}
impl<T: std::io::Read + std::io::Seek + Send + Sync> ReadSeekSend for T {}

/// Starts playing audio whose source takes a while to obtain (e.g. a
/// YouTube stream that needs yt-dlp to resolve and start downloading)
/// without blocking the caller. Stops whatever is playing, marks the
/// player as loading, and runs `loader` plus the decoder's format probe
/// on a background thread. When that finishes, playback starts — unless
/// the user has since moved to another track, in which case the result is
/// dropped. On failure, the error is kept for `take_last_error` and the
/// player is left "playing but empty", so the normal auto-advance moves on
/// to the next queued track.
pub fn play_async<F>(player: &Player, loader: F)
where
    F: FnOnce() -> Result<LoadedReader, String> + Send + 'static,
{
    let generation = {
        let mut state = player.0.lock().unwrap();
        state.player.stop();
        state.load_generation += 1;
        state.loading = true;
        state.is_playing = true;
        state.last_error = None;
        state.load_generation
    };

    let shared = player.0.clone();
    std::thread::spawn(move || {
        let decoded = loader().and_then(|reader| Decoder::new(reader).map_err(|e| e.to_string()));

        let mut state = shared.lock().unwrap();
        if state.load_generation != generation {
            return; // user moved on while we were loading
        }
        state.loading = false;
        match decoded {
            Ok(source) => {
                let user_paused = !state.is_playing;
                let source = EqSource::new(source, state.eq.clone());
                state.player.stop();
                state.player.append(source);
                if user_paused {
                    state.player.pause();
                } else {
                    state.player.play();
                    state.is_playing = true;
                }
            }
            Err(e) => {
                eprintln!("background load failed: {e}");
                state.last_error = Some(e);
            }
        }
    });
}

/// Returns (and clears) the most recent background-load error, if any.
pub fn take_last_error(player: &Player) -> Option<String> {
    player.0.lock().unwrap().last_error.take()
}

pub fn is_loading(player: &Player) -> bool {
    player.0.lock().unwrap().loading
}

pub fn pause(player: &Player) {
    let mut state = player.0.lock().unwrap();
    state.player.pause();
    state.is_playing = false;
}

pub fn resume(player: &Player) {
    let mut state = player.0.lock().unwrap();
    state.player.play();
    state.is_playing = true;
}

pub fn stop(player: &Player) {
    let mut state = player.0.lock().unwrap();
    state.player.stop();
    state.is_playing = false;
    state.loading = false;
    state.load_generation += 1;
}

pub fn set_volume(player: &Player, volume: f32) {
    let state = player.0.lock().unwrap();
    state.player.set_volume(volume.clamp(0.0, 1.0));
}

pub fn get_volume(player: &Player) -> f32 {
    let state = player.0.lock().unwrap();
    state.player.volume()
}

pub fn position_seconds(player: &Player) -> f64 {
    let state = player.0.lock().unwrap();
    state.player.get_pos().as_secs_f64()
}

/// Seeks the currently loaded track/stream to `position_seconds`. Works for
/// local files; for a progressively-downloading stream this blocks until
/// enough of it has been buffered to reach the target (see
/// `streaming::StreamReader::seek`), so a far-forward seek on a stream can
/// take a moment.
pub fn seek(player: &Player, position_seconds: f64) -> Result<(), String> {
    let state = player.0.lock().unwrap();
    state
        .player
        .try_seek(Duration::from_secs_f64(position_seconds.max(0.0)))
        .map_err(|e| e.to_string())
}

pub fn is_playing(player: &Player) -> bool {
    player.0.lock().unwrap().is_playing
}

/// True once the sink has no more queued audio (current track finished).
pub fn sink_empty(player: &Player) -> bool {
    let state = player.0.lock().unwrap();
    state.is_playing && !state.loading && state.player.empty()
}

pub fn current_queue_snapshot(player: &Player) -> (Vec<i64>, Option<usize>) {
    let state = player.0.lock().unwrap();
    (state.queue.clone(), state.queue_index)
}

pub fn advance_queue(player: &Player, direction: i32) -> Option<i64> {
    let mut state = player.0.lock().unwrap();
    if state.queue.is_empty() {
        return None;
    }
    let len = state.queue.len() as i32;
    let current = state.queue_index.map(|i| i as i32).unwrap_or(-1);
    let next = current + direction;
    if next < 0 || next >= len {
        return None;
    }
    state.queue_index = Some(next as usize);
    Some(state.queue[next as usize])
}
