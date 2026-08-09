//! The cpal device wrapper: a thin host around the device-free [`Engine`] core.
//!
//! Opens an output stream with `BufferSize::Fixed(256)` and, in its
//! callback, sets FTZ/DAZ once and drains the engine's output ring(s) wrapped in
//! `assert_no_alloc`. The callback is the ONLY real-time path; it allocates
//! nothing, takes no lock, makes no syscall, and logs nothing. Ported from the
//! Spike A `rt_engine` device half (`spike/rust-audio/engine/src/bin/rt_engine.rs`),
//! now built on the library so the device path stays exercisable.
//!
//! The engine renders at exactly [`SAMPLE_RATE`] (48000). A device that offers a
//! 48000 config in a supported sample format (`f32`, `i16`, or `u16`) is opened
//! there. A device with no 48000 config — e.g. a 44100 Bluetooth speaker — is
//! opened at its own rate and the 48 kHz stream is resampled to it on the callback
//! via [`OutputResampler`] (ADR-0029). Format conversion, clipping, and device
//! channel mapping happen only at the final callback boundary; the host's output
//! ring stays a clean 48 kHz interleaved-stereo `f32` contract.
//!
//! Graceful no-device exit: if no output device or no usable config is
//! available (likely in a sandbox / headless CI), [`run_stream`] returns
//! [`DeviceError::Unavailable`] rather than hanging or panicking.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, StreamConfig};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::host::OutputConsumer;
use crate::{Engine, CHANNELS, SAMPLE_RATE};

/// Requested device buffer size (frames). Clamped to the device's supported
/// range; the granted size is reported back in [`StreamInfo`].
const REQUESTED_BUFFER: u32 = 256;

/// Why a device stream could not be opened. `Unavailable` is the sandbox/headless
/// case — callers treat it as "no device, exit cleanly", not a failure.
#[derive(Debug)]
pub enum DeviceError {
    /// No output device, or no usable config at all (e.g. a sandbox). A
    /// non-48000 device is NOT this case anymore — it is opened and resampled
    /// (ADR-0029). Not a bug — exit cleanly.
    Unavailable(String),
    /// The stream could not be built or started.
    Stream(String),
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::Unavailable(m) => write!(f, "audio device unavailable: {m}"),
            DeviceError::Stream(m) => write!(f, "audio stream error: {m}"),
        }
    }
}

impl std::error::Error for DeviceError {}

/// What the device granted, for logging / telemetry.
#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub device_name: String,
    pub device_channels: u16,
    pub sample_rate: u32,
    pub sample_format: cpal::SampleFormat,
    pub buffer_frames: BufferSize,
}

/// A cloneable, allocation-free health signal shared with CPAL's error callback.
/// The signal is deliberately one-way: a failed stream is replaced, never reset.
#[derive(Clone)]
struct StreamHealth(Arc<AtomicBool>);

impl StreamHealth {
    fn healthy() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    fn mark_failed(&self) {
        self.0.store(false, Ordering::Release);
    }

    fn is_healthy(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A running output stream driving an [`Engine`]. The cpal stream stops when this
/// is dropped; the `Engine` lives inside the callback for the stream's lifetime.
pub struct AudioStream {
    _stream: cpal::Stream,
    info: StreamInfo,
    /// CPAL invokes its error callback on a backend-owned thread. Keep that path
    /// bounded and non-blocking: it flips this preallocated atomic and does no
    /// logging, allocation, locking, or application IPC. The shell polls the
    /// value off the real-time path and owns user-facing diagnostics/recovery.
    healthy: StreamHealth,
}

impl AudioStream {
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Whether CPAL has reported an asynchronous error since this stream was
    /// started. A newly opened stream is healthy; recovery replaces the stream
    /// (and therefore this signal) rather than trying to reset it in place.
    pub fn is_healthy(&self) -> bool {
        self.healthy.is_healthy()
    }
}

/// One output device the engine can open, for the picker UI.
pub struct OutputDeviceInfo {
    pub name: String,
    /// Channels of its chosen usable (`f32`, `i16`, or `u16`) config — the widest
    /// preferred 48000 config, or the fallback when the device cannot do 48000.
    pub channels: u16,
    /// Whether it can carry the headphone cue: a ≥4-channel device lands master
    /// on 1/2 and the cue on 3/4 (the FLX4 phones jack).
    pub cue_capable: bool,
}

/// This device's name, or `<unknown>`.
fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "<unknown>".into())
}

/// The device-boundary sample formats the engine supports. The engine and output
/// rings remain `f32`; integer support is deliberately confined to the final,
/// allocation-free callback conversion.
fn sample_format_rank(format: cpal::SampleFormat) -> Option<u8> {
    match format {
        cpal::SampleFormat::F32 => Some(0),
        cpal::SampleFormat::I16 => Some(1),
        cpal::SampleFormat::U16 => Some(2),
        _ => None,
    }
}

/// Choose a device's output config for the engine. Preference order:
///
/// 1. An exact 48000 config, preferring the widest channel layout and then `f32`,
///    `i16`, and `u16` within that layout. A ≥4-channel device (the FLX4) lands
///    master on 1/2 and cue on 3/4; a mono device receives a balanced downmix.
/// 2. Otherwise the device's own default config, when its sample format is
///    supported — its nominal rate (e.g. 44100 for a Bluetooth speaker), which
///    the OS will not itself resample, so we resample 48000 → it directly.
/// 3. Otherwise any supported config, at the supported rate NEAREST 48000
///    (widest channels, then `f32`/`i16`/`u16`, as tie-breaks).
///
/// The returned config's `sample_rate()` is the rate the stream opens at; the
/// caller resamples when it is not [`SAMPLE_RATE`].
fn pick_config(device: &cpal::Device) -> Option<cpal::SupportedStreamConfig> {
    let exact = device.supported_output_configs().ok().and_then(|configs| {
        configs
            .filter(|cfg| {
                cfg.channels() > 0
                    && sample_format_rank(cfg.sample_format()).is_some()
                    && cfg.min_sample_rate() <= SAMPLE_RATE
                    && cfg.max_sample_rate() >= SAMPLE_RATE
            })
            .min_by_key(|cfg| {
                (
                    u16::MAX - cfg.channels(),
                    sample_format_rank(cfg.sample_format()).unwrap_or(u8::MAX),
                )
            })
            .map(|cfg| cfg.with_sample_rate(SAMPLE_RATE))
    });
    if exact.is_some() {
        return exact;
    }

    // No exact 48000 config: fall back to a resampled rate. Prefer the device's own
    // default (its nominal rate, so the OS does not double-resample under us).
    if let Ok(default) = device.default_output_config() {
        if default.channels() > 0 && sample_format_rank(default.sample_format()).is_some() {
            return Some(default);
        }
    }

    // Last resort: the supported config whose range lands nearest 48000. `clamp`
    // gives the nearest in-range rate — for a 44100-only device that is 44100.
    device.supported_output_configs().ok().and_then(|configs| {
        configs
            .filter(|cfg| cfg.channels() > 0 && sample_format_rank(cfg.sample_format()).is_some())
            .min_by_key(|cfg| {
                let rate = SAMPLE_RATE.clamp(cfg.min_sample_rate(), cfg.max_sample_rate());
                (
                    rate.abs_diff(SAMPLE_RATE),
                    u16::MAX - cfg.channels(),
                    sample_format_rank(cfg.sample_format()).unwrap_or(u8::MAX),
                )
            })
            .map(|cfg| {
                let rate = SAMPLE_RATE.clamp(cfg.min_sample_rate(), cfg.max_sample_rate());
                cfg.with_sample_rate(rate)
            })
    })
}

