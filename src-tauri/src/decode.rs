//! Shell-side track decode (ADR-0030): compressed/container audio bytes in,
//! stereo f32 at the engine's 48 kHz out. Replaces the webview's
//! `OfflineAudioContext.decodeAudioData` for deck-track loading, so decoded
//! PCM never crosses the IPC boundary in either direction.
//!
//! Allocating and CPU-heavy by design — a load, not a callback. Runs on the
//! async-command pool; the RT path never appears here.

use std::io::Cursor;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{FixedSync, Resampler};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// The engine sample rate (mirrors `lsdj_engine::SAMPLE_RATE`).
const TARGET_RATE: u32 = lsdj_engine::SAMPLE_RATE;

/// Frames per rubato input block for the offline resample. Any reasonable
/// power of two works; fixed so the FFT plans build once.
const RESAMPLE_CHUNK_FRAMES: usize = 4096;

/// A decoded track at 48 kHz — split channels for the analyses (tempo pass,
/// beatgrid, bands), interleaved on demand for the engine.
pub struct DecodedTrack {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl DecodedTrack {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / TARGET_RATE as f64
    }

    /// The engine wire layout (`Host::load_track` takes interleaved stereo).
    pub fn interleaved(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.left.len() * 2);
        for (l, r) in self.left.iter().zip(&self.right) {
            out.push(*l);
            out.push(*r);
        }
        out
    }
}

/// Decode `bytes` (any allowlisted container/codec) and resample to 48 kHz
/// stereo. `extension` is a probe hint — the scoped commands know the real
/// file name; the in-memory compose path passes `None` (WAV probes fine
/// without one). Errors are strings for the command boundary; they surface
/// to the webview as an explicit load failure (ADR-0030: no silent refusal).
pub fn decode_to_48k(bytes: Vec<u8>, extension: Option<&str>) -> Result<DecodedTrack, String> {
    decode_at(bytes, extension, TARGET_RATE)
}

/// The rate Stable Audio 3 takes init audio at (the generation server's WAV gate).
const SA3_RATE: u32 = 44_100;

/// The longest source clip the generation server accepts: its
/// `MAX_INIT_AUDIO_BYTES` covers one full-length track (`TRACK_MAX_SECONDS`).
pub const MAX_SOURCE_SECONDS: f64 = 380.0;

/// A cut of a library file ready to send as Stable Audio 3 init audio.
pub struct SourceClip {
    /// 44.1 kHz 16-bit stereo PCM WAV — the only shape the server accepts.
    pub wav: Vec<u8>,
    /// The clip's length; output seconds line up with it (the CLI aligns init
    /// audio at 0, trimming or zero-padding it to the requested length).
    pub seconds: f64,
}

/// Cut `start..end` seconds (`None` = to the end; an `end` past the file
/// clamps to it) of any decodable file into a [`SourceClip`]. A 44.1 kHz
/// 16-bit source round-trips bit-exactly, so the model sees the take itself,
/// not a resampled copy.
pub fn source_clip(
    bytes: Vec<u8>,
    extension: Option<&str>,
    start: f64,
    end: Option<f64>,
) -> Result<SourceClip, String> {
    if !start.is_finite() || start < 0.0 {
        return Err("source_start must be 0 seconds or more".to_string());
    }
    if end.is_some_and(|end| !end.is_finite() || end <= start) {
        return Err("source_end must be after source_start".to_string());
    }
    let track = decode_at(bytes, extension, SA3_RATE)?;
    let rate = SA3_RATE as f64;
    let total = track.frames();
    let first = (start * rate).round() as usize;
    let last = end.map_or(total, |end| ((end * rate).round() as usize).min(total));
    if first >= last {
        return Err(format!(
            "source_start {start:.1}s is past the song's end ({:.1}s)",
            total as f64 / rate
        ));
    }
    let seconds = (last - first) as f64 / rate;
    if seconds > MAX_SOURCE_SECONDS {
        return Err(format!(
            "the source window is {seconds:.0}s; a source can be at most \
             {MAX_SOURCE_SECONDS:.0}s — narrow it with source_start/source_end"
        ));
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SA3_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::with_capacity((last - first) * 4 + 44));
    let mut writer =
        hound::WavWriter::new(&mut cursor, spec).map_err(|e| format!("cannot write clip: {e}"))?;
    for frame in first..last {
        for sample in [track.left[frame], track.right[frame]] {
            let pcm = (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            writer
                .write_sample(pcm)
                .map_err(|e| format!("cannot write clip: {e}"))?;
        }
    }
    writer
        .finalize()
        .map_err(|e| format!("cannot write clip: {e}"))?;
    Ok(SourceClip {
        wav: cursor.into_inner(),
        seconds,
    })
}

/// The fade at each crop cut — long enough that a cut mid-waveform doesn't click,
/// short enough not to soften a transient.
const CROP_FADE_SECONDS: f64 = 0.005;

/// `start..end` seconds as a frame range over `total` frames at `rate`. Refuses a
/// non-finite, negative, empty, or past-the-end window rather than clamping it, so
/// the DJ never saves a crop they didn't see.
pub fn crop_frames(start: f64, end: f64, rate: u32, total: usize) -> Result<(usize, usize), String> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        return Err("the crop needs a start before its end".to_string());
    }
    let first = (start * rate as f64).round() as usize;
    let last = (end * rate as f64).round() as usize;
    // Half a frame of slack: the webview's end handle sits at the rounded duration.
    if last > total + 1 {
        return Err(format!(
            "the crop ends at {end:.2}s, past the clip's end ({:.2}s)",
            total as f64 / rate as f64
        ));
    }
    let last = last.min(total);
    if first >= last {
        return Err("the crop is empty".to_string());
    }
    Ok((first, last))
}

