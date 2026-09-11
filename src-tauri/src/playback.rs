use crate::streaming::StreamReader;
use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::stream::{DeviceSinkBuilder, MixerDeviceSink};
use rodio::{Decoder, Player as RodioPlayer, Source};
use std::fs::File;
use std::io::BufReader;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct PlaybackState {
    _sink_device: MixerDeviceSink,
    player: RodioPlayer,
    pub queue: Vec<i64>,
    pub queue_index: Option<usize>,
    pub is_playing: bool,
    session_id: i64,
    device_name: String,
}

pub struct Player(pub Mutex<PlaybackState>);

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
        Player(Mutex::new(PlaybackState {
            _sink_device: sink_device,
            player,
            queue: Vec::new(),
            queue_index: None,
            is_playing: false,
            session_id,
            device_name,
        }))
    }
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
    state.device_name = device_name.to_string();
    Ok(())
}

pub fn session_id(player: &Player) -> i64 {
    player.0.lock().unwrap().session_id
}

fn start_source(state: &mut PlaybackState, source: impl Source<Item = f32> + Send + 'static) {
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
    start_source(&mut state, source);
    Ok(())
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
    state.is_playing && state.player.empty()
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