/// Enumerate the output devices the engine can open (any `f32`, `i16`, or `u16`
/// config — exact 48000 or a resampled fallback) with their chosen channel count,
/// for the picker. Off the RT path — called from a command when the picker opens.
/// Empty on a headless host.
pub fn list_output_devices() -> Vec<OutputDeviceInfo> {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for device in devices {
        if let Some(cfg) = pick_config(&device) {
            let channels = cfg.channels();
            out.push(OutputDeviceInfo {
                name: device_name(&device),
                channels,
                cue_capable: channels >= 4,
            });
        }
    }
    out
}

/// Find an output device by its reported name; errors if none matches (a saved
/// device may be unplugged) so the caller keeps the current stream.
fn find_output_device(host: &cpal::Host, name: &str) -> Result<cpal::Device, DeviceError> {
    let devices = host
        .output_devices()
        .map_err(|e| DeviceError::Unavailable(format!("cannot enumerate output devices: {e}")))?;
    devices
        .into_iter()
        .find(|d| device_name(d) == name)
        .ok_or_else(|| DeviceError::Unavailable(format!("output device '{name}' not found")))
}

/// Open `device_name` (or the default when `None`) at the config [`pick_config`]
/// chooses — a preferred 48000 config (so the cue reaches channels 3/4 on a
/// ≥4-channel device), or a resampled fallback rate. `info.sample_rate` and
/// `info.sample_format` describe what the stream actually opens.
fn open_output(
    selected: Option<&str>,
) -> Result<(cpal::Device, StreamConfig, StreamInfo), DeviceError> {
    let host = cpal::default_host();
    let device = match selected {
        Some(name) => find_output_device(&host, name)?,
        None => host
            .default_output_device()
            .ok_or_else(|| DeviceError::Unavailable("no default output device".into()))?,
    };

    let device_name = device_name(&device);

    let supported = pick_config(&device).ok_or_else(|| {
        DeviceError::Unavailable(format!(
            "device '{device_name}' has no usable output config \
             (supported sample formats: f32, i16, u16)"
        ))
    })?;

    let device_channels = supported.channels();
    let device_rate = supported.sample_rate();
    let sample_format = supported.sample_format();
    let buffer_size = match supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => {
            BufferSize::Fixed(REQUESTED_BUFFER.clamp(*min, *max))
        }
        cpal::SupportedBufferSize::Unknown => BufferSize::Fixed(REQUESTED_BUFFER),
    };

    let config = StreamConfig {
        channels: device_channels,
        sample_rate: device_rate,
        buffer_size,
    };

    let info = StreamInfo {
        device_name,
        device_channels,
        sample_rate: device_rate,
        sample_format,
        buffer_frames: buffer_size,
    };

    Ok((device, config, info))
}

/// A sample type accepted at the cpal device boundary. Conversions explicitly
/// clamp before quantization: `dasp_sample`'s raw `f32 → i16` conversion wraps at
/// exactly `1.0`, which is not acceptable when a limiter or resampler reaches the
/// positive endpoint. Implementations are branch/arithmetic only and RT-safe.
trait DeviceSample: cpal::SizedSample + Copy + Send + 'static {
    fn from_f32_clipped(sample: f32) -> Self;
}

/// Clamp finite and infinite values to the cpal PCM domain. A NaN is silence:
/// propagating it to a float device can poison downstream host processing, while
/// integer casts happen to turn it into zero and would otherwise disagree.
#[inline]
fn clip_sample(sample: f32) -> f32 {
    if sample.is_nan() {
        0.0
    } else {
        sample.clamp(-1.0, 1.0)
    }
}

impl DeviceSample for f32 {
    #[inline]
    fn from_f32_clipped(sample: f32) -> Self {
        clip_sample(sample)
    }
}

impl DeviceSample for i16 {
    #[inline]
    fn from_f32_clipped(sample: f32) -> Self {
        let sample = clip_sample(sample);
        if sample <= -1.0 {
            i16::MIN
        } else if sample >= 1.0 {
            i16::MAX
        } else {
            (sample * 32_768.0).round() as i16
        }
    }
}

impl DeviceSample for u16 {
    #[inline]
    fn from_f32_clipped(sample: f32) -> Self {
        let sample = clip_sample(sample);
        if sample <= -1.0 {
            u16::MIN
        } else if sample >= 1.0 {
            u16::MAX
        } else {
            ((sample + 1.0) * 32_767.5).round() as u16
        }
    }
}

/// Silence an interleaved device buffer, then map each `(channel_offset, src)`
/// interleaved-stereo `f32` block into it while clipping and converting to the
/// device sample type. Master on 1/2 is `&[(0, master)]`; the FLX4 combined path
/// is `&[(0, master), (2, cue)]`; a split FLX4 cue is `&[(2, cue)]`.
///
/// A mono device gets `(left + right) / 2`, which preserves headroom and avoids
/// selecting one side of stereo content. A frame past a source's length (or an
/// offset past the device) stays silent. `placements` is a fixed stack slice, so
/// this is allocation/lock/syscall/log free and safe in the realtime callback.
fn write_mapped<T: DeviceSample>(data: &mut [T], dev_ch: usize, placements: &[(usize, &[f32])]) {
    let silence = T::from_f32_clipped(0.0);
    data.fill(silence);
    if dev_ch == 0 {
        return;
    }

    let frames = data.len() / dev_ch;
    for f in 0..frames {
        for &(offset, src) in placements {
            let src_base = 2 * f;
            if src_base + 1 >= src.len() {
                continue;
            }
            if dev_ch == 1 && offset == 0 {
                let mono = (src[src_base] + src[src_base + 1]) * 0.5;
                data[f] = T::from_f32_clipped(mono);
            } else if offset + 1 < dev_ch {
                let dst_base = f * dev_ch + offset;
                data[dst_base] = T::from_f32_clipped(src[src_base]);
                data[dst_base + 1] = T::from_f32_clipped(src[src_base + 1]);
            }
        }
    }
}

/// Number of overlapping FFT blocks rubato's synchronous resampler uses — the
/// usual real-time choice (more blocks = marginally better stopband for more
/// latency; 2 keeps the added delay to a few ms, dwarfed by the output ring).
const RESAMPLER_BLOCKS: usize = 2;

