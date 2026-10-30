//! The cpal device wrapper: a thin host around the device-free [`Engine`] core.
//!
//! Opens a stereo / f32 output stream with `BufferSize::Fixed(256)` and, in its
//! callback, sets FTZ/DAZ once and drains the engine's output ring(s) wrapped in
//! `assert_no_alloc`. The callback is the ONLY real-time path; it allocates
//! nothing, takes no lock, makes no syscall, and logs nothing. Ported from the
//! Spike A `rt_engine` device half (`spike/rust-audio/engine/src/bin/rt_engine.rs`),
//! now built on the library so the device path stays exercisable.
//!
//! The engine renders at exactly [`SAMPLE_RATE`] (48000). A device that offers a
//! 48000/f32 config is opened there and drained bit-exact (the fast path). A
//! device with NO 48000/f32 config — e.g. a 44100 Bluetooth speaker — is opened
//! at its own f32 rate and the 48 kHz stream is resampled to it on the callback
//! via [`OutputResampler`] (ADR-0029). All device-rate knowledge lives here; the
//! host's output ring stays a clean 48 kHz interleaved-stereo contract.
//!
//! Graceful no-device exit: if no output device or no usable f32 config is
//! available (likely in a sandbox / headless CI), [`run_stream`] returns
//! [`DeviceError::Unavailable`] rather than hanging or panicking.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, StreamConfig};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use crate::host::OutputConsumer;
use crate::{Engine, CHANNELS, SAMPLE_RATE};

/// Requested device buffer size (frames). Clamped to the device's supported
/// range; the granted size is reported back in [`StreamInfo`].
const REQUESTED_BUFFER: u32 = 256;

/// Why a device stream could not be opened. `Unavailable` is the sandbox/headless
/// case — callers treat it as "no device, exit cleanly", not a failure.
#[derive(Debug)]
pub enum DeviceError {
    /// No output device, or no usable f32 config at all (e.g. a sandbox). A
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
    pub buffer_frames: BufferSize,
}

/// A running output stream driving an [`Engine`]. The cpal stream stops when this
/// is dropped; the `Engine` lives inside the callback for the stream's lifetime.
pub struct AudioStream {
    _stream: cpal::Stream,
    info: StreamInfo,
}

impl AudioStream {
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }
}

