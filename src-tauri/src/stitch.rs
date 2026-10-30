//! Bounce (loom plan decision 14): slices of decoded library songs stitched into
//! one take. A loom path is already continuous — a child's clip opens with a
//! re-decode of its parent's tail — so a join is a butt join; the short crossfade
//! only guards against a DC step where two decodes meet.

use std::io::Cursor;
use std::ops::Range;

use crate::decode::DecodedTrack;

const RATE: f64 = lsdj_engine::SAMPLE_RATE as f64;

/// The longest bounce, in frames: 30 minutes.
const MAX_FRAMES: usize = 30 * 60 * lsdj_engine::SAMPLE_RATE as usize;

/// The crossfade at each join.
const JOIN_FADE_SECONDS: f64 = 0.005;

/// Each part's `start..end` seconds as a frame range over its `frames`-long clip,
/// clamped into the clip. Refuses no parts, a non-finite or inverted window, a
/// window the clamp leaves empty, and a total past 30 minutes.
pub fn plan_stitch(parts: &[(usize, f64, f64)]) -> Result<Vec<Range<usize>>, String> {
    if parts.is_empty() {
        return Err("nothing to bounce".to_string());
    }
    let mut ranges = Vec::with_capacity(parts.len());
    for &(frames, start, end) in parts {
        if !start.is_finite() || !end.is_finite() || end <= start {
            return Err("each part of a bounce needs a start before its end".to_string());
        }
        let frame = |seconds: f64| ((seconds.max(0.0) * RATE).round() as usize).min(frames);
        let range = frame(start)..frame(end);
        if range.is_empty() {
            return Err(format!(
                "a part starting at {start:.1}s is past its song's end"
            ));
        }
        ranges.push(range);
    }
    if ranges.iter().map(Range::len).sum::<usize>() > MAX_FRAMES {
        return Err("a bounce can be at most 30 minutes".to_string());
    }
    Ok(ranges)
}

/// The slices end to end as interleaved stereo, exactly as long as the ranges sum
/// to. At each join the incoming part's own pre-roll (the frames just before its
/// start: for a child, its re-decoded copy of the parent's tail) fades in over the
/// outgoing part's last few milliseconds, linearly, the two gains summing to one.
/// A part that starts at its clip's first frame has no pre-roll and joins hard,
/// as the deck's own hand-over does.
pub fn stitch(parts: &[(&DecodedTrack, Range<usize>)]) -> Vec<f32> {
    let fade = (JOIN_FADE_SECONDS * RATE).round() as usize;
    let total: usize = parts.iter().map(|(_, range)| range.len()).sum();
    let mut out = Vec::with_capacity(total * 2);
    for (track, range) in parts {
        let overlap = fade.min(range.start).min(out.len() / 2);
        let from = out.len() - overlap * 2;
        for k in 0..overlap {
            let gain = (k + 1) as f32 / (overlap + 1) as f32;
            let frame = range.start - overlap + k;
            for (channel, side) in [&track.left, &track.right].into_iter().enumerate() {
                let sample = &mut out[from + 2 * k + channel];
                *sample = *sample * (1.0 - gain) + side[frame] * gain;
            }
        }
        for frame in range.clone() {
            out.push(track.left[frame]);
            out.push(track.right[frame]);
        }
    }
    out
}

/// Interleaved stereo at the engine rate as a 16-bit PCM WAV (the songs library's
/// shape, as [`crate::decode::crop_wav`] writes it).
pub fn wav(samples: &[f32]) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: lsdj_engine::SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::with_capacity(samples.len() * 2 + 44));
    let mut writer = hound::WavWriter::new(&mut cursor, spec)
        .map_err(|e| format!("cannot write bounce: {e}"))?;
    for sample in samples {
        let pcm = (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        writer
            .write_sample(pcm)
            .map_err(|e| format!("cannot write bounce: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| format!("cannot write bounce: {e}"))?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(frames: usize, level: f32) -> DecodedTrack {
        DecodedTrack {
            left: vec![level; frames],
            right: vec![-level; frames],
        }
    }

    #[test]
    fn plan_stitch_clamps_into_each_clip_and_refuses_bad_windows() {
        let total = 96_000; // 2 s at 48 kHz
        assert_eq!(
            plan_stitch(&[(total, 0.5, 1.0), (total, -1.0, 9.0)]),
            Ok(vec![24_000..48_000, 0..total])
        );
        assert!(plan_stitch(&[]).is_err());
        assert!(plan_stitch(&[(total, 1.0, 1.0)]).is_err());
        assert!(plan_stitch(&[(total, 1.0, 0.5)]).is_err());
        assert!(plan_stitch(&[(total, 2.0, 3.0)]).is_err());
        assert!(plan_stitch(&[(total, f64::NAN, 1.0)]).is_err());
        assert!(plan_stitch(&[(total, 0.0, f64::INFINITY)]).is_err());
        let half_hour = MAX_FRAMES / 2;
        assert!(plan_stitch(&[(half_hour, 0.0, 900.0), (half_hour, 0.0, 900.0)]).is_ok());
        assert!(plan_stitch(&[
            (half_hour, 0.0, 900.0),
            (half_hour, 0.0, 900.0),
            (total, 0.0, 1.0)
        ])
        .is_err());
    }

    #[test]
    fn stitch_is_as_long_as_its_ranges() {
        let (a, b) = (flat(1_000, 0.5), flat(2_000, 0.25));
        let out = stitch(&[(&a, 100..600), (&b, 0..2_000), (&a, 900..1_000)]);
        assert_eq!(out.len(), (500 + 2_000 + 100) * 2);
    }

    #[test]
    fn join_fades_sum_to_unity() {
        // The same level both sides: a crossfade whose gains sum to one is flat.
        let (a, b) = (flat(1_000, 0.5), flat(1_000, 0.5));
        let out = stitch(&[(&a, 0..1_000), (&b, 500..1_000)]);
        assert!(out
            .chunks(2)
            .all(|f| (f[0] - 0.5).abs() < 1e-6 && (f[1] + 0.5).abs() < 1e-6));
        // From silence into a level: a linear ramp across the outgoing part's last
        // 5 ms, reaching the incoming part exactly at the join.
        let (quiet, loud) = (flat(1_000, 0.0), flat(1_000, 1.0));
        let out = stitch(&[(&quiet, 0..1_000), (&loud, 500..1_000)]);
        let fade = 240;
        assert_eq!(out[2 * (1_000 - fade - 1)], 0.0);
        let ramp: Vec<f32> = (1_000 - fade..1_000).map(|f| out[2 * f]).collect();
        assert!(ramp.windows(2).all(|w| w[1] > w[0]) && ramp[fade - 1] < 1.0);
        assert!(out[2_000..].iter().step_by(2).all(|s| *s == 1.0));
        // No pre-roll (a part from its clip's first frame): a hard join.
        let out = stitch(&[(&quiet, 0..1_000), (&loud, 0..1_000)]);
        assert_eq!((out[1_998], out[2_000]), (0.0, 1.0));
    }

    #[test]
    fn wav_is_48k_16_bit_stereo() {
        let bytes = wav(&stitch(&[(&flat(480, 0.5), 0..480)])).expect("wav");
        let reader = hound::WavReader::new(Cursor::new(&bytes)).expect("reads");
        let spec = reader.spec();
        assert_eq!(
            (spec.channels, spec.sample_rate, spec.bits_per_sample),
            (2, 48_000, 16)
        );
        assert_eq!(reader.len(), 960);
    }
}