/// Resamples the engine's 48 kHz interleaved-stereo feed to a device with no
/// usable 48000 config (e.g. a 44100 Bluetooth speaker), via rubato's synchronous
/// FFT resampler at the fixed [`SAMPLE_RATE`] → device-rate ratio (ADR-0029).
///
/// rubato works in fixed `chunk_frames` blocks; the cpal callback's block size is
/// whatever CoreAudio hands that call (usually the requested `Fixed`, but Bluetooth
/// does not always honour it). [`fill`](Self::fill) decouples the two with a small
/// `carry` FIFO: it serves leftover resampled samples first, then resamples as many
/// `chunk_frames` blocks as the callback needs and stashes the unused tail of the
/// last block for next time. So any block size is served exactly — never a silent
/// zeroed tail or dropped frame.
///
/// Built OFF the RT path ([`OutputResampler::new`] allocates the FFT plans and
/// buffers). The callback only calls [`fill`](Self::fill), which is alloc-free:
/// [`OutputConsumer::drain_into`] (wait-free, zero-pads + counts an underrun on a
/// short ring) plus rubato `process_into_buffer` (documented alloc-free) into
/// pre-sized buffers. One instance per feed (the master, and the cue when combined
/// on a ≥4-channel device).
struct OutputResampler {
    resampler: Fft<f32>,
    /// Interleaved-stereo input scratch, sized to the resampler's worst-case input
    /// (`input_frames_max`). Filled from the ring each resampled block.
    input: Vec<f32>,
    /// One resampled block at the device rate (interleaved stereo), `chunk_frames`.
    chunk: Vec<f32>,
    /// FIFO of resampled samples produced but not yet handed to the device, kept at
    /// the front `[..carry_len]`. Always shorter than one `chunk`, so a buffer of
    /// `chunk_frames` capacity is enough.
    carry: Vec<f32>,
    /// Valid samples in `carry`.
    carry_len: usize,
    /// Device-rate frames rubato produces per block (`FixedSync::Output`).
    chunk_frames: usize,
}

impl OutputResampler {
    /// Build a [`SAMPLE_RATE`] → `device_rate` stereo resampler working in
    /// `chunk_frames`-frame blocks. `None` if rubato rejects the rates. Allocates —
    /// call OFF the RT path.
    fn new(device_rate: u32, chunk_frames: usize) -> Option<Self> {
        let resampler = Fft::<f32>::new(
            SAMPLE_RATE as usize,
            device_rate as usize,
            chunk_frames,
            CHANNELS as usize,
            RESAMPLER_BLOCKS,
            FixedSync::Output,
        )
        .ok()?;
        let input = vec![0.0; resampler.input_frames_max() * CHANNELS as usize];
        let chunk = vec![0.0; chunk_frames * CHANNELS as usize];
        let carry = vec![0.0; chunk_frames * CHANNELS as usize];
        Some(OutputResampler {
            resampler,
            input,
            chunk,
            carry,
            carry_len: 0,
            chunk_frames,
        })
    }

    /// **RT path.** Fill `out` (interleaved-stereo at the device rate) entirely,
    /// from the `carry` FIFO plus freshly resampled blocks. `out.len()` may be any
    /// even length — it need not equal `chunk_frames` (the FIFO absorbs the
    /// difference). Alloc-free.
    fn fill(&mut self, src: &mut OutputConsumer, out: &mut [f32]) {
        let mut written = 0;
        // Serve carried-over samples from the previous call first.
        if self.carry_len > 0 {
            let n = self.carry_len.min(out.len());
            out[..n].copy_from_slice(&self.carry[..n]);
            self.carry.copy_within(n..self.carry_len, 0);
            self.carry_len -= n;
            written = n;
        }
        // Resample fresh blocks until `out` is full; stash any tail of the last one.
        while written < out.len() {
            let produced = self.produce_chunk(src);
            let n = (out.len() - written).min(produced);
            out[written..written + n].copy_from_slice(&self.chunk[..n]);
            written += n;
            if n < produced {
                self.carry[..produced - n].copy_from_slice(&self.chunk[n..produced]);
                self.carry_len = produced - n;
            }
        }
    }

    /// Drain the next input block from `src` and resample it into `self.chunk`,
    /// returning the sample count produced (`chunk_frames * CHANNELS`). On a
    /// size-invariant rubato error (should never happen — buffers are sized to
    /// `input_frames_max`/`chunk_frames`) the block is silenced.
    fn produce_chunk(&mut self, src: &mut OutputConsumer) -> usize {
        // How many input frames rubato wants next (varies as 48000/device_rate is
        // not integer); drain exactly that — `drain_into` zero-pads + counts an
        // underrun on a short ring.
        let n_in = self.resampler.input_frames_next();
        src.drain_into(&mut self.input[..n_in * CHANNELS as usize]);
        if !self.resample_chunk(n_in) {
            self.chunk.iter_mut().for_each(|s| *s = 0.0);
        }
        self.chunk.len()
    }

    /// Resample the `n_in` interleaved-stereo frames already in `self.input` into
    /// `self.chunk`. Returns whether rubato succeeded. The drain-free half of
    /// [`produce_chunk`](Self::produce_chunk), split out so the resampling can be
    /// unit-tested on a directly-filled `input`. Alloc-free.
    fn resample_chunk(&mut self, n_in: usize) -> bool {
        // Disjoint field borrows (input / chunk / resampler) confined to here.
        let input = InterleavedSlice::new(
            &self.input[..n_in * CHANNELS as usize],
            CHANNELS as usize,
            n_in,
        );
        let output =
            InterleavedSlice::new_mut(&mut self.chunk[..], CHANNELS as usize, self.chunk_frames);
        match (input, output) {
            (Ok(inp), Ok(mut outp)) => self
                .resampler
                .process_into_buffer(&inp, &mut outp, None)
                .is_ok(),
            _ => false,
        }
    }
}

/// The complete mutable state captured by a production cpal output callback.
/// Constructed before stream start; its vectors never resize after capture.
struct OutputWriter {
    device_channels: usize,
    primary_offset: usize,
    primary: OutputConsumer,
    secondary: Option<OutputConsumer>,
    primary_resampler: Option<OutputResampler>,
    secondary_resampler: Option<OutputResampler>,
    scratch: Vec<f32>,
    secondary_scratch: Vec<f32>,
}

