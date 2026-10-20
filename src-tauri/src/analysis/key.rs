//! Offline musical-key estimate for a decoded track: a whole-track chromagram
//! (peak-picked FFT magnitudes folded onto the 12 pitch classes) correlated
//! against major/minor key profiles in all 24 keys. Same
//! honesty rule as the tempo pass: near-silent or atonal input gets `None`,
//! never a guessed key. Allocating and CPU-bound — a load-time analysis, never
//! the audio callback.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// The key verdict: Camelot code (`"8A"`), conventional name (`"A min"`), and
/// the winning profile correlation (Pearson r, 0..1 in practice).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyEstimate {
    pub camelot: String,
    pub name: String,
    pub confidence: f64,
}

/// ~2.9 Hz bins at 48 kHz — two bins per semitone from ~100 Hz up, so the
/// fold to pitch classes is unambiguous across the range that carries harmony.
const FRAME: usize = 16_384;
/// Measured on 113 real renders whose prompt named a key: reaching up to
/// 5 kHz let upper partials vote for the fifth (a third of the misses were
/// "a fifth off"); 120 Hz–3 kHz plus the EDMA profiles below took exact
/// agreement from 23 to 46 (the rest: model ignored the prompt, or near
/// misses — relative/parallel/fifth).
const MIN_HZ: f64 = 120.0;
const MAX_HZ: f64 = 3_000.0;
/// Below this RMS (≈ -60 dBFS) a frame is silence and contributes nothing.
const SILENT_RMS: f64 = 1e-3;
/// The honesty gates. A flat chroma (noise, drums) correlates with SOME
/// profile by chance — 12 numbers make Pearson r jumpy — so the chroma must
/// first be lumpy (coefficient of variation) and then fit a key well.
/// ponytail: thresholds from synthetic fixtures plus a look at real renders;
/// raise them if wrong keys show up on drum-heavy material.
const MIN_CHROMA_CV: f64 = 0.1;
const MIN_CORRELATION: f64 = 0.5;

/// Faraldo et al.'s EDMA profiles (electronic dance music, as shipped in
/// Essentia): on the same 113 renders they matched the prompt's key more often
/// than Krumhansl–Kessler or Temperley, and LSDJ's material is that genre.
const MAJOR: [f64; 12] = [
    0.165, 0.047, 0.083, 0.067, 0.100, 0.093, 0.053, 0.132, 0.052, 0.074, 0.069, 0.064,
];
const MINOR: [f64; 12] = [
    0.172, 0.040, 0.076, 0.120, 0.058, 0.061, 0.050, 0.138, 0.075, 0.053, 0.072, 0.072,
];
const MAJOR_NAMES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];
const MINOR_NAMES: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "G#", "A", "Bb", "B",
];

/// Estimate the key of a stereo track, or `None` when it is silent/atonal.
pub fn track_key(left: &[f32], right: &[f32], sample_rate: f64) -> Option<KeyEstimate> {
    let chroma = chromagram(left, right, sample_rate)?;
    let mean = chroma.iter().sum::<f64>() / 12.0;
    let std = (chroma.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / 12.0).sqrt();
    if mean <= 0.0 || std / mean < MIN_CHROMA_CV {
        return None;
    }
    let mut best: Option<(f64, usize, bool)> = None;
    for tonic in 0..12 {
        for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
            let rotated: Vec<f64> = (0..12).map(|pc| profile[(pc + 12 - tonic) % 12]).collect();
            let r = pearson(&chroma, &rotated);
            if best.is_none_or(|(held, _, _)| r > held) {
                best = Some((r, tonic, minor));
            }
        }
    }
    let (r, tonic, minor) = best?;
    if r < MIN_CORRELATION {
        return None;
    }
    Some(KeyEstimate {
        camelot: camelot(tonic, minor),
        name: if minor {
            format!("{} min", MINOR_NAMES[tonic])
        } else {
            format!("{} maj", MAJOR_NAMES[tonic])
        },
        confidence: r,
    })
}

/// Camelot wheel code for a tonic pitch class (C = 0): majors are `B`, minors
/// share their relative major's number with `A` (C maj = 8B, A min = 8A).
fn camelot(tonic: usize, minor: bool) -> String {
    let major_pc = if minor { (tonic + 3) % 12 } else { tonic };
    let number = (major_pc * 7 + 7) % 12 + 1;
    format!("{number}{}", if minor { 'A' } else { 'B' })
}

