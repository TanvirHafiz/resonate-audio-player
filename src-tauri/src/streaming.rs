use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

struct Inner {
    data: Vec<u8>,
    done: bool,
    error: Option<String>,
}

/// A `Read + Seek` view over an HTTP(S) response body that is still being
/// downloaded in a background thread. Bytes already downloaded are kept in
/// memory so playback can start before the whole stream has arrived, and so
/// symphonia's format-sniffing (which typically seeks back to the start
/// after reading a few header bytes) has something to seek into.
///
/// Seeking past what has been downloaded so far blocks until either enough
/// data arrives or the download finishes/fails.
pub struct StreamReader {
    shared: Arc<(Mutex<Inner>, Condvar)>,
    position: usize,
}

impl StreamReader {
    /// Spawns a background thread that streams `url` into memory and
    /// returns a reader over it. Errors from the initial connection are
    /// returned immediately; errors after that surface from `read`.
    pub fn open(url: &str) -> Result<Self, String> {
        let response = ureq::get(url)
            .call()
            .map_err(|e| format!("failed to open stream: {e}"))?;

        let shared = Arc::new((
            Mutex::new(Inner {
                data: Vec::new(),
                done: false,
                error: None,
            }),
            Condvar::new(),
        ));

        let shared_clone = shared.clone();
        thread::spawn(move || {
            let mut body_reader = response.into_body().into_reader();
            let mut buf = [0u8; 16 * 1024];
            loop {
                match body_reader.read(&mut buf) {
                    Ok(0) => {
                        let (lock, cvar) = &*shared_clone;
                        lock.lock().unwrap().done = true;
                        cvar.notify_all();
                        break;
                    }
                    Ok(n) => {
                        let (lock, cvar) = &*shared_clone;
                        lock.lock().unwrap().data.extend_from_slice(&buf[..n]);
                        cvar.notify_all();
                    }
                    Err(e) => {
                        let (lock, cvar) = &*shared_clone;
                        let mut inner = lock.lock().unwrap();
                        inner.error = Some(e.to_string());
                        inner.done = true;
                        cvar.notify_all();
                        break;
                    }
                }
            }
        });

        Ok(StreamReader { shared, position: 0 })
    }
}

impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let (lock, cvar) = &*self.shared;
        let mut inner = lock.lock().unwrap();
        loop {
            if self.position < inner.data.len() {
                let available = &inner.data[self.position..];
                let n = available.len().min(buf.len());
                buf[..n].copy_from_slice(&available[..n]);
                self.position += n;
                return Ok(n);
            }
            if inner.done {
                if let Some(err) = &inner.error {
                    return Err(std::io::Error::other(err.clone()));
                }
                return Ok(0);
            }
            inner = cvar.wait(inner).unwrap();
        }
    }
}

impl Seek for StreamReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let (lock, cvar) = &*self.shared;
        let mut inner = lock.lock().unwrap();

        let target: i64 = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.position as i64 + n,
            SeekFrom::End(_) => {
                return Err(std::io::Error::other(
                    "seeking from end is not supported on a live stream",
                ));
            }
        };
        if target < 0 {
            return Err(std::io::Error::other("negative seek position"));
        }
        let target = target as usize;

        loop {
            if target <= inner.data.len() {
                self.position = target;
                return Ok(target as u64);
            }
            if inner.done {
                return Err(std::io::Error::other(
                    "seek target is past the end of the stream",
                ));
            }
            inner = cvar.wait(inner).unwrap();
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct PodcastEpisode {
    pub title: String,
    pub url: String,
    pub published: Option<String>,
    pub duration_seconds: Option<f64>,
}

fn parse_duration(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<f64>() {
        return Some(secs);
    }
    let parts: Vec<&str> = raw.split(':').collect();
    let mut seconds = 0.0;
    for part in parts {
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(seconds)
}

/// Fetches and parses a podcast RSS feed, returning its episodes with
/// playable enclosure URLs.
pub fn fetch_podcast_episodes(feed_url: &str) -> Result<Vec<PodcastEpisode>, String> {
    let response = ureq::get(feed_url)
        .call()
        .map_err(|e| format!("failed to fetch feed: {e}"))?;
    let reader = response.into_body().into_reader();
    let channel = rss::Channel::read_from(std::io::BufReader::new(reader))
        .map_err(|e| format!("failed to parse feed: {e}"))?;

    let episodes = channel
        .items()
        .iter()
        .filter_map(|item| {
            let url = item.enclosure().map(|e| e.url().to_string())?;
            let title = item.title().unwrap_or("Untitled episode").to_string();
            let duration_seconds = item
                .itunes_ext()
                .and_then(|ext| ext.duration())
                .and_then(parse_duration);
            Some(PodcastEpisode {
                title,
                url,
                published: item.pub_date().map(|s| s.to_string()),
                duration_seconds,
            })
        })
        .collect();

    Ok(episodes)
}