impl OutputWriter {
    /// **RT path.** Drain/resample and convert an entire cpal callback in bounded,
    /// frame-aligned tiles. The scratch buffers are reusable working space, not a
    /// limit on callback length: hosts may deliver a block larger than the granted
    /// size, and every frame still consumes its matching ring input. Iteration is
    /// arithmetic over preallocated slices only.
    fn write<T: DeviceSample>(&mut self, data: &mut [T]) {
        let silence = T::from_f32_clipped(0.0);
        let dev_ch = self.device_channels;
        if dev_ch == 0 {
            data.fill(silence);
            return;
        }

        let primary_scratch_frames = self.scratch.len() / CHANNELS as usize;
        let tile_frames = if self.secondary.is_some() {
            primary_scratch_frames.min(self.secondary_scratch.len() / CHANNELS as usize)
        } else {
            primary_scratch_frames
        };
        if tile_frames == 0 {
            data.fill(silence);
            return;
        }

        let total_frames = data.len() / dev_ch;
        let mut frame_start = 0;
        while frame_start < total_frames {
            let frames = (total_frames - frame_start).min(tile_frames);
            let device_start = frame_start * dev_ch;
            let device_end = device_start + frames * dev_ch;
            let stereo_samples = frames * CHANNELS as usize;
            let primary_tile = &mut self.scratch[..stereo_samples];

            if let Some(resampler) = self.primary_resampler.as_mut() {
                resampler.fill(&mut self.primary, primary_tile);
            } else {
                self.primary.drain_into(primary_tile);
            }

            if let Some(secondary) = self.secondary.as_mut() {
                let secondary_tile = &mut self.secondary_scratch[..stereo_samples];
                if let Some(resampler) = self.secondary_resampler.as_mut() {
                    resampler.fill(secondary, secondary_tile);
                } else {
                    secondary.drain_into(secondary_tile);
                }
                write_mapped(
                    &mut data[device_start..device_end],
                    dev_ch,
                    &[(0, primary_tile), (2, secondary_tile)],
                );
            } else {
                write_mapped(
                    &mut data[device_start..device_end],
                    dev_ch,
                    &[(self.primary_offset, primary_tile)],
                );
            }
            frame_start += frames;
        }

        // cpal supplies whole frames, but make a malformed trailing partial frame
        // deterministic and silent without reading another engine frame.
        data[total_frames * dev_ch..].fill(silence);
    }
}

/// **RT path.** The legacy engine-in-callback exerciser uses the same bounded
/// tiling rule as [`OutputWriter::write`], rendering every callback frame even
/// when cpal hands it a block larger than the preallocated scratch.
fn render_engine_chunks<T: DeviceSample>(
    data: &mut [T],
    dev_ch: usize,
    engine: &mut Engine,
    scratch: &mut [f32],
) {
    let silence = T::from_f32_clipped(0.0);
    if dev_ch == 0 {
        data.fill(silence);
        return;
    }
    let tile_frames = scratch.len() / CHANNELS as usize;
    if tile_frames == 0 {
        data.fill(silence);
        return;
    }

    let total_frames = data.len() / dev_ch;
    let mut frame_start = 0;
    while frame_start < total_frames {
        let frames = (total_frames - frame_start).min(tile_frames);
        let device_start = frame_start * dev_ch;
        let device_end = device_start + frames * dev_ch;
        let stereo_samples = frames * CHANNELS as usize;
        let tile = &mut scratch[..stereo_samples];
        engine.render(tile, frames);
        write_mapped(&mut data[device_start..device_end], dev_ch, &[(0, tile)]);
        frame_start += frames;
    }
    data[total_frames * dev_ch..].fill(silence);
}

/// Open `selected` (a supported 48000 config, or a resampled fallback rate), build a
/// stream that drains `primary` onto channels 1/2 — and, when `secondary` is
/// `Some` AND the device has ≥4 channels,
/// also drains it onto channels 3/4 (the FLX4 combined master+cue path). On a
/// narrower device the `secondary` is dropped: the stream is primary-only and the
/// secondary ring stays undrained (its `push_all` overflow is discarded, so the
/// render thread never stalls). Start it and return the running stream.
///
/// `primary_on_phones` flips the lone-primary placement onto channels 3/4 when the
/// device has ≥4 channels — the FLX4 chosen as a SEPARATE cue device, whose phones
/// jack is 3/4 (its 1/2 is the MASTER RCA). It is ignored when `secondary` is set
/// (the combined path already owns 1/2 and 3/4) or on a stereo device.
///
/// The callback is the ONLY real-time path: it sets FTZ/DAZ once, drains the
/// ring(s), resamples to the device rate when one is needed, and spreads into the
/// device buffer, all under `assert_no_alloc` — alloc/lock/syscall free (rubato's
/// `process_into_buffer` and the ring drains are all alloc-free). The [`Engine`]
/// renders at 48 kHz on the host's dedicated render thread into the rings; the
/// callback only pulls from them (see [`crate::host`] for the decoupled-render-
/// thread rationale and latency note).
///
/// On any sandbox/headless condition this returns [`DeviceError::Unavailable`]
/// without hanging — the host keeps running headlessly (its render thread fills
/// the rings; with no device nothing drains them, which is fine).
fn open_spread_stream(
    selected: Option<&str>,
    primary: OutputConsumer,
    secondary: Option<OutputConsumer>,
    primary_on_phones: bool,
) -> Result<AudioStream, DeviceError> {
    let (device, config, info) = open_output(selected)?;
    match info.sample_format {
        cpal::SampleFormat::F32 => {
            build_spread_stream::<f32>(device, config, info, primary, secondary, primary_on_phones)
        }
        cpal::SampleFormat::I16 => {
            build_spread_stream::<i16>(device, config, info, primary, secondary, primary_on_phones)
        }
        cpal::SampleFormat::U16 => {
            build_spread_stream::<u16>(device, config, info, primary, secondary, primary_on_phones)
        }
        format => Err(DeviceError::Unavailable(format!(
            "selected unsupported output sample format {format}"
        ))),
    }
}