/// One output device the engine can open, for the picker UI.
pub struct OutputDeviceInfo {
    pub name: String,
    /// Channels of its chosen usable (f32, ≥ stereo) config — the widest 48000/f32
    /// config, or the fallback config when the device cannot do 48000.
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

/// Choose a device's output config for the engine. Preference order:
///
/// 1. An exact 48000/f32 config (≥ stereo), WIDEST channel count — the bit-exact
///    fast path. A ≥4-channel device (the FLX4) lands master on 1/2, cue on 3/4.
/// 2. Otherwise the device's own default config, if it is f32 / ≥ stereo — its
///    nominal rate (e.g. 44100 for a Bluetooth speaker), which the OS will not
///    itself resample, so we resample 48000 → it directly (ADR-0029).
/// 3. Otherwise any f32 / ≥ stereo config, at the supported rate NEAREST 48000
///    (widest channels as a tie-break).
///
/// The returned config's `sample_rate()` is the rate the stream opens at; the
/// caller resamples when it is not [`SAMPLE_RATE`].
fn pick_config(device: &cpal::Device) -> Option<cpal::SupportedStreamConfig> {
    let exact = device.supported_output_configs().ok().and_then(|configs| {
        configs
            .filter(|cfg| {
                cfg.channels() >= CHANNELS
                    && cfg.sample_format() == cpal::SampleFormat::F32
                    && cfg.min_sample_rate() <= SAMPLE_RATE
                    && cfg.max_sample_rate() >= SAMPLE_RATE
            })
            .max_by_key(|cfg| cfg.channels())
            .map(|cfg| cfg.with_sample_rate(SAMPLE_RATE))
    });
    if exact.is_some() {
        return exact;
    }

    // No 48000/f32 config: fall back to a resampled rate. Prefer the device's own
    // default (its nominal rate, so the OS does not double-resample under us).
    if let Ok(default) = device.default_output_config() {
        if default.sample_format() == cpal::SampleFormat::F32 && default.channels() >= CHANNELS {
            return Some(default);
        }
    }

    // Last resort: the f32 / ≥ stereo config whose supported range lands a rate
    // closest to 48000 (widest channels breaks ties). `clamp` gives the nearest
    // in-range rate — for a 44100-only device that is 44100.
    device.supported_output_configs().ok().and_then(|configs| {
        configs
            .filter(|cfg| {
                cfg.channels() >= CHANNELS && cfg.sample_format() == cpal::SampleFormat::F32
            })
            .min_by_key(|cfg| {
                let rate = SAMPLE_RATE.clamp(cfg.min_sample_rate(), cfg.max_sample_rate());
                (rate.abs_diff(SAMPLE_RATE), u16::MAX - cfg.channels())
            })
            .map(|cfg| {
                let rate = SAMPLE_RATE.clamp(cfg.min_sample_rate(), cfg.max_sample_rate());
                cfg.with_sample_rate(rate)
            })
    })
}

/// Enumerate the output devices the engine can open (any f32, ≥ stereo config —
/// exact 48000 or a resampled fallback) with their chosen channel count, for the
/// picker. Off the RT path — called from a command when the picker opens. Empty
/// on a headless host.
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
/// chooses — a 48000/f32 widest config (so the cue reaches channels 3/4 on a
/// ≥4-channel device), or a resampled fallback rate. `info.sample_rate` is the
/// rate the stream actually opens at; the caller resamples when it ≠ 48000.
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
            "device '{device_name}' has no usable f32 output config \
             (no f32 ≥ stereo config at any sample rate)"
        ))
    })?;

    let device_channels = supported.channels();
    let device_rate = supported.sample_rate();
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
        buffer_frames: buffer_size,
    };

    Ok((device, config, info))
}

/// Zero a wider interleaved device buffer, then lay each `(channel_offset, src)`
/// interleaved-stereo block onto channels `[offset, offset+1]`. One primitive for
/// every routing: master on 1/2 is `&[(0, master)]`; the FLX4 combined path is
/// `&[(0, master), (2, cue)]`; a split cue on the FLX4 phones is `&[(2, cue)]`.
/// A frame past a block's length (or an offset past the device) is left silent.
/// `placements` is a small fixed stack slice, so this stays pure and alloc-free —
/// RT-safe.
fn spread(data: &mut [f32], dev_ch: usize, placements: &[(usize, &[f32])]) {
    let frames = data.len() / dev_ch;
    for f in 0..frames {
        let base = f * dev_ch;
        for c in 0..dev_ch {
            data[base + c] = 0.0;
        }
        for &(offset, src) in placements {
            if offset + 1 < dev_ch && 2 * f + 1 < src.len() {
                data[base + offset] = src[2 * f];
                data[base + offset + 1] = src[2 * f + 1];
            }
        }
    }
}

/// Number of overlapping FFT blocks rubato's synchronous resampler uses — the
/// usual real-time choice (more blocks = marginally better stopband for more
/// latency; 2 keeps the added delay to a few ms, dwarfed by the output ring).
const RESAMPLER_BLOCKS: usize = 2;

/// Resamples the engine's 48 kHz interleaved-stereo feed to a device with no
/// 48000/f32 config (e.g. a 44100 Bluetooth speaker), via rubato's synchronous
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
        Some(OutputResampler { resampler, input, chunk, carry, carry_len: 0, chunk_frames })
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

