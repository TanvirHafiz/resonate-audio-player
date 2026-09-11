use rodio::{Decoder, Source};
use std::fs::File;
use std::io::BufReader;

#[derive(Debug, Clone, Copy)]
pub struct Features {
    pub tempo_bpm: f64,
    pub energy: f64,
    pub loudness: f64,
    pub brightness: f64,
    pub acousticness: f64,
    pub valence: f64,
}

/// Decodes `path` and computes a lightweight audio-feature vector using
/// simple time-domain DSP — no FFT, no pretrained model. Tempo comes from
/// autocorrelation of a coarse energy envelope, brightness from
/// zero-crossing rate, and acousticness from crest factor (peak/RMS).
///
/// This is a heuristic feature set good enough to group similar-sounding
/// tracks and drive the recommendation engine; it is not a substitute for
/// a real MIR (music information retrieval) model.
/// Cap on how much audio is actually decoded/analyzed per track. Mood/tempo
/// characteristics are stable enough within a track that a representative
/// sample is plenty — and without this cap, a multi-hour audiobook or
/// podcast episode dropped into the library would decode for a very long
/// time (or exhaust memory) before this heuristic analysis even applies.
const MAX_ANALYSIS_SECONDS: f64 = 90.0;

pub fn analyze(path: &str) -> Result<Features, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let decoder = Decoder::new(BufReader::new(file)).map_err(|e| e.to_string())?;
    let channels = decoder.channels().get() as usize;
    let sample_rate = decoder.sample_rate().get() as f64;

    let max_samples = (MAX_ANALYSIS_SECONDS * sample_rate) as usize * channels.max(1);
    let samples: Vec<f32> = decoder.take(max_samples).collect();
    if samples.len() < channels.max(1) {
        return Err("no audio samples decoded".into());
    }

    let mono: Vec<f32> = if channels > 1 {
        samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    } else {
        samples
    };

    let energy_rms = rms(&mono);
    let peak = mono.iter().fold(0.0f32, |m, &s| m.max(s.abs())) as f64;
    let loudness = peak.clamp(0.0, 1.0);

    let crest_factor = if energy_rms > 0.0001 {
        peak / energy_rms
    } else {
        0.0
    };
    // Percussive/transient-heavy audio tends toward a higher peak-to-RMS
    // ratio; a LOW crest factor is treated as more "acoustic"/sustained.
    let acousticness = (1.0 - (crest_factor / 12.0)).clamp(0.0, 1.0);

    let zcr = zero_crossing_rate(&mono);
    // Typical music rarely exceeds ~0.12 zero-crossings/sample; scale that
    // range into 0-1 as a brightness proxy.
    let brightness = (zcr / 0.12).clamp(0.0, 1.0);

    let energy = energy_norm(energy_rms);
    let tempo_bpm = estimate_tempo(&mono, sample_rate).unwrap_or(0.0);
    let tempo_norm = (tempo_bpm / 200.0).clamp(0.0, 1.0);

    let valence = (0.45 * energy + 0.30 * brightness + 0.25 * tempo_norm).clamp(0.0, 1.0);

    Ok(Features {
        tempo_bpm,
        energy,
        loudness,
        brightness,
        acousticness,
        valence,
    })
}

fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / samples.len() as f64).sqrt()
}

fn energy_norm(rms_value: f64) -> f64 {
    // Typical program material sits well under 0.25 RMS for float PCM;
    // scale so loud, dense mixes approach 1.0.
    (rms_value / 0.25).clamp(0.0, 1.0)
}

fn zero_crossing_rate(samples: &[f32]) -> f64 {
    if samples.len() < 2 {
        return 0.0;
    }
    let crossings = samples
        .windows(2)
        .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
        .count();
    crossings as f64 / samples.len() as f64
}

/// Estimates tempo via autocorrelation of a coarse energy envelope, looking
/// for the strongest periodicity between 60 and 180 BPM.
fn estimate_tempo(samples: &[f32], sample_rate: f64) -> Option<f64> {
    let frame_size = (sample_rate * 0.02) as usize; // ~20ms frames
    if frame_size == 0 || samples.len() < frame_size * 4 {
        return None;
    }

    let envelope: Vec<f64> = samples.chunks(frame_size).map(rms).collect();

    // Onset/novelty function: positive-only frame-to-frame energy increase.
    let novelty: Vec<f64> = envelope
        .windows(2)
        .map(|w| (w[1] - w[0]).max(0.0))
        .collect();
    if novelty.len() < 8 {
        return None;
    }

    let frame_rate = sample_rate / frame_size as f64; // frames per second
    let min_bpm = 60.0;
    let max_bpm = 180.0;
    let min_lag = (frame_rate * 60.0 / max_bpm).round().max(1.0) as usize;
    let max_lag = ((frame_rate * 60.0 / min_bpm).round() as usize).min(novelty.len().saturating_sub(1));
    if max_lag <= min_lag {
        return None;
    }

    let mean = novelty.iter().sum::<f64>() / novelty.len() as f64;
    let centered: Vec<f64> = novelty.iter().map(|v| v - mean).collect();

    let mut best_lag = min_lag;
    let mut best_score = f64::MIN;
    for lag in min_lag..=max_lag {
        let mut score = 0.0;
        for i in 0..centered.len() - lag {
            score += centered[i] * centered[i + lag];
        }
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }

    if best_score <= 0.0 {
        return None;
    }

    Some(frame_rate * 60.0 / best_lag as f64)
}