/// Typed half of [`open_spread_stream`]. The sample-format dispatch happens once,
/// before cpal starts the stream; every callback then drains/resamples into fixed
/// `f32` scratch and performs only channel mapping plus scalar conversion.
fn build_spread_stream<T: DeviceSample>(
    device: cpal::Device,
    config: StreamConfig,
    info: StreamInfo,
    primary: OutputConsumer,
    secondary: Option<OutputConsumer>,
    primary_on_phones: bool,
) -> Result<AudioStream, DeviceError> {
    let device_channels = info.device_channels as usize;

    // The secondary (cue) feed needs channels 3/4 — only a ≥4-channel device (the
    // FLX4) can carry it alongside the primary. Drop it on a narrower device.
    let secondary = if device_channels >= 4 {
        secondary
    } else {
        None
    };
    let secondary_routed = secondary.is_some();
    // Where the primary lands: a standalone cue stream on a ≥4-channel device (the
    // FLX4 chosen as a SEPARATE cue device) belongs on the phones channels 3/4
    // (offset 2), not 1/2 (its MASTER RCA). Master, and a cue on a stereo device
    // (laptop jack, Bluetooth), land on 1/2 (offset 0).
    let primary_offset = if primary_on_phones && device_channels >= 4 {
        2
    } else {
        0
    };

    // When the device opened at a rate other than the engine's 48 kHz, build a
    // resampler per feed (off the RT path; the callback only `fill`s them). A
    // failure here is fatal — playing 48 kHz audio straight into a 44.1 kHz buffer
    // would be pitched wrong — so it surfaces as a stream error. The resampler's
    // chunk granularity is the granted buffer; its FIFO decouples that from the
    // actual callback block size, so a varying block is served exactly.
    let device_rate = info.sample_rate;
    let chunk_frames = match info.buffer_frames {
        BufferSize::Fixed(n) => n as usize,
        BufferSize::Default => REQUESTED_BUFFER as usize,
    };
    let build_resampler = |feed: &str| -> Result<OutputResampler, DeviceError> {
        OutputResampler::new(device_rate, chunk_frames).ok_or_else(|| {
            DeviceError::Stream(format!(
                "failed to build {device_rate} Hz {feed} resampler from {SAMPLE_RATE} Hz"
            ))
        })
    };
    let primary_resampler = if device_rate != SAMPLE_RATE {
        Some(build_resampler("master")?)
    } else {
        None
    };
    let secondary_resampler = if device_rate != SAMPLE_RATE && secondary_routed {
        Some(build_resampler("cue")?)
    } else {
        None
    };

    let mut first_call = true;
    // Per-callback f32 scratch: rings and resamplers remain interleaved stereo,
    // regardless of the device's sample format or channel layout. Sized ONCE here,
    // off the RT path, for a generous worst-case block; the callback never resizes.
    let mut scratch: Vec<f32> = Vec::new();
    let mut secondary_scratch: Vec<f32> = Vec::new();
    scratch_reserve(&mut scratch, chunk_frames.saturating_mul(4));
    if secondary_routed {
        scratch_reserve(&mut secondary_scratch, chunk_frames.saturating_mul(4));
    }
    let mut output = OutputWriter {
        device_channels,
        primary_offset,
        primary,
        secondary,
        primary_resampler,
        secondary_resampler,
        scratch,
        secondary_scratch,
    };

    let healthy = StreamHealth::healthy();
    let error_health = healthy.clone();
    let err_fn = move |_error| {
        // This may run on an audio-backend thread. Never format/log/lock/send
        // from here: one atomic store is sufficient for the shell's live poll.
        error_health.mark_failed();
    };

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                no_alloc(|| {
                    if first_call {
                        set_ftz_daz();
                        first_call = false;
                    }
                    output.write(data);
                });
            },
            err_fn,
            None,
        )
        .map_err(|e| DeviceError::Stream(format!("failed to build output stream: {e}")))?;

    stream
        .play()
        .map_err(|e| DeviceError::Stream(format!("failed to start stream: {e}")))?;

    Ok(AudioStream {
        _stream: stream,
        info,
        healthy,
    })
}

/// Open the MAIN output device (`main`, or the default when `None`) draining the
/// master ring onto channels 1/2. When `cue` is `Some` (combined mode — the FLX4)
/// the cue ring also drains onto channels 3/4, provided the device has ≥4
/// channels; otherwise the stream is master-only and the cue rides its own stream
/// ([`open_cue_stream`]).
pub fn open_main_stream(
    main: Option<&str>,
    master: OutputConsumer,
    cue: Option<OutputConsumer>,
) -> Result<AudioStream, DeviceError> {
    // Master always on channels 1/2 (the FLX4's MASTER RCA when it is the main
    // device); never on the phones channels.
    open_spread_stream(main, master, cue, false)
}

/// Open the CUE output device (`cue_dev`, or the default when `None`) draining the
/// cue ring — split mode, a second independently chosen device. On a stereo cue
/// device the cue plays out its channels 1/2 (laptop jack, Bluetooth); on a
/// ≥4-channel cue device it plays out channels 3/4 (the FLX4 phones jack, whose
/// 1/2 is the MASTER RCA). Independent of the main stream, so opening / replacing
/// it never disturbs the master.
pub fn open_cue_stream(
    cue_dev: Option<&str>,
    cue: OutputConsumer,
) -> Result<AudioStream, DeviceError> {
    open_spread_stream(cue_dev, cue, None, true)
}

/// Open the default output device at exactly 48000 in a supported sample format,
/// build the stream that renders `engine` in its callback, start it, and return
/// the running stream. The `engine` is MOVED into the audio callback.
///
/// This is the original engine-in-callback path (Phase 1 / `device_run`). The
/// Tauri app now drives audio through [`open_main_stream`] / [`open_cue_stream`] +
/// [`crate::host`] instead, but this path stays for the `device_run` binary and
/// hardware spikes. It renders the engine directly in the callback and does NOT
/// resample, so it requires an exact-48000 device (the app path resamples — see
/// [`open_spread_stream`] / ADR-0029).
///
/// On any sandbox/headless condition (no device, no 48000 config) this returns
/// [`DeviceError::Unavailable`] without hanging — the caller decides whether that
/// is fatal.
pub fn run_stream(engine: Engine) -> Result<AudioStream, DeviceError> {
    let (device, config, info) = open_output(None)?;
    if info.sample_rate != SAMPLE_RATE {
        return Err(DeviceError::Unavailable(format!(
            "default device opened at {} Hz; run_stream needs {SAMPLE_RATE} (it does not resample)",
            info.sample_rate
        )));
    }
    match info.sample_format {
        cpal::SampleFormat::F32 => build_engine_stream::<f32>(device, config, info, engine),
        cpal::SampleFormat::I16 => build_engine_stream::<i16>(device, config, info, engine),
        cpal::SampleFormat::U16 => build_engine_stream::<u16>(device, config, info, engine),
        format => Err(DeviceError::Unavailable(format!(
            "selected unsupported output sample format {format}"
        ))),
    }
}

/// Typed implementation of the legacy engine-in-callback hardware spike. The
/// production app uses [`build_spread_stream`], but keeping this path typed makes
/// the standalone device exerciser work on integer-only hosts too.
fn build_engine_stream<T: DeviceSample>(
    device: cpal::Device,
    config: StreamConfig,
    info: StreamInfo,
    mut engine: Engine,
) -> Result<AudioStream, DeviceError> {
    let device_channels = info.device_channels as usize;

    // The engine renders internal stereo f32 into a pre-sized buffer; typed PCM
    // conversion and channel mapping happen in the same final-boundary primitive
    // as the production ring-drain path. Sized ONCE here, off the RT path.
    let mut first_call = true;
    let mut scratch: Vec<f32> = Vec::new();
    let granted_frames = match info.buffer_frames {
        BufferSize::Fixed(n) => n as usize,
        BufferSize::Default => REQUESTED_BUFFER as usize,
    };
    scratch_reserve(&mut scratch, granted_frames.saturating_mul(4));

    let healthy = StreamHealth::healthy();
    let error_health = healthy.clone();
    let err_fn = move |_error| {
        // Same non-blocking contract as the production spread-stream path.
        error_health.mark_failed();
    };

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                // Everything below MUST be alloc/lock/syscall/log free. The guard
                // proves it (warns in release if violated).
                crate::device::no_alloc(|| {
                    if first_call {
                        crate::device::set_ftz_daz();
                        first_call = false;
                    }
                    render_engine_chunks(data, device_channels, &mut engine, &mut scratch);
                });
            },
            err_fn,
            None,
        )
        .map_err(|e| DeviceError::Stream(format!("failed to build output stream: {e}")))?;

    stream
        .play()
        .map_err(|e| DeviceError::Stream(format!("failed to start stream: {e}")))?;

    Ok(AudioStream {
        _stream: stream,
        info,
        healthy,
    })
}