/// Whole-track pitch-class energy: Hann-windowed frames, spectral peaks only
/// (a peak is a partial; the noise floor between them is not harmony).
fn chromagram(left: &[f32], right: &[f32], sample_rate: f64) -> Option<[f64; 12]> {
    let fft = FftPlanner::<f64>::new().plan_fft_forward(FRAME);
    let window: Vec<f64> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / FRAME as f64).cos())
        .collect();
    let bin_hz = sample_rate / FRAME as f64;
    let pitch_class: Vec<Option<usize>> = (0..FRAME / 2)
        .map(|k| {
            let hz = k as f64 * bin_hz;
            (MIN_HZ..=MAX_HZ).contains(&hz).then(|| {
                let midi = 69.0 + 12.0 * (hz / 440.0).log2();
                (midi.round() as i64).rem_euclid(12) as usize
            })
        })
        .collect();

    let mut chroma = [0.0f64; 12];
    let mut buf = vec![Complex::new(0.0, 0.0); FRAME];
    let mut mags = vec![0.0f64; FRAME / 2];
    let mut voiced = false;
    let frames = left.len().min(right.len());
    let mut start = 0;
    while start + FRAME <= frames {
        let mut energy = 0.0;
        for i in 0..FRAME {
            let s = 0.5 * (left[start + i] as f64 + right[start + i] as f64);
            energy += s * s;
            buf[i] = Complex::new(s * window[i], 0.0);
        }
        start += FRAME;
        if (energy / FRAME as f64).sqrt() < SILENT_RMS {
            continue;
        }
        voiced = true;
        fft.process(&mut buf);
        for (m, c) in mags.iter_mut().zip(&buf) {
            *m = c.norm();
        }
        for k in 1..mags.len() - 1 {
            if let Some(pc) = pitch_class[k] {
                if mags[k] > mags[k - 1] && mags[k] >= mags[k + 1] {
                    chroma[pc] += mags[k];
                }
            }
        }
    }
    voiced.then_some(chroma)
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        num += (x - ma) * (y - mb);
        da += (x - ma).powi(2);
        db += (y - mb).powi(2);
    }
    if da == 0.0 || db == 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn hz(midi: f64) -> f64 {
        440.0 * 2f64.powf((midi - 69.0) / 12.0)
    }

    /// Mono sum of harmonic tones (3 partials, falling 1/n) — `notes` are
    /// (midi, start s, end s) spans, so chords and melodies both fit.
    fn render(notes: &[(f64, f64, f64)], seconds: f64) -> Vec<f32> {
        let mut out = vec![0.0f32; (seconds * SR) as usize];
        for &(midi, from, to) in notes {
            let f = hz(midi);
            for i in (from * SR) as usize..((to * SR) as usize).min(out.len()) {
                let t = i as f64 / SR;
                let s: f64 = (1..=3)
                    .map(|n| (2.0 * std::f64::consts::PI * f * n as f64 * t).sin() / n as f64)
                    .sum();
                out[i] += (0.1 * s) as f32;
            }
        }
        out
    }

    fn key_of(mono: &[f32]) -> Option<KeyEstimate> {
        track_key(mono, mono, SR)
    }

    fn chord(root_notes: &[f64], seconds: f64) -> Vec<f32> {
        let notes: Vec<_> = root_notes.iter().map(|&m| (m, 0.0, seconds)).collect();
        render(&notes, seconds)
    }

    #[test]
    fn camelot_codes_match_the_wheel() {
        assert_eq!(camelot(0, false), "8B"); // C maj
        assert_eq!(camelot(9, true), "8A"); // A min
        assert_eq!(camelot(7, false), "9B"); // G maj
        assert_eq!(camelot(5, false), "7B"); // F maj
        assert_eq!(camelot(11, false), "1B"); // B maj
        assert_eq!(camelot(0, true), "5A"); // C min
        assert_eq!(camelot(6, true), "11A"); // F# min
    }

    #[test]
    fn a_minor_triad_is_8a() {
        let key = key_of(&chord(&[57.0, 60.0, 64.0], 10.0)).expect("a key");
        assert_eq!((key.camelot.as_str(), key.name.as_str()), ("8A", "A min"));
    }

    #[test]
    fn c_major_scale_over_a_tonic_pad_is_8b() {
        let mut notes = vec![(48.0, 0.0, 14.0), (52.0, 0.0, 14.0), (55.0, 0.0, 14.0)];
        for (i, m) in [60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 71.0, 72.0]
            .iter()
            .enumerate()
        {
            notes.push((*m, i as f64 * 1.5, i as f64 * 1.5 + 1.5));
        }
        let key = key_of(&render(&notes, 14.0)).expect("a key");
        assert_eq!((key.camelot.as_str(), key.name.as_str()), ("8B", "C maj"));
    }

    #[test]
    fn other_tonics_rotate_correctly() {
        let g = key_of(&chord(&[55.0, 59.0, 62.0], 8.0)).expect("G");
        assert_eq!((g.camelot.as_str(), g.name.as_str()), ("9B", "G maj"));
        let fs = key_of(&chord(&[54.0, 57.0, 61.0], 8.0)).expect("F#m");
        assert_eq!((fs.camelot.as_str(), fs.name.as_str()), ("11A", "F# min"));
    }

    #[test]
    fn silence_noise_and_drums_have_no_key() {
        let silence = vec![0.0f32; (5.0 * SR) as usize];
        assert!(key_of(&silence).is_none());
        let mut noise = crate::analysis::beat::fixtures::noise_source(7);
        let white: Vec<f32> = (0..(10.0 * SR) as usize)
            .map(|_| (noise() * 0.2) as f32)
            .collect();
        assert!(key_of(&white).is_none(), "{:?}", key_of(&white));
        let clicks = crate::analysis::beat::fixtures::click_track(124.0, 10.0, SR, 3);
        let (l, r) = crate::analysis::beat::fixtures::deinterleave(&clicks);
        assert!(
            track_key(&l, &r, SR).is_none(),
            "{:?}",
            track_key(&l, &r, SR)
        );
    }
}