/// Open `selected` (a 48000/f32 config, or a resampled fallback rate), build a
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
    mut primary: OutputConsumer,
    secondary: Option<OutputConsumer>,
    primary_on_phones: bool,
) -> Result<AudioStream, DeviceError> {
    let (device, config, info) = open_output(selected)?;
    let device_channels = info.device_channels as usize;

    // The secondary (cue) feed needs channels 3/4 — only a ≥4-channel device (the
    // FLX4) can carry it alongside the primary. Drop it on a narrower device.
    let mut secondary = if device_channels >= 4 { secondary } else { None };
    let secondary_routed = secondary.is_some();
    // Where the primary lands: a standalone cue stream on a ≥4-channel device (the
    // FLX4 chosen as a SEPARATE cue device) belongs on the phones channels 3/4
    // (offset 2), not 1/2 (its MASTER RCA). Master, and a cue on a stereo device
    // (laptop jack, Bluetooth), land on 1/2 (offset 0).
    let primary_offset = if primary_on_phones && device_channels >= 4 { 2 } else { 0 };

    // When the device opened at a rate other than the engine's 48 kHz, build a
    // resampler per feed (off the RT path; the callback only `fill`s them). A
    // failure here is fatal — playing 48 kHz audio straight into a 44.1 kHz buffer
    // would be pitched wrong — so it surfaces as a stream error. The resampler's
    // chunk granularity is the requested buffer; its FIFO decouples that from the
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
    let mut primary_resampler = if device_rate != SAMPLE_RATE {
        Some(build_resampler("master")?)
    } else {
        None
    };
    let mut secondary_resampler = if device_rate != SAMPLE_RATE && secondary_routed {
        Some(build_resampler("cue")?)
    } else {
        None
    };

    let mut first_call = true;
    // Per-callback scratch for wide (>2ch) devices: the rings (and the resamplers)
    // produce interleaved stereo, so on a wider device we gather stereo into these
    // scratches and spread into the device buffer — same path whether the stereo
    // came from a straight ring drain or a resample. Sized ONCE here, off the RT
    // path, for a generous worst-case block; the callback never resizes them.
    let mut scratch: Vec<f32> = Vec::new();
    let mut secondary_scratch: Vec<f32> = Vec::new();
    if device_channels != CHANNELS as usize {
        scratch_reserve(&mut scratch, REQUESTED_BUFFER as usize * 4);
        if secondary_routed {
            scratch_reserve(&mut secondary_scratch, REQUESTED_BUFFER as usize * 4);
        }
    }

    let err_fn = |e| eprintln!("lsdj-engine: stream error: {e}");

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                no_alloc(|| {
                    if first_call {
                        set_ftz_daz();
                        first_call = false;
                    }
                    let dev_ch = device_channels;
                    match primary_resampler.as_mut() {
                        // Bit-exact: device runs at 48 kHz, drain straight through.
                        None => {
                            if dev_ch == CHANNELS as usize {
                                // Stereo fast path: drain into the device buffer.
                                primary.drain_into(data);
                            } else {
                                // Wider device: drain stereo into scratch, spread.
                                let want = (data.len() / dev_ch) * CHANNELS as usize;
                                let usable = scratch.len().min(want);
                                primary.drain_into(&mut scratch[..usable]);
                                if let Some(secondary) = secondary.as_mut() {
                                    // Combined: primary on 1/2, secondary (cue) 3/4.
                                    let su = secondary_scratch.len().min(want);
                                    secondary.drain_into(&mut secondary_scratch[..su]);
                                    spread(
                                        data,
                                        dev_ch,
                                        &[(0, &scratch[..usable]), (2, &secondary_scratch[..su])],
                                    );
                                } else {
                                    // Lone feed (master, or a split cue).
                                    spread(data, dev_ch, &[(primary_offset, &scratch[..usable])]);
                                }
                            }
                        }
                        // Device runs at another rate: resample 48 kHz → device rate.
                        // `fill` serves exactly the bytes asked for (its FIFO absorbs
                        // the chunk-vs-block-size difference).
                        Some(pr) => {
                            if dev_ch == CHANNELS as usize {
                                // Stereo device: resample into the device buffer.
                                pr.fill(&mut primary, data);
                            } else {
                                // Wider device: resample into scratch, then spread.
                                let want = (data.len() / dev_ch) * CHANNELS as usize;
                                let usable = scratch.len().min(want);
                                if let (Some(sr), Some(secondary)) =
                                    (secondary_resampler.as_mut(), secondary.as_mut())
                                {
                                    // Combined: both feeds resampled, master 1/2, cue 3/4.
                                    let su = secondary_scratch.len().min(want);
                                    pr.fill(&mut primary, &mut scratch[..usable]);
                                    sr.fill(secondary, &mut secondary_scratch[..su]);
                                    spread(
                                        data,
                                        dev_ch,
                                        &[(0, &scratch[..usable]), (2, &secondary_scratch[..su])],
                                    );
                                } else {
                                    // Lone feed (master, or a split cue).
                                    pr.fill(&mut primary, &mut scratch[..usable]);
                                    spread(data, dev_ch, &[(primary_offset, &scratch[..usable])]);
                                }
                            }
                        }
                    }
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

/// Open the default output device at exactly 48000/stereo/f32, build the stream
/// that renders `engine` in its callback, start it, and return the running
/// stream. The `engine` is MOVED into the audio callback.
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
pub fn run_stream(mut engine: Engine) -> Result<AudioStream, DeviceError> {
    let (device, config, info) = open_output(None)?;
    if info.sample_rate != SAMPLE_RATE {
        return Err(DeviceError::Unavailable(format!(
            "default device opened at {} Hz; run_stream needs {SAMPLE_RATE} (it does not resample)",
            info.sample_rate
        )));
    }
    let device_channels = info.device_channels;

    // Per-callback scratch for wide (>2ch) devices: the engine renders exactly
    // stereo, so on a wider device we render into this stereo scratch and spread
    // it into the device buffer (extra channels zeroed). On the common stereo
    // device the scratch stays empty and the fast path renders straight into
    // `data`. Sized ONCE here, off the RT path, for a generous worst-case block
    // (4× the requested buffer); the callback never resizes it.
    let mut first_call = true;
    let mut scratch: Vec<f32> = Vec::new();
    if device_channels as usize != CHANNELS as usize {
        scratch_reserve(&mut scratch, REQUESTED_BUFFER as usize * 4);
    }

    let err_fn = |e| eprintln!("lsdj-engine: stream error: {e}");

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                // Everything below MUST be alloc/lock/syscall/log free. The guard
                // proves it (warns in release if violated).
                crate::device::no_alloc(|| {
                    if first_call {
                        crate::device::set_ftz_daz();
                        first_call = false;
                    }
                    let dev_ch = device_channels as usize;
                    let frames = data.len() / dev_ch;

                    if dev_ch == CHANNELS as usize {
                        // Stereo fast path: render straight into the device buffer.
                        engine.render(data, frames);
                    } else {
                        // Wider device: render stereo into scratch, then spread.
                        // `scratch` was pre-sized below on the first wide call;
                        // if cpal ever hands a bigger block than expected we skip
                        // the overflow rather than alloc on the RT thread.
                        let want = frames * CHANNELS as usize;
                        let usable = scratch.len().min(want);
                        let frames_usable = usable / CHANNELS as usize;
                        engine.render(&mut scratch[..usable], frames_usable);
                        for f in 0..frames {
                            let base = f * dev_ch;
                            if f < frames_usable {
                                data[base] = scratch[2 * f];
                                data[base + 1] = scratch[2 * f + 1];
                            } else {
                                data[base] = 0.0;
                                data[base + 1] = 0.0;
                            }
                            for c in 2..dev_ch {
                                data[base + c] = 0.0;
                            }
                        }
                    }
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
    })
}

/// Pre-size the scratch buffer (off the RT path), before it is moved into the
/// callback. Pulled out so the intent — allocate the worst-case block ONCE,
/// never on the RT thread — is explicit.
fn scratch_reserve(scratch: &mut Vec<f32>, frames: usize) {
    scratch.resize(frames * CHANNELS as usize, 0.0);
}

/// `assert_no_alloc` wrapper, isolated here so `lib.rs`/tests don't depend on the
/// allocator guard. The guard only arms if `AllocDisabler` is the global
/// allocator (registered by the binary); otherwise it is a transparent passthrough.
#[inline]
pub(crate) fn no_alloc<T>(f: impl FnOnce() -> T) -> T {
    assert_no_alloc::assert_no_alloc(f)
}

/// Enable flush-to-zero / denormals-are-zero on the calling (audio) thread so a
/// decaying denormal tail never trips the CPU's slow denormal path. Ported
/// verbatim from the spike.
#[inline]
pub(crate) fn set_ftz_daz() {
    #[cfg(all(target_arch = "x86_64", target_feature = "sse"))]
    unsafe {
        use std::arch::x86_64::{
            _MM_FLUSH_ZERO_ON, _MM_GET_FLUSH_ZERO_MODE, _MM_SET_FLUSH_ZERO_MODE,
        };
        let _ = _MM_GET_FLUSH_ZERO_MODE();
        _MM_SET_FLUSH_ZERO_MODE(_MM_FLUSH_ZERO_ON);
        // DAZ via the MXCSR DAZ bit (bit 6).
        let mut mxcsr: u32;
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut mxcsr, options(nostack));
        mxcsr |= 1 << 6;
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
    use super::{spread, OutputConsumer, OutputResampler, CHANNELS, SAMPLE_RATE};
    use rubato::Resampler;

    /// A lone block at offset 0 lands on channels 1/2 and zeroes the rest of each
    /// frame (master, or a split cue on a stereo/wide non-FLX4 device).
    #[test]
    fn spread_offset_0_lands_on_channels_1_2() {
        let src = [0.1, 0.2, 0.3, 0.4]; // two stereo frames
        let dev_ch = 4;
        let mut data = vec![9.0f32; 2 * dev_ch]; // pre-fill to prove zeroing
        spread(&mut data, dev_ch, &[(0, &src)]);
        assert_eq!(data, vec![0.1, 0.2, 0.0, 0.0, 0.3, 0.4, 0.0, 0.0]);
    }

    /// A lone block at offset 2 lands on channels 3/4 with 1/2 silent — how a
    /// split cue stream reaches the FLX4 phones jack (its 1/2 is the MASTER RCA).
    #[test]
    fn spread_offset_2_lands_on_channels_3_4() {
        let cue = [0.7, 0.8]; // one stereo frame
        let dev_ch = 4;
        let mut data = vec![9.0f32; dev_ch];
        spread(&mut data, dev_ch, &[(2, &cue)]);
        assert_eq!(data, vec![0.0, 0.0, 0.7, 0.8]);
    }

    /// A device block longer than the source silences the trailing frames (the
    /// overflow guard for an unexpectedly large block).
    #[test]
    fn spread_zeroes_frames_past_the_source() {
        let src = [0.5, 0.6]; // one stereo frame only
        let dev_ch = 2;
        let mut data = vec![9.0f32; 2 * dev_ch]; // two frames
        spread(&mut data, dev_ch, &[(0, &src)]);
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
        spread(&mut data, dev_ch, &[(0, &master), (2, &cue)]);
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
        spread(&mut data, dev_ch, &[(0, &master), (2, &cue)]);
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
        assert!(n_in >= CHUNK_FRAMES, "downsample pulls ≥ output frames, got {n_in}");
        assert!(
            r.input.len() >= n_in * CHANNELS as usize,
            "input scratch ({}) fits the demand ({n_in})",
            r.input.len()
        );
        fill_sine(&mut r.input, n_in, 1_000.0, 0.0);
        assert!(r.resample_chunk(n_in), "resample succeeds");
        assert!(r.chunk.iter().all(|s| s.is_finite()), "output is finite (no NaN/inf)");
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
        assert!(db.abs() < 1.0, "sine energy preserved within 1 dB, got {db:.2} dB (rms {out_rms:.4})");
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
            let ok = out[..len].as_chunks::<{ CHANNELS as usize }>().0.iter().all(|frame| {
                (frame[0] - LEFT).abs() < 0.02 && (frame[1] - RIGHT).abs() < 0.02
            });
            assert!(
                ok,
                "every {bf}-frame block keeps left≈{LEFT}/right≈{RIGHT} (continuous, \
                 no gaps, no L/R swap): {:?}…",
                &out[..8]
            );
        }
    }
}
