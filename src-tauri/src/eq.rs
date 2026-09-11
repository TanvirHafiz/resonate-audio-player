//! A minimal 3-band (bass/mid/treble) equalizer, applied as a `rodio::Source`
//! wrapper so preset changes take effect on whatever's currently playing
//! without restarting it. Each band is a peaking biquad filter (RBJ Audio
//! Cookbook formulas); presets just pick gains for the three bands.

use rodio::{ChannelCount, SampleRate, Source};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BASS_HZ: f32 = 100.0;
const MID_HZ: f32 = 1000.0;
const TREBLE_HZ: f32 = 8000.0;
const Q: f32 = 0.7;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EqParams {
    pub bass_db: f32,
    pub mid_db: f32,
    pub treble_db: f32,
}

impl EqParams {
    pub const FLAT: EqParams = EqParams {
        bass_db: 0.0,
        mid_db: 0.0,
        treble_db: 0.0,
    };
}

/// A named gain preset. `name` matches what the frontend shows/sends.
pub struct Preset {
    pub name: &'static str,
    pub params: EqParams,
}

pub const PRESETS: &[Preset] = &[
    Preset { name: "Flat", params: EqParams::FLAT },
    Preset { name: "Rock", params: EqParams { bass_db: 4.0, mid_db: -2.0, treble_db: 3.0 } },
    Preset { name: "Pop", params: EqParams { bass_db: 2.0, mid_db: 1.0, treble_db: 2.0 } },
    Preset { name: "Disco", params: EqParams { bass_db: 5.0, mid_db: 0.0, treble_db: 4.0 } },
    Preset { name: "Soft", params: EqParams { bass_db: 1.0, mid_db: 2.0, treble_db: -1.0 } },
    Preset { name: "Bass Boost", params: EqParams { bass_db: 6.0, mid_db: 0.0, treble_db: 0.0 } },
    Preset { name: "Vocal Boost", params: EqParams { bass_db: -2.0, mid_db: 4.0, treble_db: 0.0 } },
];

pub fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|p| p.name).collect()
}

fn params_for(name: &str) -> EqParams {
    PRESETS
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| p.params)
        .unwrap_or(EqParams::FLAT)
}

/// Shared handle between the playback command layer and the live audio
/// source. `version` lets the audio thread cheaply check "did anything
/// change?" every sample without locking `params` unless it actually did.
struct EqShared {
    version: AtomicU64,
    params: Mutex<EqParams>,
    preset_name: Mutex<String>,
}

#[derive(Clone)]
pub struct EqHandle(Arc<EqShared>);

impl EqHandle {
    pub fn new() -> Self {
        EqHandle(Arc::new(EqShared {
            version: AtomicU64::new(0),
            params: Mutex::new(EqParams::FLAT),
            preset_name: Mutex::new("Flat".to_string()),
        }))
    }

    pub fn set_preset(&self, name: &str) {
        let params = params_for(name);
        *self.0.params.lock().unwrap() = params;
        *self.0.preset_name.lock().unwrap() = name.to_string();
        self.0.version.fetch_add(1, Ordering::Relaxed);
    }

    pub fn current_preset(&self) -> String {
        self.0.preset_name.lock().unwrap().clone()
    }
}

#[derive(Copy, Clone)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        // Identity filter (no-op) until the first coefficient update.
        Biquad { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }
}

impl Biquad {
    fn set_peaking(&mut self, freq_hz: f32, q: f32, gain_db: f32, sample_rate: f32) {
        if sample_rate <= 0.0 {
            return;
        }
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * freq_hz / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Wraps any `f32` rodio [`Source`] with the 3-band EQ. Filter state is kept
/// per channel (so a stereo L/R signal doesn't bleed into a single filter's
/// history) and coefficients are only recomputed when `handle`'s preset
/// actually changes.
pub struct EqSource<S> {
    inner: S,
    channels: usize,
    channel_idx: usize,
    filters: Vec<[Biquad; 3]>,
    sample_rate: f32,
    handle: EqHandle,
    local_version: u64,
}

impl<S: Source<Item = f32>> EqSource<S> {
    pub fn new(inner: S, handle: EqHandle) -> Self {
        let channels = inner.channels().get() as usize;
        let sample_rate = inner.sample_rate().get() as f32;
        EqSource {
            inner,
            channels: channels.max(1),
            channel_idx: 0,
            filters: vec![[Biquad::default(); 3]; channels.max(1)],
            sample_rate,
            handle,
            // Guarantee a coefficient computation on the very first sample.
            local_version: u64::MAX,
        }
    }

    fn refresh_if_needed(&mut self) {
        let version = self.handle.0.version.load(Ordering::Relaxed);
        if version == self.local_version {
            return;
        }
        self.local_version = version;
        let params = *self.handle.0.params.lock().unwrap();
        for triple in &mut self.filters {
            triple[0].set_peaking(BASS_HZ, Q, params.bass_db, self.sample_rate);
            triple[1].set_peaking(MID_HZ, Q, params.mid_db, self.sample_rate);
            triple[2].set_peaking(TREBLE_HZ, Q, params.treble_db, self.sample_rate);
        }
    }
}

impl<S: Source<Item = f32>> Iterator for EqSource<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.inner.next()?;
        self.refresh_if_needed();

        let triple = &mut self.filters[self.channel_idx];
        self.channel_idx = (self.channel_idx + 1) % self.channels;

        let mut s = sample;
        s = triple[0].process(s);
        s = triple[1].process(s);
        s = triple[2].process(s);
        Some(s)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S: Source<Item = f32>> Source for EqSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)
    }
}