/// Pre-size the scratch buffer (off the RT path), before it is moved into the
/// callback. Pulled out so the intent — allocate the worst-case block ONCE,
/// never on the RT thread — is explicit.
fn scratch_reserve(scratch: &mut Vec<f32>, frames: usize) {
    scratch.resize(frames.saturating_mul(CHANNELS as usize), 0.0);
}

/// `assert_no_alloc` wrapper, isolated here so `lib.rs`/tests don't depend on the
/// allocator guard. The guard only arms if `AllocDisabler` is the global
/// allocator (registered by the binary); otherwise it is a transparent passthrough.
#[inline]
pub(crate) fn no_alloc<T>(f: impl FnOnce() -> T) -> T {
    assert_no_alloc::assert_no_alloc(f)
}

/// Enable flush-to-zero / denormals-are-zero on the calling (audio) thread so a
/// decaying denormal tail never trips the CPU's slow denormal path. Derived from
/// the spike, with direct MXCSR access for cross-toolchain compatibility.
#[inline]
pub(crate) fn set_ftz_daz() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        // x86_64 guarantees SSE2. Manipulate MXCSR directly instead of the
        // deprecated `_MM_*FLUSH_ZERO*` intrinsics so this also compiles cleanly
        // under MSVC. Initialized storage is required because the inline assembly
        // writes through a pointer, which Rust's definite-initialization analysis
        // deliberately does not infer.
        let mut mxcsr = 0u32;
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut mxcsr, options(nostack));
        mxcsr |= (1 << 15) | (1 << 6); // FTZ | DAZ
        std::arch::asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack, readonly));
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        // AArch64: set the FZ bit (bit 24) of FPCR to flush denormals to zero.
        let mut fpcr: u64;
        std::arch::asm!("mrs {}, fpcr", out(reg) fpcr);
        fpcr |= 1 << 24;
        std::arch::asm!("msr fpcr, {}", in(reg) fpcr);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        write_mapped, DeviceSample, OutputConsumer, OutputResampler, OutputWriter, StreamHealth,
        CHANNELS, SAMPLE_RATE,
    };
    use rubato::Resampler;

    #[test]
    fn stream_health_is_one_way_and_shared_with_the_error_callback() {
        let health = StreamHealth::healthy();
        let callback_view = health.clone();
        assert!(health.is_healthy());

        callback_view.mark_failed();

        assert!(!health.is_healthy());
        assert!(!callback_view.is_healthy());
    }

    /// The three supported output types agree at silence and both PCM endpoints;
    /// out-of-domain values clip instead of wrapping, and NaN becomes silence.
    #[test]
    fn device_sample_conversion_clips_extrema() {
        let source = [
            f32::NEG_INFINITY,
            -1.5,
            -1.0,
            -0.5,
            0.0,
            0.5,
            1.0,
            1.5,
            f32::INFINITY,
            f32::NAN,
        ];
        let f32_out = source.map(<f32 as DeviceSample>::from_f32_clipped);
        assert_eq!(
            f32_out,
            [-1.0, -1.0, -1.0, -0.5, 0.0, 0.5, 1.0, 1.0, 1.0, 0.0]
        );

        let i16_out = source.map(<i16 as DeviceSample>::from_f32_clipped);
        assert_eq!(
            i16_out,
            [
                i16::MIN,
                i16::MIN,
                i16::MIN,
                -16_384,
                0,
                16_384,
                i16::MAX,
                i16::MAX,
                i16::MAX,
                0,
            ]
        );

        let u16_out = source.map(<u16 as DeviceSample>::from_f32_clipped);
        assert_eq!(
            u16_out,
            [
                0,
                0,
                0,
                16_384,
                32_768,
                49_151,
                u16::MAX,
                u16::MAX,
                u16::MAX,
                32_768
            ]
        );
    }

    /// A mono host receives a headroom-preserving arithmetic downmix rather than
    /// silently losing either side; the final result is clipped to the PCM range.
    #[test]
    fn write_mapped_downmixes_stereo_to_mono() {
        let src = [1.0, -1.0, 0.8, 0.4, 2.0, 2.0];
        let mut data = [9.0f32; 3];
        write_mapped(&mut data, 1, &[(0, &src)]);
        assert_eq!(data, [0.0, 0.6, 1.0]);
    }

    /// Stereo is the identity layout apart from the required final-boundary clip.
    #[test]
    fn write_mapped_preserves_stereo_and_clips() {
        let src = [-2.0, 0.25, 0.5, 2.0];
        let mut data = [9.0f32; 4];
        write_mapped(&mut data, 2, &[(0, &src)]);
        assert_eq!(data, [-1.0, 0.25, 0.5, 1.0]);
    }

    /// Integer callbacks use the same layout primitive, including unsigned PCM's
    /// non-zero equilibrium value.
    #[test]
    fn write_mapped_converts_stereo_to_integer_pcm() {
        let src = [-1.0, 0.0, 0.5, 1.0];
        let mut i16_data = [9i16; 4];
        write_mapped(&mut i16_data, 2, &[(0, &src)]);
        assert_eq!(i16_data, [i16::MIN, 0, 16_384, i16::MAX]);

        let mut u16_data = [9u16; 4];
        write_mapped(&mut u16_data, 2, &[(0, &src)]);
        assert_eq!(u16_data, [u16::MIN, 32_768, 49_151, u16::MAX]);
    }

    /// A callback larger than the fixed scratch is processed tile-by-tile. Every
    /// output frame consumes its matching ring frame; no tail is zeroed or left
    /// poisoned after the first tile.
    #[test]
    fn long_callback_drains_primary_past_scratch_capacity() {
        const FRAMES: usize = 19;
        let source: Vec<f32> = (0..FRAMES)
            .flat_map(|frame| {
                let sample = frame as f32 / FRAMES as f32;
                [sample, -sample]
            })
            .collect();
        let (mut producer, primary) = OutputConsumer::new_test_pair(FRAMES + 1);
        for &sample in &source {
            assert!(producer.push(sample).is_ok());
        }

        let mut output = OutputWriter {
            device_channels: CHANNELS as usize,
            primary_offset: 0,
            primary,
            secondary: None,
            primary_resampler: None,
            secondary_resampler: None,
            scratch: vec![0.0; 3 * CHANNELS as usize],
            secondary_scratch: Vec::new(),
        };
        let mut data = vec![9.0f32; FRAMES * CHANNELS as usize];
        output.write(&mut data);

        assert_eq!(data, source, "all 19 frames survive a 3-frame scratch tile");
    }

    /// Combined master/cue routing remains aligned across many scratch boundaries,
    /// including when the secondary scratch is the smaller tiling constraint.
    #[test]
    fn long_callback_drains_primary_and_secondary_in_lockstep() {
        const FRAMES: usize = 17;
        const DEVICE_CHANNELS: usize = 6;
        let master: Vec<f32> = (0..FRAMES)
            .flat_map(|frame| {
                let sample = frame as f32 * 0.01;
                [sample, -sample]
            })
            .collect();
        let cue: Vec<f32> = (0..FRAMES)
            .flat_map(|frame| {
                let sample = 0.2 + frame as f32 * 0.01;
                [sample, -sample]
            })
            .collect();
        let (mut master_producer, primary) = OutputConsumer::new_test_pair(FRAMES + 1);
        let (mut cue_producer, cue_consumer) = OutputConsumer::new_test_pair(FRAMES + 1);
        for &sample in &master {
            assert!(master_producer.push(sample).is_ok());
        }
        for &sample in &cue {
            assert!(cue_producer.push(sample).is_ok());
        }

        let mut output = OutputWriter {
            device_channels: DEVICE_CHANNELS,
            primary_offset: 0,
            primary,
            secondary: Some(cue_consumer),
            primary_resampler: None,
            secondary_resampler: None,
            scratch: vec![0.0; 4 * CHANNELS as usize],
            secondary_scratch: vec![0.0; 2 * CHANNELS as usize],
        };
        let mut data = vec![9.0f32; FRAMES * DEVICE_CHANNELS];
        output.write(&mut data);

        for (frame, output) in data.chunks_exact(DEVICE_CHANNELS).enumerate() {
            assert_eq!(
                output,
                &[
                    master[2 * frame],
                    master[2 * frame + 1],
                    cue[2 * frame],
                    cue[2 * frame + 1],
                    0.0,
                    0.0,
                ],
                "frame {frame} stays aligned after repeated two-frame tiles",
            );
        }
    }

    /// A lone block at offset 0 lands on channels 1/2 and zeroes the rest of each
    /// frame (master, or a split cue on a stereo/wide non-FLX4 device).
    #[test]
    fn spread_offset_0_lands_on_channels_1_2() {
        let src = [0.1, 0.2, 0.3, 0.4]; // two stereo frames
        let dev_ch = 4;
        let mut data = vec![9.0f32; 2 * dev_ch]; // pre-fill to prove zeroing
        write_mapped(&mut data, dev_ch, &[(0, &src)]);
        assert_eq!(data, vec![0.1, 0.2, 0.0, 0.0, 0.3, 0.4, 0.0, 0.0]);
    }

    /// A lone block at offset 2 lands on channels 3/4 with 1/2 silent — how a
    /// split cue stream reaches the FLX4 phones jack (its 1/2 is the MASTER RCA).
    #[test]
    fn spread_offset_2_lands_on_channels_3_4() {
        let cue = [0.7, 0.8]; // one stereo frame
        let dev_ch = 4;
        let mut data = vec![9.0f32; dev_ch];
        write_mapped(&mut data, dev_ch, &[(2, &cue)]);
        assert_eq!(data, vec![0.0, 0.0, 0.7, 0.8]);
    }

    /// A device block longer than the source silences the trailing frames (the
    /// overflow guard for an unexpectedly large block).
    #[test]
    fn spread_zeroes_frames_past_the_source() {
        let src = [0.5, 0.6]; // one stereo frame only
        let dev_ch = 2;
        let mut data = vec![9.0f32; 2 * dev_ch]; // two frames
        write_mapped(&mut data, dev_ch, &[(0, &src)]);
        assert_eq!(data, vec![0.5, 0.6, 0.0, 0.0]);
    }

    /// Two placements (the FLX4 combined path): master on 1/2, cue on 3/4, and
    /// channels ≥4 zeroed.
    #[test]
    fn spread_combined_lands_master_1_2_cue_3_4() {
        let master = [0.1, 0.2]; // one stereo frame
        let cue = [0.7, 0.8];
        let dev_ch = 6;
        let mut data = vec![9.0f32; dev_ch];
        write_mapped(&mut data, dev_ch, &[(0, &master), (2, &cue)]);
        assert_eq!(data, vec![0.1, 0.2, 0.7, 0.8, 0.0, 0.0]);
    }

    /// Placements run dry independently: a short cue still silences only channels
    /// 3/4, leaving master on 1/2 intact.
    #[test]
    fn spread_combined_silences_a_short_placement_only() {
        let master = [0.1, 0.2, 0.3, 0.4]; // two frames
        let cue = [0.7, 0.8]; // one frame — second frame's cue runs dry
        let dev_ch = 4;
        let mut data = vec![9.0f32; 2 * dev_ch];
        write_mapped(&mut data, dev_ch, &[(0, &master), (2, &cue)]);
        assert_eq!(
            data,
            vec![0.1, 0.2, 0.7, 0.8, 0.3, 0.4, 0.0, 0.0],
            "frame 0 carries master+cue; frame 1 carries master with cue zeroed",
        );
    }

    // --- OutputResampler (the non-48000 fallback path, ADR-0029) ---
    //
    // Most of these exercise the resampling core directly on `OutputResampler::input`
    // / `resample_chunk` (the drain-free half of `produce_chunk`), so no device or
    // ring is needed — the same headless-testability discipline as the playback
    // varispeed tests. `fill` (the carry-FIFO + drain path) is exercised against a
    // real ring via `OutputConsumer::new_test_pair`. The actual 44.1 kHz output to a
    // Bluetooth device is the hardware checklist's job.

    /// Target rate for the tests — a 44100 Bluetooth speaker, the motivating case.
    const DEVICE_RATE: u32 = 44_100;
    /// Resampler chunk size (frames) used throughout.
    const CHUNK_FRAMES: usize = 256;

    /// Oversized callbacks also tile correctly after the non-48k resampler. Once
    /// startup latency clears, a DC signal must fill the entire long callback,
    /// including its final frame beyond many scratch boundaries.
    #[test]
    fn long_resampled_callback_has_no_silent_tail() {
        const LEFT: f32 = 0.3;
        const RIGHT: f32 = -0.3;
        const TILE_FRAMES: usize = 3;
        const CALLBACK_FRAMES: usize = 29;
        let (mut producer, primary) = OutputConsumer::new_test_pair(1 << 16);
        while producer.slots() >= CHANNELS as usize {
            assert!(producer.push(LEFT).is_ok());
            assert!(producer.push(RIGHT).is_ok());
        }

        let mut output = OutputWriter {
            device_channels: CHANNELS as usize,
            primary_offset: 0,
            primary,
            secondary: None,
            primary_resampler: Some(
                OutputResampler::new(DEVICE_RATE, 8).expect("small 44.1k resampler builds"),
            ),
            secondary_resampler: None,
            scratch: vec![0.0; TILE_FRAMES * CHANNELS as usize],
            secondary_scratch: Vec::new(),
        };
        let mut warmup = vec![0.0f32; 8 * CHANNELS as usize];
        for _ in 0..32 {
            output.write(&mut warmup);
        }

        let mut data = vec![-9.0f32; CALLBACK_FRAMES * CHANNELS as usize];
        output.write(&mut data);
        assert!(
            data.chunks_exact(CHANNELS as usize)
                .all(|frame| { (frame[0] - LEFT).abs() < 0.02 && (frame[1] - RIGHT).abs() < 0.02 }),
            "all {CALLBACK_FRAMES} frames are resampled through {TILE_FRAMES}-frame tiles: {:?}",
            &data[data.len() - 8..],
        );
    }

    /// Fill `n_in` interleaved-stereo frames of `input` with a 48 kHz-domain sine at
    /// `freq`, continuing from `phase0`; returns the phase to resume from so
    /// successive blocks stay continuous.
    fn fill_sine(input: &mut [f32], n_in: usize, freq: f32, phase0: f32) -> f32 {
        let dphase = 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE as f32;
        let mut phase = phase0;
        for f in 0..n_in {
            let s = phase.sin() * 0.5;
            input[2 * f] = s;
            input[2 * f + 1] = s;
            phase += dphase;
        }
        phase
    }

    /// A 48000 → 44100 resampler builds, and each block is a full `chunk_frames`
    /// from a (downsample) demand of ≥ `chunk_frames` input frames — all finite.
    #[test]
    fn output_resampler_builds_and_produces_full_blocks() {
        let mut r =
            OutputResampler::new(DEVICE_RATE, CHUNK_FRAMES).expect("44.1k resampler builds");
        assert_eq!(r.chunk.len(), CHUNK_FRAMES * CHANNELS as usize);
        let n_in = r.resampler.input_frames_next();
        assert!(
            n_in >= CHUNK_FRAMES,
            "downsample pulls ≥ output frames, got {n_in}"
        );
        assert!(
            r.input.len() >= n_in * CHANNELS as usize,
            "input scratch ({}) fits the demand ({n_in})",
            r.input.len()
        );
        fill_sine(&mut r.input, n_in, 1_000.0, 0.0);
        assert!(r.resample_chunk(n_in), "resample succeeds");
        assert!(
            r.chunk.iter().all(|s| s.is_finite()),
            "output is finite (no NaN/inf)"
        );
    }

    /// Over many blocks the resampler consumes input at exactly the 48000/44100
    /// ratio — proof the `input_frames_next` bookkeeping does not drift (which would
    /// slowly drain or overflow the output ring in the running app).
    #[test]
    fn output_resampler_consumes_input_at_the_rate_ratio() {
        let mut r = OutputResampler::new(DEVICE_RATE, CHUNK_FRAMES).unwrap();
        let blocks = 500;
        let mut total_in = 0usize;
        for _ in 0..blocks {
            let n_in = r.resampler.input_frames_next();
            total_in += n_in;
            for s in r.input[..n_in * CHANNELS as usize].iter_mut() {
                *s = 0.0;
            }
            assert!(r.resample_chunk(n_in));
        }
        let ratio = total_in as f64 / (blocks * CHUNK_FRAMES) as f64;
        let expected = SAMPLE_RATE as f64 / DEVICE_RATE as f64; // ≈ 1.0884
        assert!(
            (ratio - expected).abs() < 0.005,
            "input/output ratio {ratio:.4} ≈ 48000/44100 {expected:.4} (no drift)"
        );
    }

    /// A continuous 1 kHz sine keeps its level through the conversion: steady-state
    /// output RMS matches the input within 1 dB (correct pitch + no gain change).
    #[test]
    fn output_resampler_preserves_sine_energy() {
        let mut r = OutputResampler::new(DEVICE_RATE, CHUNK_FRAMES).unwrap();
        let mut phase = 0.0;
        // Warm up past the resampler's startup delay.
        for _ in 0..10 {
            let n_in = r.resampler.input_frames_next();
            phase = fill_sine(&mut r.input, n_in, 1_000.0, phase);
            assert!(r.resample_chunk(n_in));
        }
        let mut sum_sq = 0.0f64;
        let mut n = 0u64;
        for _ in 0..20 {
            let n_in = r.resampler.input_frames_next();
            phase = fill_sine(&mut r.input, n_in, 1_000.0, phase);
            assert!(r.resample_chunk(n_in));
            for &s in r.chunk.iter() {
                sum_sq += (s as f64) * (s as f64);
                n += 1;
            }
        }
        let out_rms = (sum_sq / n as f64).sqrt();
        let in_rms = 0.5 / std::f64::consts::SQRT_2; // amplitude-0.5 sine
        let db = 20.0 * (out_rms / in_rms).log10();
        assert!(
            db.abs() < 1.0,
            "sine energy preserved within 1 dB, got {db:.2} dB (rms {out_rms:.4})"
        );
    }

    /// `fill` serves any block size — including ones that differ from the resampler
    /// chunk — exactly and continuously, via the carry FIFO. Driving a DISTINCT
    /// per-channel DC (left = +0.3, right = −0.3) through irregular block sizes,
    /// every block comes back full with left and right intact (no zeroed tails, no
    /// gaps, no drift, and no L/R swap at a carry boundary) once the startup delay
    /// clears. This is the robustness ADR-0029 adds for devices that don't honour
    /// the requested buffer size (some Bluetooth paths).
    #[test]
    fn fill_serves_any_block_size_continuously() {
        const LEFT: f32 = 0.3;
        const RIGHT: f32 = -0.3;
        let mut r = OutputResampler::new(DEVICE_RATE, CHUNK_FRAMES).unwrap();
        // A ring big enough to stay primed, kept topped up with the L≠R signal so
        // `fill`'s internal drains never starve (this isolates the FIFO logic from
        // underruns). Push whole frames so the interleaved ring stays L/R aligned.
        let (mut producer, mut consumer) = OutputConsumer::new_test_pair(1 << 16);
        let top_up = |producer: &mut rtrb::Producer<f32>| {
            while producer.slots() >= CHANNELS as usize {
                let _ = producer.push(LEFT);
                let _ = producer.push(RIGHT);
            }
        };
        top_up(&mut producer);

        // Block sizes that are smaller, equal to, and larger than CHUNK_FRAMES, plus
        // odd-but-even counts — exactly the variability `fill` must absorb.
        let block_frames = [64usize, 256, 300, 200, 512, 100, 333 & !1, 256];
        let mut out = vec![0.0f32; 1024 * CHANNELS as usize];

        // Warm up past the resampler's startup delay (early output is silent).
        for _ in 0..16 {
            r.fill(&mut consumer, &mut out[..CHUNK_FRAMES * CHANNELS as usize]);
            top_up(&mut producer);
        }

        for &bf in block_frames.iter().cycle().take(64) {
            let len = bf * CHANNELS as usize;
            // Poison the slice so a missed write shows up as a failure.
            out[..len].fill(-9.0);
            r.fill(&mut consumer, &mut out[..len]);
            top_up(&mut producer);
            let ok = out[..len]
                .chunks_exact(CHANNELS as usize)
                .all(|frame| (frame[0] - LEFT).abs() < 0.02 && (frame[1] - RIGHT).abs() < 0.02);
            assert!(
                ok,
                "every {bf}-frame block keeps left≈{LEFT}/right≈{RIGHT} (continuous, \
                 no gaps, no L/R swap): {:?}…",
                &out[..8]
            );
        }
    }
}