/// The gain at frame `index` of a `len`-frame crop with `fade`-frame linear fades
/// at both ends: 0 on the first and last frame, 1 in the body.
pub fn crop_gain(index: usize, len: usize, fade: usize) -> f32 {
    if fade == 0 {
        return 1.0;
    }
    let edge = index.min(len - 1 - index);
    (edge as f32 / fade as f32).min(1.0)
}

/// Cut `start..end` seconds of any decodable file into a new 16-bit PCM WAV at the
/// source's own rate and channel count (mono stays mono; more than two keep the
/// first two), with [`CROP_FADE_SECONDS`] fades at both cuts.
pub fn crop_wav(
    bytes: Vec<u8>,
    extension: Option<&str>,
    start: f64,
    end: f64,
) -> Result<Vec<u8>, String> {
    let (track, rate, channels) = decode_native(bytes, extension)?;
    let (first, last) = crop_frames(start, end, rate, track.frames())?;
    let len = last - first;
    let fade = ((CROP_FADE_SECONDS * rate as f64).round() as usize).min(len / 2);
    let channels: u16 = if channels == 1 { 1 } else { 2 };
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::with_capacity(len * channels as usize * 2 + 44));
    let mut writer =
        hound::WavWriter::new(&mut cursor, spec).map_err(|e| format!("cannot write crop: {e}"))?;
    for index in 0..len {
        let gain = crop_gain(index, len, fade);
        let frame = first + index;
        for sample in [track.left[frame], track.right[frame]]
            .into_iter()
            .take(channels as usize)
        {
            let pcm = (sample * gain * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            writer
                .write_sample(pcm)
                .map_err(|e| format!("cannot write crop: {e}"))?;
        }
    }
    writer
        .finalize()
        .map_err(|e| format!("cannot write crop: {e}"))?;
    Ok(cursor.into_inner())
}

/// A whole file's min/max envelope in `buckets` columns (the channel average), plus
/// its length in seconds — the crop panel's waveform, for a file on no deck.
pub fn clip_peaks(
    bytes: Vec<u8>,
    extension: Option<&str>,
    buckets: usize,
) -> Result<(Vec<f32>, Vec<f32>, f64), String> {
    let (track, rate, _) = decode_native(bytes, extension)?;
    let frames = track.frames();
    let buckets = buckets.clamp(1, 4096);
    let mut min = vec![0.0f32; buckets];
    let mut max = vec![0.0f32; buckets];
    for bucket in 0..buckets {
        let from = bucket * frames / buckets;
        let to = ((bucket + 1) * frames / buckets).max(from + 1).min(frames);
        for frame in from..to {
            let sample = (track.left[frame] + track.right[frame]) / 2.0;
            min[bucket] = min[bucket].min(sample);
            max[bucket] = max[bucket].max(sample);
        }
    }
    Ok((min, max, frames as f64 / rate as f64))
}

/// [`decode_to_48k`] at any `target_rate` (the returned track's
/// `duration_seconds` assumes 48 kHz, so callers at another rate count frames).
fn decode_at(
    bytes: Vec<u8>,
    extension: Option<&str>,
    target_rate: u32,
) -> Result<DecodedTrack, String> {
    let (track, rate, _) = decode_native(bytes, extension)?;
    if rate == target_rate {
        return Ok(track);
    }
    resample(&track.left, &track.right, rate, target_rate)
}

