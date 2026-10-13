use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct Track {
    pub id: i64,
    pub file_path: Option<String>,
    pub stream_url: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<i64>,
    pub duration_seconds: f64,
    pub source_type: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct NowPlaying {
    pub track: Option<Track>,
    pub is_playing: bool,
    pub position_seconds: f64,
    pub volume: f32,
    pub queue_index: Option<usize>,
    pub queue_len: usize,
    /// True while the current track's audio is still being fetched in the
    /// background (YouTube tracks take a second or two to start).
    pub loading: bool,
    /// A playback error to show the user once (e.g. a YouTube video that
    /// is unavailable). Cleared after it's been reported.
    pub error: Option<String>,
}