/// Decode at the file's own rate: the track (mono duplicated to both sides), its
/// sample rate, and its channel count.
fn decode_native(
    bytes: Vec<u8>,
    extension: Option<&str>,
) -> Result<(DecodedTrack, u32, usize), String> {
    let mut hint = Hint::new();
    if let Some(ext) = extension {
        hint.with_extension(ext);
    }
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("unsupported audio format: {e}"))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| "no audio track in file".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("unsupported audio codec: {e}"))?;

    let mut sample_rate = 0u32;
    let mut channels = 0usize;
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut scratch: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // A clean end of stream is the normal exit; symphonia surfaces it
            // as an UnexpectedEof I/O error.
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(format!("cannot read audio: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // Per symphonia's contract a decode error is a corrupt packet,
            // recoverable by continuing; anything else is fatal.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("cannot decode audio: {e}")),
        };
        let spec = *decoded.spec();
        if sample_rate == 0 {
            sample_rate = spec.rate;
            channels = spec.channels.count();
            if channels == 0 {
                return Err("no audio channels in file".to_string());
            }
        } else if spec.rate != sample_rate || spec.channels.count() != channels {
            // A chained stream (e.g. concatenated Oggs) can change spec
            // mid-file; reading on would misinterpret every later frame at
            // the first spec. Refuse explicitly — no silent misread.
            return Err("audio format changes mid-file".to_string());
        }
        let buffer = scratch.get_or_insert_with(|| {
            SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)
        });
        if buffer.capacity() < decoded.frames() * channels {
            *buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        }
        buffer.copy_interleaved_ref(decoded);
        let samples = buffer.samples();
        for frame in samples.chunks_exact(channels) {
            left.push(frame[0]);
            right.push(if channels > 1 { frame[1] } else { frame[0] });
        }
    }
    if left.is_empty() {
        return Err("no audio frames decoded".to_string());
    }
    Ok((DecodedTrack { left, right }, sample_rate, channels))
}

/// Offline stereo resample `source_rate` → `target_rate` (rubato FFT, the
/// ADR-0029 crate). Fixed-input blocks; the tail is zero-padded through, and the
/// output is trimmed by the resampler's delay to the expected length so the
/// track neither shifts nor grows.
fn resample(
    left: &[f32],
    right: &[f32],
    source_rate: u32,
    target_rate: u32,
) -> Result<DecodedTrack, String> {
    let mut resampler = rubato::Fft::<f32>::new(
        source_rate as usize,
        target_rate as usize,
        RESAMPLE_CHUNK_FRAMES,
        2,
        2,
        FixedSync::Input,
    )
    .map_err(|e| format!("cannot resample {source_rate} Hz audio: {e}"))?;
    let frames_in = left.len();
    let expected_frames =
        (frames_in as f64 * target_rate as f64 / source_rate as f64).round() as usize;
    let delay = resampler.output_delay();

    let mut input = vec![0.0f32; RESAMPLE_CHUNK_FRAMES * 2];
    let mut output = vec![0.0f32; resampler.output_frames_max() * 2];
    let mut produced: Vec<f32> = Vec::with_capacity((expected_frames + delay) * 2 + output.len());
    let mut fed = 0usize;
    // Feed real frames, then zeros, until the delay-shifted tail is out.
    while produced.len() / 2 < expected_frames + delay {
        for i in 0..RESAMPLE_CHUNK_FRAMES {
            let frame = fed + i;
            let (l, r) = if frame < frames_in {
                (left[frame], right[frame])
            } else {
                (0.0, 0.0)
            };
            input[2 * i] = l;
            input[2 * i + 1] = r;
        }
        fed += RESAMPLE_CHUNK_FRAMES;
        let block_out = {
            let inp = InterleavedSlice::new(&input, 2, RESAMPLE_CHUNK_FRAMES)
                .map_err(|e| format!("resample input: {e}"))?;
            let mut outp =
                InterleavedSlice::new_mut(&mut output, 2, resampler.output_frames_max())
                    .map_err(|e| format!("resample output: {e}"))?;
            let (_, out_frames) = resampler
                .process_into_buffer(&inp, &mut outp, None)
                .map_err(|e| format!("resample: {e}"))?;
            out_frames
        };
        produced.extend_from_slice(&output[..block_out * 2]);
    }

    let start = delay * 2;
    let end = start + expected_frames * 2;
    let trimmed = &produced[start..end.min(produced.len())];
    let mut out_left = Vec::with_capacity(expected_frames);
    let mut out_right = Vec::with_capacity(expected_frames);
    for frame in trimmed.chunks_exact(2) {
        out_left.push(frame[0]);
        out_right.push(frame[1]);
    }
    Ok(DecodedTrack {
        left: out_left,
        right: out_right,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialise a 16-bit PCM WAV in memory (hound — the corpus harness dep).
    fn wav_bytes(rate: u32, channels: u16, frames: usize, hz: f64) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(&mut cursor, spec).expect("wav writer");
        for i in 0..frames {
            let sample = ((2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64).sin()
                * 0.5
                * i16::MAX as f64) as i16;
            for _ in 0..channels {
                writer.write_sample(sample).expect("wav sample");
            }
        }
        writer.finalize().expect("wav finalize");
        cursor.into_inner()
    }

    #[test]
    fn decodes_a_48k_wav_without_resampling() {
        let frames = 48_000;
        let bytes = wav_bytes(48_000, 2, frames, 440.0);
        let decoded = decode_to_48k(bytes, Some("wav")).expect("decodes");
        assert_eq!(decoded.frames(), frames);
        assert!((decoded.duration_seconds() - 1.0).abs() < 1e-9);
        // Content survived: a 440 Hz half-scale sine has RMS ≈ 0.35.
        let rms = (decoded.left.iter().map(|s| (*s as f64).powi(2)).sum::<f64>()
            / frames as f64)
            .sqrt();
        assert!((rms - 0.35).abs() < 0.02, "rms {rms}");
    }

    #[test]
    fn resamples_a_44100_wav_to_48k() {
        let frames = 44_100 * 2;
        let bytes = wav_bytes(44_100, 2, frames, 440.0);
        let decoded = decode_to_48k(bytes, Some("wav")).expect("decodes");
        // 2 s of audio at the target rate, to the frame.
        assert_eq!(decoded.frames(), 96_000);
        // The tone survives the resample (same RMS, no silence padding drift).
        let mid = &decoded.left[24_000..72_000];
        let rms =
            (mid.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / mid.len() as f64).sqrt();
        assert!((rms - 0.35).abs() < 0.02, "rms {rms}");
    }

    #[test]
    fn duplicates_a_mono_source_to_both_channels() {
        let bytes = wav_bytes(48_000, 1, 4_800, 220.0);
        let decoded = decode_to_48k(bytes, Some("wav")).expect("decodes");
        assert_eq!(decoded.left, decoded.right);
    }

    #[test]
    fn source_clip_cuts_a_window_bit_exactly() {
        let bytes = wav_bytes(44_100, 2, 44_100 * 3, 440.0);
        let clip = source_clip(bytes.clone(), Some("wav"), 1.0, Some(2.0)).expect("clips");
        assert!((clip.seconds - 1.0).abs() < 1e-9);
        // Same 44-byte header shape, and the PCM is the source's second second.
        assert_eq!(
            &clip.wav[44..],
            &bytes[44 + 44_100 * 4..44 + 2 * 44_100 * 4]
        );
        // An end past the file clamps; a start past it refuses.
        let tail = source_clip(bytes.clone(), Some("wav"), 2.5, Some(99.0)).expect("clamps");
        assert!((tail.seconds - 0.5).abs() < 1e-9);
        assert!(source_clip(bytes.clone(), Some("wav"), 3.5, None).is_err());
        assert!(source_clip(bytes, Some("wav"), 1.0, Some(1.0)).is_err());
    }

    #[test]
    fn source_clip_resamples_and_widens_mono() {
        let clip = source_clip(wav_bytes(48_000, 1, 48_000, 220.0), Some("wav"), 0.0, None)
            .expect("clips");
        assert_eq!(clip.wav.len(), 44 + 44_100 * 4);
        let spec = hound::WavReader::new(Cursor::new(&clip.wav))
            .expect("wav")
            .spec();
        assert_eq!(
            (spec.channels, spec.sample_rate, spec.bits_per_sample),
            (2, 44_100, 16)
        );
    }

    #[test]
    fn crop_frames_maps_seconds_and_refuses_bad_windows() {
        assert_eq!(crop_frames(1.0, 2.0, 100, 300), Ok((100, 200)));
        // The end handle at the (rounded) duration clamps by a frame, no more.
        assert_eq!(crop_frames(0.0, 3.005, 100, 300), Ok((0, 300)));
        assert!(crop_frames(0.0, 3.5, 100, 300).is_err());
        assert!(crop_frames(2.0, 2.0, 100, 300).is_err());
        assert!(crop_frames(2.0, 1.0, 100, 300).is_err());
        assert!(crop_frames(-1.0, 1.0, 100, 300).is_err());
        assert!(crop_frames(f64::NAN, 1.0, 100, 300).is_err());
        assert!(crop_frames(0.0, f64::INFINITY, 100, 300).is_err());
    }

    #[test]
    fn crop_gain_fades_both_cuts_to_silence() {
        assert_eq!(crop_gain(0, 100, 10), 0.0);
        assert_eq!(crop_gain(99, 100, 10), 0.0);
        assert_eq!(crop_gain(5, 100, 10), 0.5);
        assert_eq!(crop_gain(50, 100, 10), 1.0);
        assert_eq!(crop_gain(0, 1, 0), 1.0);
    }

    #[test]
    fn crop_wav_keeps_rate_and_channels_and_cuts_the_window() {
        let bytes = wav_bytes(44_100, 2, 44_100 * 3, 440.0);
        let crop = crop_wav(bytes.clone(), Some("wav"), 1.0, 2.0).expect("crops");
        let mut reader = hound::WavReader::new(Cursor::new(&crop)).expect("wav");
        let spec = reader.spec();
        assert_eq!((spec.channels, spec.sample_rate, spec.bits_per_sample), (2, 44_100, 16));
        let samples: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(samples.len(), 44_100 * 2);
        // Silent at both cuts, the source itself past the fade.
        assert_eq!(&samples[..2], &[0, 0]);
        assert_eq!(&samples[samples.len() - 2..], &[0, 0]);
        let body = 1_000 * 2;
        assert_eq!(
            &crop[44 + body * 2..44 + body * 2 + 20],
            &bytes[44 + (44_100 * 2 + body) * 2..44 + (44_100 * 2 + body) * 2 + 20]
        );
        let mono = crop_wav(wav_bytes(48_000, 1, 48_000, 220.0), Some("wav"), 0.0, 0.5)
            .expect("crops mono");
        let spec = hound::WavReader::new(Cursor::new(&mono)).expect("wav").spec();
        assert_eq!((spec.channels, spec.sample_rate), (1, 48_000));
        assert!(crop_wav(bytes, Some("wav"), 2.0, 9.0).is_err());
    }

    #[test]
    fn clip_peaks_spans_the_file() {
        let (min, max, seconds) =
            clip_peaks(wav_bytes(48_000, 2, 48_000, 440.0), Some("wav"), 10).expect("peaks");
        assert_eq!((min.len(), max.len()), (10, 10));
        assert!((seconds - 1.0).abs() < 1e-9);
        assert!(max.iter().all(|peak| *peak > 0.4) && min.iter().all(|peak| *peak < -0.4));
    }

    #[test]
    #[ignore = "timing diagnostic — run with --ignored --nocapture"]
    fn timing_full_load_pipeline() {
        use std::time::Instant;
        let samples = crate::analysis::beat::fixtures::click_track(128.0, 300.0, 44_100.0, 1);
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(&mut cursor, spec).expect("wav writer");
        for s in &samples {
            let clamped = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            writer.write_sample(clamped).expect("wav sample");
        }
        writer.finalize().expect("wav finalize");
        let bytes = cursor.into_inner();

        let t = Instant::now();
        let decoded = decode_to_48k(bytes, Some("wav")).expect("decodes");
        eprintln!("decode+resample (5 min @ 44.1k): {:?}", t.elapsed());
        let t = Instant::now();
        let coarse = crate::analysis::beat::track_bpm(&decoded.left, &decoded.right, 48_000.0);
        eprintln!("track_bpm: {:?} -> {coarse:?}", t.elapsed());
        let t = Instant::now();
        let grid =
            crate::analysis::grid::track_beatgrid(&decoded.left, &decoded.right, 48_000.0, coarse);
        eprintln!("beatgrid: {:?} -> {:?}", t.elapsed(), grid.map(|g| g.bpm));
        let t = Instant::now();
        let bands = crate::analysis::bands::track_bands(&decoded.left, &decoded.right, 48_000.0);
        eprintln!("bands: {:?} ({} hops)", t.elapsed(), bands.hops());
        let t = Instant::now();
        let interleaved = decoded.interleaved();
        eprintln!("interleave: {:?} ({} samples)", t.elapsed(), interleaved.len());
    }

    #[test]
    fn refuses_bytes_that_are_not_audio() {
        let err = match decode_to_48k(b"definitely not audio".to_vec(), None) {
            Err(err) => err,
            Ok(_) => panic!("must refuse non-audio bytes"),
        };
        assert!(err.contains("unsupported"), "{err}");
    }
}
