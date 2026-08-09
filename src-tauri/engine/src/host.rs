//! The engine host: a dedicated render thread that owns the [`Engine`], plus a
//! thread-safe control surface the rest of the process (the Tauri IPC thread)
//! drives (Phase 2, step 2).
//!
//! # Why a decoupled render thread (the load-bearing design)
//!
//! [`Engine`] has `&mut self` methods for BOTH the RT path ([`Engine::render`],
//! alloc-free) and control ([`Engine::set_eq`], [`Engine::load_track`], … — some
//! of which ALLOCATE, rebuilding `fundsp` nodes or taking ownership of a decoded
//! buffer). A single thread must own the `Engine` (Rust's `&mut self` rules make
//! that ownership the whole RT-safety argument: a control mutation and a render
//! can never overlap). If the cpal device callback owned the `Engine`, applying a
//! control command would allocate INSIDE the audio callback — forbidden.
//!
//! So the `Engine` lives on its own spawned **render thread**, NOT in the cpal
//! callback. That thread loops:
//!
//! 1. Drain the wait-free [`Command`] channel, applying each command to the
//!    `Engine`. Allocs are FINE here — this is not the cpal callback. The old
//!    buffers/nodes a command replaces are dropped HERE too, off the callback.
//! 2. Render blocks via [`Engine::render`] into the **output ring** (an `rtrb`
//!    of interleaved-stereo f32), pacing to keep it filled to a target depth
//!    ([`OUTPUT_RING_TARGET_FRAMES`]) and parking briefly when it is full.
//! 3. Publish a [`Snapshot`] (per-deck track status / loop slots) behind a
//!    `Mutex` the IPC thread reads.
//!
//! The cpal device callback ([`crate::device::open_main_stream`]) ONLY drains the
//! output ring into the device buffer, counts an underrun if the ring is short,
//! and sets FTZ/DAZ — trivially alloc-free, still under `assert_no_alloc`.
//!
//! ## The latency trade-off (a deliberate v1 choice)
//!
//! Going through the output ring adds the ring's depth
//! (~[`OUTPUT_RING_TARGET_FRAMES`] frames, tens of ms) of latency on top of the
//! device buffer, versus rendering directly in the callback. We accept that here
//! because rendering in the callback is incompatible with allocating control
//! commands. The lower-latency build-and-swap-in-callback design (render in the
//! callback; deliver control as pre-built nodes over a wait-free channel and
//! return the old ones over a garbage channel) is a future optimisation — it
//! needs settable-coefficient EQ (or handed-across chains) and that garbage-return
//! path, neither of which exists yet. This decoupled design keeps every alloc off
//! the callback today with no engine reshaping.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::telemetry::Telemetry;
use crate::{
    DeckHandle, Engine, EqBand, FxKind, LoopSlotStatus, TrackStatus, CHANNELS, DECK_COUNT,
    LOOP_SLOT_COUNT, SAMPLE_RATE,
};

/// Output ring capacity in frames. Sized to a generous ~0.5 s so a slow device
/// callback or a control-command burst on the render thread never overflows it;
/// the render thread paces to [`OUTPUT_RING_TARGET_FRAMES`], far below the cap.
const OUTPUT_RING_FRAMES: usize = SAMPLE_RATE as usize / 2;

/// Output ring fill target in frames (~75 ms): the render thread renders ahead
/// until the ring holds this much, then parks. Big enough to ride out render-loop
/// scheduling jitter, small enough to keep the added latency modest. This is the
/// latency the decoupled design trades for keeping allocs off the callback.
const OUTPUT_RING_TARGET_FRAMES: usize = (SAMPLE_RATE as usize) * 75 / 1000;

/// Frames the render thread produces per [`Engine::render`] block. Independent of
/// the device buffer size (the callback drains whatever it is handed from the
/// ring); a small block keeps the pacing responsive.
const RENDER_BLOCK_FRAMES: usize = 256;

/// How long the render thread parks when the output ring is full before checking
/// again. Short relative to the ring target so the ring is topped up promptly.
const RENDER_PARK: Duration = Duration::from_millis(2);

/// Capacity of the wait-free command channel (commands, not samples). Deep enough
/// to absorb a burst of UI control changes between render-loop drains without the
/// IPC thread ever blocking; an overrun drops the command (logged) rather than
/// stalling the UI.
const COMMAND_QUEUE_DEPTH: usize = 1024;

/// Capacity of the capture-sample reply channel. capture_sample is a rare,
/// explicit user action (M15 style sampling); one outstanding reply at a time is
/// plenty, with slack so a double-press never blocks.
const CAPTURE_REPLY_DEPTH: usize = 8;

/// Anti-OOM backstop for the capture buffer, in interleaved samples (~60 s of
/// stereo). This is NOT a recording-length limit — the take streams to disk and is
/// bounded only by free space. It caps how far the buffer may grow if the writer
/// thread stalls (e.g. a wedged disk); in normal operation the writer drains every
/// [`WRITER_PARK`] tick, keeping the buffer far below this.
const RECORDING_BACKPRESSURE_SAMPLES: usize = 60 * SAMPLE_RATE as usize * CHANNELS as usize;

/// Hard ceiling on a take's length, in interleaved samples: the largest count whose
/// byte size still fits a canonical WAV's 32-bit RIFF/data size fields
/// (`36 + samples * 2 <= u32::MAX`, floored to a whole stereo frame). At 48 kHz
/// stereo that's ~6 h 12 m — past any real set, but a plain WAV simply cannot address
/// more without RF64/WAVE64. Capture stops here so the header stays valid rather than
/// wrapping the size fields into a malformed file; a longer take would need a format
/// change, tracked in ADR-0028.
const RECORDING_MAX_SAMPLES: usize = ((u32::MAX as usize - 36) / 4) * 2;

/// How long the recorder's writer thread parks when the capture buffer is empty
/// before checking again. Short enough that buffered (un-written) audio — and thus
/// RAM — stays tiny; long enough to avoid a busy-spin.
const WRITER_PARK: Duration = Duration::from_millis(50);

/// The master-bus recorder. While `active`, the render thread appends each rendered
/// master block (as int16 PCM) into `buffer`; a dedicated writer thread swaps the
/// buffer empty each tick and streams it to a WAV on disk, so memory stays flat and
/// the take is bounded by disk space, not RAM. The append runs on the render thread
/// (NOT the cpal callback), which may lock + allocate; when inactive it is one
/// relaxed atomic load and returns.
struct Recorder {
    active: AtomicBool,
    /// Captured samples awaiting the writer thread. `Arc` so the writer thread holds
    /// the same buffer the render thread appends to; both touch it only under the
    /// `Mutex`, briefly.
    buffer: Arc<Mutex<Vec<i16>>>,
    /// The disk-writer thread + its stop flag — present only while recording.
    writer: Mutex<Option<WriterHandle>>,
    /// Interleaved samples captured so far this take. Capture stops appending once
    /// this reaches [`RECORDING_MAX_SAMPLES`], so the streamed WAV's 32-bit size
    /// fields never overflow. Written only on the render thread (reset at `start`,
    /// under the writer lock).
    captured: AtomicUsize,
}

/// Handle to the running writer thread: its stop flag and join handle. Joining
/// returns the thread's final write result (header patched, file flushed).
struct WriterHandle {
    stop: Arc<AtomicBool>,
    join: JoinHandle<Result<(), String>>,
}

impl Recorder {
    fn new() -> Self {
        Recorder {
            active: AtomicBool::new(false),
            buffer: Arc::new(Mutex::new(Vec::new())),
            writer: Mutex::new(None),
            captured: AtomicUsize::new(0),
        }
    }

    /// Append a rendered master block (interleaved-stereo f32) as int16 PCM. A
    /// no-op when not recording. Bounded by two independent caps: the buffer stops
    /// growing at [`RECORDING_BACKPRESSURE_SAMPLES`] so a stalled writer can't exhaust
    /// RAM, and the take stops capturing at [`RECORDING_MAX_SAMPLES`] so the WAV's
    /// 32-bit size fields never overflow.
    fn capture(&self, block: &[f32]) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        let captured = self.captured.load(Ordering::Relaxed);
        let mut buf = self.buffer.lock().unwrap_or_else(|p| p.into_inner());
        let buffer_room = RECORDING_BACKPRESSURE_SAMPLES.saturating_sub(buf.len());
        let format_room = RECORDING_MAX_SAMPLES.saturating_sub(captured);
        let before = buf.len();
        for &s in block.iter().take(buffer_room.min(format_room)) {
            buf.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        let pushed = buf.len() - before;
        drop(buf);
        if pushed > 0 {
            self.captured.fetch_add(pushed, Ordering::Relaxed);
        }
    }

    /// Take ownership of an already-opened `file`, write a placeholder WAV header, and
    /// spawn the writer thread that streams captured audio to it. The caller opens the
    /// file atomically (so path safety stays at the trust boundary, not here); this
    /// only errors if a take is already in progress or the writer thread can't spawn.
    /// On success, capture begins.
    fn start(&self, file: File) -> Result<(), String> {
        let mut slot = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_some() {
            return Err("a recording is already in progress".into());
        }
        self.buffer.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.captured.store(0, Ordering::Relaxed);

        let stop = Arc::new(AtomicBool::new(false));
        let buffer = self.buffer.clone();
        let thread_stop = stop.clone();
        let join = thread::Builder::new()
            .name("lsdj-recorder".into())
            .spawn(move || {
                let mut out = BufWriter::new(file);
                drain_to_wav(&buffer, &thread_stop, &mut out)
            })
            .map_err(|e| format!("cannot spawn recorder thread: {e}"))?;

        *slot = Some(WriterHandle { stop, join });
        // Activate only once the writer is ready, so no block is captured (and then
        // dropped) before there is somewhere to stream it.
        self.active.store(true, Ordering::Release);
        Ok(())
    }

    /// Stop the take: deactivate capture, signal the writer to flush the remaining
    /// samples + patch the WAV header, and join it. A no-op (`Ok`) when not
    /// recording. Returns the writer thread's final write result.
    fn stop(&self) -> Result<(), String> {
        self.active.store(false, Ordering::Release);
        let handle = self
            .writer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        let Some(handle) = handle else {
            return Ok(());
        };
        handle.stop.store(true, Ordering::Release);
        match handle.join.join() {
            Ok(result) => result,
            Err(_) => Err("recorder thread panicked".into()),
        }
    }
}

/// Stream the captured int16 samples in `buffer` to `out` as a 48 kHz / stereo /
/// 16-bit PCM WAV (the speaker feed exactly — post-limiter/clip-guard). Writes a
/// placeholder header, drains the buffer in swapped chunks until `stop` is set and
/// the buffer is empty, then seeks back and patches the RIFF + data sizes. Runs on
/// the writer thread (`out` is a `BufWriter<File>`); a `Cursor` stands in for the
/// file in tests.
fn drain_to_wav<W: Write + Seek>(
    buffer: &Mutex<Vec<i16>>,
    stop: &AtomicBool,
    out: &mut W,
) -> Result<(), String> {
    fn run<W: Write + Seek>(
        buffer: &Mutex<Vec<i16>>,
        stop: &AtomicBool,
        out: &mut W,
    ) -> std::io::Result<()> {
        write_wav_header(out, 0)?; // placeholder; patched once the length is known
        // Ping-ponged with the render thread's buffer so neither side reallocates
        // after warmup: each tick we swap our empty Vec in and write what came out.
        let mut scratch: Vec<i16> = Vec::new();
        let mut total: usize = 0;
        loop {
            let stopping = stop.load(Ordering::Acquire);
            {
                let mut buf = buffer.lock().unwrap_or_else(|p| p.into_inner());
                std::mem::swap(&mut *buf, &mut scratch);
            }
            if scratch.is_empty() {
                // `stop` is set only after capture is deactivated, so an empty buffer
                // while stopping means every captured sample has been drained.
                if stopping {
                    break;
                }
                thread::sleep(WRITER_PARK);
                continue;
            }
            for &s in &scratch {
                out.write_all(&s.to_le_bytes())?;
            }
            total += scratch.len();
            scratch.clear();
        }
        patch_wav_sizes(out, total)?;
        out.flush()
    }
    run(buffer, stop, out).map_err(|e| format!("recording write failed: {e}"))
}

/// Write the 44-byte canonical WAV header for 48 kHz / stereo / 16-bit PCM with the
/// given data-chunk length in bytes (0 as a placeholder while streaming).
fn write_wav_header<W: Write>(out: &mut W, data_len: u32) -> std::io::Result<()> {
    let channels = CHANNELS as u32;
    let sample_rate = SAMPLE_RATE;
    let bits = 16u32;
    let byte_rate = sample_rate * channels * bits / 8;
    let block_align = (channels * bits / 8) as u16;

    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_len).to_le_bytes())?;
    out.write_all(b"WAVE")?;
    out.write_all(b"fmt ")?;
    out.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&(channels as u16).to_le_bytes())?;
    out.write_all(&sample_rate.to_le_bytes())?;
    out.write_all(&byte_rate.to_le_bytes())?;
    out.write_all(&block_align.to_le_bytes())?;
    out.write_all(&(bits as u16).to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())
}

/// Patch the RIFF + data sizes into an already-written WAV once `total_samples` is
/// known, then leave the cursor at the end. The two size fields live at fixed
/// offsets 4 (RIFF chunk) and 40 (data chunk) in the canonical header above.
fn patch_wav_sizes<W: Write + Seek>(out: &mut W, total_samples: usize) -> std::io::Result<()> {
    let data_len = (total_samples * 2) as u32;
    out.seek(SeekFrom::Start(4))?;
    out.write_all(&(36 + data_len).to_le_bytes())?;
    out.seek(SeekFrom::Start(40))?;
    out.write_all(&data_len.to_le_bytes())?;
    out.seek(SeekFrom::End(0))?;
    Ok(())
}

/// A control command enqueued by the IPC/control thread and applied on the render
/// thread (where allocation is fine — it is NOT the cpal callback). One variant
/// per `Engine` control method. The `Vec` buffers for `LoadTrack` /
/// `LoadGeneratedLoop` are built and owned by the caller OFF the render thread
/// and MOVED in here; the render thread just installs them, and the OLD buffers
/// the install replaces are dropped on the render thread, off the callback.
///
/// Read-backs that return data (capture_sample) are NOT plain fire-and-forget:
/// they carry a reply sender so the render thread can ship the captured `Vec`
/// back to the waiting caller.
enum Command {
    SetCrossfade(f32),
    SetEq(usize, EqBand, f32),
    SetVolume(usize, f32),
    SetFx(usize, FxKind),
    SetFxAmount(usize, f32),
    ClearFx(usize),
    /// The deck's gated beat period in seconds (ADR-0025), `None` while the
    /// honesty gate is blank — the synced dub echo's clock.
    SetBeatPeriod(usize, Option<f32>),
    SetTrim(usize, f32),
    SetOnAir(usize, bool),
    /// Play/stop the live Realtime deck stream (gate the ring drain). `deck_stop`
    /// → `false`, `deck_play` → `true`.
    SetDeckPlaying(usize, bool),
    SetCue(usize, bool),
    SetCueMix(f32),
    /// Preview a decoded buffer into the cue/headphone feed (ADR-0027); replaces
    /// any current preview. Fire-and-forget like `LoadTrack` — the `Vec` is built
    /// off the render thread and the only verdict is "started".
    AuditionPlay(Vec<f32>),
    /// Stop the cue preview (ADR-0027).
    AuditionStop,
    LoadTrack(usize, Vec<f32>),
    UnloadTrack(usize),
    PlayTrack(usize),
    PauseTrack(usize),
    SeekTrack(usize, f64),
    SetTrackRate(usize, f64),
    NudgeTrackPhase(usize, f64),
    SetTrackLoop(usize, u64, u64),
    ClearTrackLoop(usize),
    CaptureLoop(usize, usize, f64),
    /// Play a loop slot; the `bool` is `layer` — replace the live stream (a freeze)
    /// or sum on top of it (a loaded sample, ADR-0022).
    PlayLoop(usize, usize, bool),
    StopLoop(usize),
    /// Stop one layered loop (the loaded-sample slot stops summing; ADR-0022).
    StopLayer(usize, usize),
    StopOneShot(usize),
    ClearLoop(usize, usize),
    /// Load a decoded pad/loop into a slot; whether the engine accepted it
    /// (`false` off Realtime, or a loop too short to install) is sent back over
    /// the reply channel so the caller learns the real verdict instead of
    /// polling the slot — a refusal is otherwise silent. The `Vec` is built off
    /// the render thread; the caller parks on the receiver, like `CaptureSample`.
    LoadGeneratedLoop(usize, usize, Vec<f32>, bool, Producer<bool>),
    /// Capture played history; the result (`Some(samples)` / `None`) is sent back
    /// over the enclosed reply channel. Built on the render thread (it allocates)
    /// and shipped to the caller, which is parked on the receiver.
    CaptureSample(usize, f64, Producer<Option<Vec<f32>>>),
    /// Read a loop slot's stored buffer (to persist a freeze/pad to the samples
    /// library); the cloned `Vec` (`None` for an empty slot) is sent back over the
    /// reply channel. Clones on the render thread, off the callback, like
    /// `CaptureSample`. The caller parks on the receiver.
    ReadLoopSlot(usize, usize, Producer<Option<Vec<f32>>>),
    /// Compute a loaded track's min/max envelope at `buckets` resolution; the
    /// result (`Some((min, max))` / `None` off Playback) is sent back over the
    /// reply channel. Allocates the envelope on the render thread (off the
    /// callback), like `CaptureSample`. The caller parks on the receiver.
    #[allow(clippy::type_complexity)]
    TrackPeaks(usize, usize, Producer<Option<(Vec<f32>, Vec<f32>)>>),
    /// Switch the MASTER output device: the render thread re-points its master
    /// ring producer to this fresh one (dropping the old, which closes the ring
    /// the about-to-be-replaced main stream was draining). The new device stream
    /// is opened on the matching consumer BEFORE this is sent, so a failed open
    /// never reaches here and audio is undisturbed. Built off the render thread
    /// (`Host::new_output_ring`) and moved in. The cue ring is left alone, so a
    /// cue-device switch never disturbs the master and vice versa.
    SwapMasterRing(Producer<f32>),
    /// Switch the CUE output device: the cue counterpart of [`SwapMasterRing`].
    SwapCueRing(Producer<f32>),
    /// Switch the MASTER and CUE producers as one queue operation. Combined
    /// output streams consume both rings, so accepting only one half would leave
    /// the other bus connected to a stream that is about to be dropped.
    SwapOutputRings {
        master: Producer<f32>,
        cue: Producer<f32>,
    },
}

/// A point-in-time copy of the per-deck state the IPC thread reads back: track
/// transport and loop-slot status. Updated by the render thread each block behind
/// a `Mutex` (held only by the render thread and the IPC thread — never the cpal
/// callback, which stays lock-free). Telemetry (underruns, ring fill, peaks, gain
/// reduction) is NOT here: it is already atomics on [`Telemetry`], read directly.
#[derive(Debug, Clone)]
struct Snapshot {
    /// Per-deck track transport, `None` when the deck is in Realtime mode.
    track_status: [Option<TrackStatus>; DECK_COUNT],
    /// Per-deck loop-slot status (filled / playing), length [`LOOP_SLOT_COUNT`].
    loop_slots: [[LoopSlotStatus; LOOP_SLOT_COUNT]; DECK_COUNT],
}

impl Snapshot {
    fn empty() -> Self {
        let empty_slot = LoopSlotStatus { filled: false, playing: false };
        Snapshot {
            track_status: [None; DECK_COUNT],
            loop_slots: [[empty_slot; LOOP_SLOT_COUNT]; DECK_COUNT],
        }
    }
}

/// A health snapshot the IPC `engine_telemetry` command returns: per-deck ring
/// fill + the shared underrun count + the master peak + the master limiter gain
/// reduction. BPM is intentionally absent for now (no tempo analysis yet). All of
/// these are read wait-free from the engine [`Telemetry`] atomics, so reading them
/// never perturbs the render thread or the callback.
#[derive(Debug, Clone, Copy)]
pub struct Health {
    /// Output-ring fill the callback sees, in frames — how much rendered audio is
    /// buffered ahead of the device. Distinct from the per-deck input rings.
    pub output_ring_frames: usize,
    /// Per-deck input-ring fill (frames) from [`Telemetry::ring_fill`].
    pub deck_ring_frames: [usize; DECK_COUNT],
    /// Total underruns counted on the input rings (the worklet definition).
    pub deck_underruns: u64,
    /// Output-ring underruns: callback blocks that found the output ring short.
    pub output_underruns: u64,
    /// Master peak magnitude since the last read (read-and-reset).
    pub master_peak: f32,
    /// Deepest master limiter gain reduction in dB (≤ 0) since the last read.
    pub master_gain_reduction_db: f32,
    /// Per-deck post-fader peak magnitude since the last read (read-and-reset) —
    /// the channel meters (`getLevel`).
    pub deck_levels: [f32; DECK_COUNT],
    /// Total frames rendered since start — the shared audio clock the UI
    /// extrapolates positions in (`getContextTime`). Seconds = `/ SAMPLE_RATE`.
    pub context_frames: u64,
}

/// The render thread's output producer + the host-side handles the device and the
/// IPC layer hold. Returned by [`Host::new`] alongside the [`DeckHandle`]s.
///
/// `OutputConsumer` is the device side of the output ring — the cpal callback's
/// SOLE reader. It is `Send` (one `rtrb::Consumer`) so it can be moved into the
/// callback, and counts an output underrun on a short drain.
pub struct OutputConsumer {
    consumer: Consumer<f32>,
    telemetry: Arc<Telemetry>,
}

impl OutputConsumer {
    /// **RT path (the cpal callback's body).** Drain up to `out.len()` samples
    /// from the output ring into `out`; zero-fill and count one output underrun if
    /// the ring is short. Wait-free: only `rtrb` pops and an atomic add — no
    /// alloc, no lock, no syscall, so it is safe under `assert_no_alloc`.
    #[inline]
    pub fn drain_into(&mut self, out: &mut [f32]) {
        let want = out.len();
        // Read whatever is available, up to `want` — never more than the ring
        // holds, so a slightly-short ring still hands over its good samples (it
        // does not throw the whole block away).
        let take = self.consumer.slots().min(want);
        let mut filled = 0;
        if take > 0 {
            if let Ok(chunk) = self.consumer.read_chunk(take) {
                let (a, b) = chunk.as_slices();
                out[..a.len()].copy_from_slice(a);
                out[a.len()..a.len() + b.len()].copy_from_slice(b);
                filled = a.len() + b.len();
                chunk.commit(filled);
            }
        }
        if filled < want {
            // Short read: zero the rest and count it. A persistently short output
            // ring means the render thread is not keeping up.
            for s in out[filled..].iter_mut() {
                *s = 0.0;
            }
            self.telemetry.note_output_underrun();
        }
    }

    /// Test-only: a fresh output ring's producer + consumer (with throwaway
    /// telemetry) so other modules' tests can drive `drain_into` against a real
    /// wait-free ring (e.g. the device-side resampler). `capacity_frames` is in
    /// stereo frames.
    #[cfg(test)]
    pub(crate) fn new_test_pair(capacity_frames: usize) -> (Producer<f32>, OutputConsumer) {
        let (tx, rx) = RingBuffer::<f32>::new(capacity_frames * CHANNELS as usize);
        (
            tx,
            OutputConsumer {
                consumer: rx,
                telemetry: Arc::new(Telemetry::new()),
            },
        )
    }
}

/// A freshly-built output ring producer (master OR cue) for an output-device
/// switch. Built off the render thread ([`Host::new_output_ring`]) and handed
/// back to it via [`Host::install_master_ring`]
/// / [`Host::install_cue_ring`] (or paired by [`Host::install_output_rings`])
/// once the new device stream is open on the matching consumer. Opaque: the
/// producer only travels from the host into the render thread.
pub struct OutputRing(Producer<f32>);

/// Owns the [`Engine`] on a dedicated render thread and exposes thread-safe
/// control. Construct with [`Host::new`]; drive control through its methods (each
/// enqueues a [`Command`] over the wait-free channel); read state back through
/// [`Host::health`], [`Host::track_status`], and [`Host::loop_slots`].
///
/// On `Drop` the render thread is signalled to stop and joined, so the `Engine`
/// is torn down cleanly.
pub struct Host {
    /// Wait-free command producer; the IPC thread's sole writer.
    commands: Mutex<Producer<Command>>,
    /// Read-back snapshot, updated by the render thread each block.
    snapshot: Arc<Mutex<Snapshot>>,
    /// Shared engine telemetry (atomics; readable from anywhere).
    telemetry: Arc<Telemetry>,
    /// Stop flag for the render thread.
    stop: Arc<AtomicBool>,
    /// Master-bus recorder; the render thread appends to it while active.
    recorder: Arc<Recorder>,
    /// The render thread; joined on `Drop`.
    render_thread: Option<JoinHandle<()>>,
}

/// Push one complete control operation onto the single-writer command ring.
/// Kept separate from `Host` so saturation behavior can be exercised without a
/// render thread racing the test's queue setup.
fn enqueue_command(commands: &Mutex<Producer<Command>>, command: Command) -> bool {
    let mut producer = match commands.lock() {
        Ok(producer) => producer,
        Err(poisoned) => poisoned.into_inner(),
    };
    match producer.push(command) {
        Ok(()) => true,
        Err(_) => {
            eprintln!("lsdj-host: command queue full — dropping a control command");
            false
        }
    }
}

impl Host {
    /// Build the engine, create its [`DECK_COUNT`] decks, KEEP the engine on a
    /// newly spawned render thread, and return the [`Host`], the device-side
    /// [`OutputConsumer`], and the per-deck [`DeckHandle`] producers.
    ///
    /// The `DeckHandle`s are the non-RT producer side of each deck's input ring —
    /// the sidecar PCM feed (a later step) moves them onto its transport thread
    /// and writes model output through `DeckHandle::post_pcm`. Until then the
    /// decks simply render silence (their rings stay empty), which is fine: the
    /// render thread keeps the output ring filled and control/read-back work
    /// headlessly with no device and no feed.
    pub fn new() -> (Host, OutputConsumer, OutputConsumer, [DeckHandle; DECK_COUNT]) {
        let mut engine = Engine::new();
        // create_deck returns the producer half; collect the two handles to hand
        // back to the caller (the sidecar feed).
        let handles: [DeckHandle; DECK_COUNT] =
            std::array::from_fn(|index| engine.create_deck(index));
        let telemetry = engine.telemetry();

        let (cmd_tx, cmd_rx) = RingBuffer::<Command>::new(COMMAND_QUEUE_DEPTH);
        let (out_tx, out_rx) = RingBuffer::<f32>::new(OUTPUT_RING_FRAMES * CHANNELS as usize);
        // The headphone-cue ring, same size as the master ring (Slice 5).
        let (cue_tx, cue_rx) = RingBuffer::<f32>::new(OUTPUT_RING_FRAMES * CHANNELS as usize);
        let snapshot = Arc::new(Mutex::new(Snapshot::empty()));
        let stop = Arc::new(AtomicBool::new(false));
        let recorder = Arc::new(Recorder::new());

        let output = OutputConsumer {
            consumer: out_rx,
            telemetry: telemetry.clone(),
        };
        let cue_output = OutputConsumer {
            consumer: cue_rx,
            telemetry: telemetry.clone(),
        };

        let render_thread = spawn_render_thread(
            engine,
            cmd_rx,
            out_tx,
            cue_tx,
            snapshot.clone(),
            stop.clone(),
            recorder.clone(),
        );

        let host = Host {
            commands: Mutex::new(cmd_tx),
            snapshot,
            telemetry,
            stop,
            recorder,
            render_thread: Some(render_thread),
        };
        (host, output, cue_output, handles)
    }

    /// Start recording the master bus (exactly the speaker feed — post-limiter and
    /// clip-guard), streaming it to the already-opened `file` as a 16-bit PCM WAV. The
    /// caller opens the file (atomically, at the trust boundary) so this layer never
    /// touches paths. Errors if a take is already in progress.
    pub fn start_recording(&self, file: File) -> Result<(), String> {
        self.recorder.start(file)
    }

    /// Stop recording: flush the remaining audio, patch the WAV header, and close
    /// the file. A no-op when not recording. Returns any write error from the take.
    pub fn stop_recording(&self) -> Result<(), String> {
        self.recorder.stop()
    }

    /// Build one fresh output ring for an output-device switch: the producer
    /// (carried in [`OutputRing`]) and its device-side [`OutputConsumer`] for the
    /// new cpal stream. The ring is symmetric — master vs cue is decided by which
    /// of [`install_master_ring`](Self::install_master_ring) /
    /// [`install_cue_ring`](Self::install_cue_ring) the caller installs it with,
    /// or by pairing both rings in
    /// [`install_output_rings`](Self::install_output_rings).
    /// The render thread keeps filling the CURRENT ring until that install swaps
    /// it, so the caller opens the new stream on this consumer FIRST and only
    /// installs on success, leaving audio undisturbed if the device fails to open.
    pub fn new_output_ring(&self) -> (OutputRing, OutputConsumer) {
        let (tx, rx) = RingBuffer::<f32>::new(OUTPUT_RING_FRAMES * CHANNELS as usize);
        (
            OutputRing(tx),
            OutputConsumer {
                consumer: rx,
                telemetry: self.telemetry.clone(),
            },
        )
    }

    /// Hand the new MASTER ring producer to the render thread (it swaps off the
    /// old master ring on its next command drain). Call AFTER the new device
    /// stream is open on the matching consumer. Returns false only if the command
    /// queue was momentarily full — the caller then treats the switch as failed
    /// and keeps the old stream (its ring is still being filled).
    pub fn install_master_ring(&self, ring: OutputRing) -> bool {
        self.send(Command::SwapMasterRing(ring.0))
    }

    /// Hand the new CUE ring producer to the render thread — the cue counterpart
    /// of [`install_master_ring`](Self::install_master_ring). Swapped independently
    /// of the master, so a cue-device switch never disturbs the master stream.
    pub fn install_cue_ring(&self, ring: OutputRing) -> bool {
        self.send(Command::SwapCueRing(ring.0))
    }

    /// Hand a combined stream's MASTER and CUE rings to the render thread in one
    /// command. The queue accepts both producers or neither; callers may only
    /// retire the previous split cue after this returns true.
    pub fn install_output_rings(&self, master: OutputRing, cue: OutputRing) -> bool {
        self.send(Command::SwapOutputRings {
            master: master.0,
            cue: cue.0,
        })
    }

    /// Enqueue a command for the render thread. Drops the command (logged) if the
    /// queue is momentarily full — a non-blocking control surface never stalls the
    /// caller (the UI/IPC thread). Returns whether the command was enqueued.
    fn send(&self, command: Command) -> bool {
        // The Mutex only serialises IPC callers against each other (the producer
        // half is single-writer); it is never touched by the cpal callback.
        enqueue_command(&self.commands, command)
    }

    // --- Control surface (one method per Engine control op) ---

    pub fn set_crossfade(&self, position: f32) {
        self.send(Command::SetCrossfade(position));
    }

    pub fn set_eq(&self, deck: usize, band: EqBand, value: f32) {
        self.send(Command::SetEq(deck, band, value));
    }

    pub fn set_volume(&self, deck: usize, gain: f32) {
        self.send(Command::SetVolume(deck, gain));
    }

    pub fn set_fx(&self, deck: usize, kind: FxKind) {
        self.send(Command::SetFx(deck, kind));
    }

    pub fn set_fx_amount(&self, deck: usize, amount: f32) {
        self.send(Command::SetFxAmount(deck, amount));
    }

    pub fn clear_fx(&self, deck: usize) {
        self.send(Command::ClearFx(deck));
    }

    /// Set a deck's gated beat period (ADR-0025) — the synced dub echo snaps
    /// to it; `None` (gate blank) reverts the echo to free-running.
    pub fn set_beat_period(&self, deck: usize, period_seconds: Option<f32>) {
        self.send(Command::SetBeatPeriod(deck, period_seconds));
    }

    pub fn set_trim(&self, deck: usize, db: f32) {
        self.send(Command::SetTrim(deck, db));
    }

    pub fn set_on_air(&self, deck: usize, on: bool) {
        self.send(Command::SetOnAir(deck, on));
    }

    /// Gate the live Realtime deck stream (the `deck_play`/`deck_stop` path).
    pub fn set_deck_playing(&self, deck: usize, playing: bool) {
        self.send(Command::SetDeckPlaying(deck, playing));
    }

    pub fn set_cue(&self, deck: usize, on: bool) {
        self.send(Command::SetCue(deck, on));
    }

    pub fn set_cue_mix(&self, position: f32) {
        self.send(Command::SetCueMix(position));
    }

    /// Preview a decoded interleaved-stereo buffer into the headphone/cue feed
    /// (ADR-0027). `samples` is built/owned by the caller off the render thread and
    /// MOVED into the command; replaces any current preview.
    pub fn audition_play(&self, samples: Vec<f32>) {
        self.send(Command::AuditionPlay(samples));
    }

    /// Stop the headphone preview (ADR-0027).
    pub fn audition_stop(&self) {
        self.send(Command::AuditionStop);
    }

    /// Load a decoded track onto a deck. `samples` is built/owned by the caller
    /// off the render thread and MOVED into the command; the render thread
    /// installs it and drops the previously-loaded buffer there, off the callback.
    pub fn load_track(&self, deck: usize, samples: Vec<f32>) {
        self.send(Command::LoadTrack(deck, samples));
    }

    pub fn unload_track(&self, deck: usize) {
        self.send(Command::UnloadTrack(deck));
    }

    pub fn play_track(&self, deck: usize) {
        self.send(Command::PlayTrack(deck));
    }

    pub fn pause_track(&self, deck: usize) {
        self.send(Command::PauseTrack(deck));
    }

    pub fn seek_track(&self, deck: usize, frames: f64) {
        self.send(Command::SeekTrack(deck, frames));
    }

    pub fn set_track_rate(&self, deck: usize, rate: f64) {
        self.send(Command::SetTrackRate(deck, rate));
    }

    pub fn nudge_track_phase(&self, deck: usize, frames: f64) {
        self.send(Command::NudgeTrackPhase(deck, frames));
    }

    pub fn set_track_loop(&self, deck: usize, start: u64, end: u64) {
        self.send(Command::SetTrackLoop(deck, start, end));
    }

    pub fn clear_track_loop(&self, deck: usize) {
        self.send(Command::ClearTrackLoop(deck));
    }

    pub fn capture_loop(&self, deck: usize, slot: usize, seconds: f64) {
        self.send(Command::CaptureLoop(deck, slot, seconds));
    }

    pub fn play_loop(&self, deck: usize, slot: usize, layer: bool) {
        self.send(Command::PlayLoop(deck, slot, layer));
    }

    pub fn stop_loop(&self, deck: usize) {
        self.send(Command::StopLoop(deck));
    }

    pub fn stop_layer(&self, deck: usize, slot: usize) {
        self.send(Command::StopLayer(deck, slot));
    }

    pub fn stop_one_shot(&self, deck: usize) {
        self.send(Command::StopOneShot(deck));
    }

    pub fn clear_loop(&self, deck: usize, slot: usize) {
        self.send(Command::ClearLoop(deck, slot));
    }

    /// Load a decoded loop/pad into a slot. Like [`Host::load_track`], `samples`
    /// is moved into the command and installed (and any old buffer dropped) on the
    /// render thread. Round-trips a reply (the `capture_sample` pattern) so the
    /// caller gets the engine's actual verdict — `true` accepted, `false` refused
    /// (off Realtime, or a loop too short to install) or the render thread is gone
    /// — rather than inferring success by polling the slot, which cannot tell a
    /// slow install from a silent refusal.
    pub fn load_generated_loop(
        &self,
        deck: usize,
        slot: usize,
        samples: Vec<f32>,
        one_shot: bool,
    ) -> bool {
        let (reply_tx, mut reply_rx) = RingBuffer::<bool>::new(CAPTURE_REPLY_DEPTH);
        if !self.send(Command::LoadGeneratedLoop(deck, slot, samples, one_shot, reply_tx)) {
            return false;
        }
        // Park until the render thread answers — a rare, explicit user action, so a
        // bounded spin-park keeps it off any RT path without a vanished render
        // thread hanging the call.
        for _ in 0..1000 {
            match reply_rx.pop() {
                Ok(accepted) => return accepted,
                Err(_) => thread::sleep(Duration::from_millis(1)),
            }
        }
        false
    }

    /// Capture the last `seconds` of played history on a Realtime deck (M15 style
    /// sampling). Round-trips through the render thread: a reply channel carries
    /// the captured interleaved-stereo `Vec` (or `None` below the floor / off
    /// Realtime) back to this caller, which parks until it arrives. Returns `None`
    /// if the render thread is gone or the command could not be enqueued.
    pub fn capture_sample(&self, deck: usize, seconds: f64) -> Option<Vec<f32>> {
        let (reply_tx, mut reply_rx) = RingBuffer::<Option<Vec<f32>>>::new(CAPTURE_REPLY_DEPTH);
        if !self.send(Command::CaptureSample(deck, seconds, reply_tx)) {
            return None;
        }
        // Park until the render thread answers. The capture is a rare, explicit
        // user action; a short spin-park keeps it off any RT path while not busy-
        // burning a core. Bounded so a vanished render thread cannot hang the call.
        for _ in 0..1000 {
            match reply_rx.pop() {
                Ok(result) => return result,
                Err(_) => thread::sleep(Duration::from_millis(1)),
            }
        }
        None
    }

    /// A loop slot's stored buffer (interleaved-stereo f32), for persisting a freeze
    /// / loaded pad to the generated-samples library. `None` for an empty slot, a bad
    /// index, or a vanished render thread. Round-trips through the render thread on
    /// the same parked-reply pattern as [`Host::capture_sample`] (the clone allocates
    /// off the callback); a rare, explicit action, so the bounded park is well off
    /// any hot path.
    pub fn read_loop_slot(&self, deck: usize, slot: usize) -> Option<Vec<f32>> {
        let (reply_tx, mut reply_rx) = RingBuffer::<Option<Vec<f32>>>::new(CAPTURE_REPLY_DEPTH);
        if !self.send(Command::ReadLoopSlot(deck, slot, reply_tx)) {
            return None;
        }
        for _ in 0..1000 {
            match reply_rx.pop() {
                Ok(result) => return result,
                Err(_) => thread::sleep(Duration::from_millis(1)),
            }
        }
        None
    }

    /// A loaded track's min/max envelope at `buckets` resolution (the waveform
    /// overview), or `None` off Playback / for a bad deck index. Round-trips
    /// through the render thread (the envelope allocates) on the same parked-reply
    /// pattern as [`Host::capture_sample`]; it is a rare action (one per track
    /// load), so the bounded park is well off any hot path.
    #[allow(clippy::type_complexity)]
    pub fn track_peaks(&self, deck: usize, buckets: usize) -> Option<(Vec<f32>, Vec<f32>)> {
        let (reply_tx, mut reply_rx) =
            RingBuffer::<Option<(Vec<f32>, Vec<f32>)>>::new(CAPTURE_REPLY_DEPTH);
        if !self.send(Command::TrackPeaks(deck, buckets, reply_tx)) {
            return None;
        }
        for _ in 0..1000 {
            match reply_rx.pop() {
                Ok(result) => return result,
                Err(_) => thread::sleep(Duration::from_millis(1)),
            }
        }
        None
    }

    // --- Read-back ---

    /// A health snapshot for the `engine_telemetry` IPC command. Reads the engine
    /// telemetry atomics (wait-free) plus the live output-ring fill. The peak and
    /// gain-reduction meters are read-and-reset (the UI samples them each frame).
    pub fn health(&self) -> Health {
        Health {
            output_ring_frames: self.telemetry.output_ring_frames(),
            deck_ring_frames: std::array::from_fn(|d| self.telemetry.ring_fill(d)),
            deck_underruns: self.telemetry.underruns(),
            output_underruns: self.telemetry.output_underruns(),
            master_peak: self.telemetry.take_master_peak(),
            master_gain_reduction_db: self.telemetry.take_master_gain_reduction_db(),
            deck_levels: std::array::from_fn(|d| self.telemetry.take_deck_peak(d)),
            context_frames: self.telemetry.frames_rendered(),
        }
    }

    /// The track transport for a deck, or `None` off Playback. Reads the snapshot
    /// the render thread publishes each block.
    ///
    /// # Panics
    /// Panics if `deck >= DECK_COUNT` (a caller programming error).
    pub fn track_status(&self, deck: usize) -> Option<TrackStatus> {
        assert!(deck < DECK_COUNT, "deck index {deck} out of range");
        let snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        snapshot.track_status[deck]
    }

    /// The loop-slot status for a deck (filled / playing per slot), length
    /// [`LOOP_SLOT_COUNT`]. Reads the published snapshot.
    ///
    /// # Panics
    /// Panics if `deck >= DECK_COUNT` (a caller programming error).
    pub fn loop_slots(&self, deck: usize) -> [LoopSlotStatus; LOOP_SLOT_COUNT] {
        assert!(deck < DECK_COUNT, "deck index {deck} out of range");
        let snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        snapshot.loop_slots[deck]
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Finish any in-flight take first, so its writer thread flushes and exits
        // rather than being orphaned when the render thread (its feed) stops.
        let _ = self.recorder.stop();
        // Signal the render thread and join it so the Engine is dropped cleanly.
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.render_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn the render thread: it owns the `Engine`, drains the command channel,
/// renders into the output ring at the pacing target, and publishes the snapshot.
fn spawn_render_thread(
    engine: Engine,
    commands: Consumer<Command>,
    output: Producer<f32>,
    cue_output: Producer<f32>,
    snapshot: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    recorder: Arc<Recorder>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("lsdj-render".into())
        .spawn(move || {
            let telemetry = engine.telemetry();
            let mut loop_state = RenderLoop {
                engine,
                commands,
                output,
                snapshot,
                telemetry,
                telemetry_set: false,
                block: vec![0.0f32; RENDER_BLOCK_FRAMES * CHANNELS as usize],
                cue_output,
                cue_block: vec![0.0f32; RENDER_BLOCK_FRAMES * CHANNELS as usize],
                recorder,
            };
            while !stop.load(Ordering::Relaxed) {
                if !loop_state.step() {
                    // Output ring is full enough: park briefly, then re-check (a
                    // command may also have arrived). This is the back-pressure
                    // that paces rendering to the target depth.
                    thread::sleep(RENDER_PARK);
                }
            }
        })
        .expect("failed to spawn lsdj render thread")
}

/// The render thread's owned state and per-iteration logic, factored out so the
/// drain + render + publish steps can be unit-tested via a manual [`pump`] without
/// a device or a spawned thread (see [`TestHost`]).
struct RenderLoop {
    engine: Engine,
    commands: Consumer<Command>,
    output: Producer<f32>,
    snapshot: Arc<Mutex<Snapshot>>,
    /// Shared engine telemetry: the render thread records the output-ring fill
    /// here each block (the callback records output underruns; both wait-free).
    telemetry: Arc<Telemetry>,
    telemetry_set: bool,
    /// Reusable render scratch; allocated once (the render thread allocates
    /// freely, but a per-block alloc would be wasteful), drained into the ring.
    block: Vec<f32>,
    /// The headphone-cue output ring producer (Slice 5) and its render scratch.
    /// Filled in lockstep with `block` via `render_with_cue`; the device drains it
    /// onto the cue channels (FLX4 phones). Not drained on a stereo device — the
    /// ring just fills and `push_all` drops the overflow, so the render thread
    /// never blocks on it.
    cue_output: Producer<f32>,
    cue_block: Vec<f32>,
    /// Master-bus recorder; each rendered master block is appended while active.
    recorder: Arc<Recorder>,
}

impl RenderLoop {
    /// Drain every pending command, applying each to the engine. Allocation is
    /// fine on this thread; this is where `set_eq` rebuilds nodes and `load_track`
    /// installs (and drops the old) buffer — all off the cpal callback.
    fn drain_commands(&mut self) {
        while let Ok(command) = self.commands.pop() {
            self.apply(command);
        }
    }

    /// Apply one command to the engine. The engine methods bounds-check the deck
    /// index themselves (panicking on a programming error); the IPC layer clamps
    /// indices before sending so a webview cannot trip those panics.
    fn apply(&mut self, command: Command) {
        match command {
            Command::SetCrossfade(p) => self.engine.set_crossfade(p),
            Command::SetEq(d, band, v) => self.engine.set_eq(d, band, v),
            Command::SetVolume(d, g) => self.engine.set_volume(d, g),
            Command::SetFx(d, kind) => self.engine.set_fx(d, kind),
            Command::SetFxAmount(d, a) => self.engine.set_fx_amount(d, a),
            Command::SetBeatPeriod(d, p) => self.engine.set_beat_period(d, p),
            Command::ClearFx(d) => self.engine.clear_fx(d),
            Command::SetTrim(d, db) => self.engine.set_trim(d, db),
            Command::SetOnAir(d, on) => self.engine.set_on_air(d, on),
            Command::SetDeckPlaying(d, playing) => self.engine.set_deck_playing(d, playing),
            Command::SetCue(d, on) => self.engine.set_cue(d, on),
            Command::SetCueMix(p) => self.engine.set_cue_mix(p),
            Command::AuditionPlay(samples) => self.engine.audition_play(samples),
            Command::AuditionStop => self.engine.audition_stop(),
            Command::LoadTrack(d, samples) => self.engine.load_track(d, samples),
            Command::UnloadTrack(d) => self.engine.unload_track(d),
            Command::PlayTrack(d) => self.engine.play_track(d),
            Command::PauseTrack(d) => self.engine.pause_track(d),
            Command::SeekTrack(d, f) => self.engine.seek_track(d, f),
            Command::SetTrackRate(d, r) => self.engine.set_track_rate(d, r),
            Command::NudgeTrackPhase(d, f) => self.engine.nudge_track_phase(d, f),
            Command::SetTrackLoop(d, s, e) => self.engine.set_track_loop(d, s, e),
            Command::ClearTrackLoop(d) => self.engine.clear_track_loop(d),
            Command::CaptureLoop(d, slot, secs) => {
                self.engine.capture_loop(d, slot, secs);
            }
            Command::PlayLoop(d, slot, layer) => {
                self.engine.play_loop(d, slot, layer);
            }
            Command::StopLoop(d) => self.engine.stop_loop(d),
            Command::StopLayer(d, slot) => self.engine.stop_layer(d, slot),
            Command::StopOneShot(d) => self.engine.stop_one_shot(d),
            Command::ClearLoop(d, slot) => self.engine.clear_loop(d, slot),
            Command::LoadGeneratedLoop(d, slot, samples, one_shot, mut reply) => {
                let accepted = self.engine.load_generated_loop(d, slot, samples, one_shot);
                // The caller is parked on the receiver; a full/closed reply queue
                // just means it gave up — drop the verdict silently.
                let _ = reply.push(accepted);
            }
            Command::CaptureSample(d, secs, mut reply) => {
                let captured = self.engine.capture_sample(d, secs);
                // The caller is parked on the receiver; a full/closed reply queue
                // just means the caller gave up — drop the result silently.
                let _ = reply.push(captured);
            }
            Command::ReadLoopSlot(d, slot, mut reply) => {
                let samples = self.engine.read_loop_slot(d, slot);
                // As CaptureSample: the parked caller may have given up; drop silently.
                let _ = reply.push(samples);
            }
            Command::TrackPeaks(d, buckets, mut reply) => {
                let peaks = self.engine.get_track_peaks(d, buckets);
                let _ = reply.push(peaks);
            }
            Command::SwapMasterRing(output) => {
                // Re-point the render thread at the new master ring. The old
                // producer drops here (off the cpal callback), closing the ring
                // the previous main stream drained — harmless, it is being
                // dropped. The next `step` fills the new ring.
                self.output = output;
            }
            Command::SwapCueRing(cue) => {
                // As above, for the cue ring — swapped independently of the master
                // so a cue-device change never disturbs the master stream.
                self.cue_output = cue;
            }
            Command::SwapOutputRings { master, cue } => {
                // A combined device drains both rings. Apply the pair from one
                // command so queue pressure can never expose a half-swapped
                // topology to the render loop or its device stream.
                self.output = master;
                self.cue_output = cue;
            }
        }
    }

    /// Publish the per-deck read-back snapshot from the current engine state.
    /// Called after a render block so a UI read sees fresh transport/loop state.
    fn publish_snapshot(&self) {
        let track_status = std::array::from_fn(|d| self.engine.get_track_status(d));
        let loop_slots = std::array::from_fn(|d| {
            let slots = self.engine.loop_slots(d);
            std::array::from_fn(|s| slots[s])
        });
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        snapshot.track_status = track_status;
        snapshot.loop_slots = loop_slots;
    }

    /// One render-loop iteration: drain commands, then — if the output ring is
    /// below the pacing target — render one block into it and publish the
    /// snapshot. Returns `true` if a block was rendered (keep going immediately),
    /// `false` if the ring is full enough (the caller parks).
    fn step(&mut self) -> bool {
        self.drain_commands();

        let target_samples = OUTPUT_RING_TARGET_FRAMES * CHANNELS as usize;
        let buffered = self.output.buffer().capacity() - self.output.slots();
        self.telemetry
            .record_output_ring_fill(buffered / CHANNELS as usize);
        if buffered >= target_samples {
            // Even when not rendering, keep the read-back fresh so transport state
            // (e.g. a track ending) surfaces promptly while the ring coasts.
            self.publish_snapshot();
            return false;
        }

        // Render one block and push as much of it as fits into the output ring.
        if !self.telemetry_set {
            // FTZ/DAZ on the RENDER thread too: the denormal tail lives in
            // render(), not the drain. Harmless to set on a non-callback thread.
            crate::device::set_ftz_daz();
            self.telemetry_set = true;
        }
        self.engine
            .render_with_cue(&mut self.block, &mut self.cue_block, RENDER_BLOCK_FRAMES);
        push_all(&mut self.output, &self.block);
        push_all(&mut self.cue_output, &self.cue_block);
        // Tap the rendered master into the recorder if it is recording (no-op
        // otherwise; runs on the render thread, never the cpal callback).
        self.recorder.capture(&self.block);
        self.publish_snapshot();
        true
    }
}

/// Push the whole slice into the ring, writing only the prefix that fits (the ring
/// is sized well above the pacing target, so in steady state it always fits). Any
/// overflow is dropped rather than blocking the render thread — the same
/// non-blocking discipline as the input rings' `post_pcm`.
fn push_all(output: &mut Producer<f32>, samples: &[f32]) {
    let want = samples.len().min(output.slots());
    if want == 0 {
        return;
    }
    if let Ok(chunk) = output.write_chunk_uninit(want) {
        let n = chunk.len();
        chunk.fill_from_iter(samples[..n].iter().copied());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A testable host owning the `Engine`, the command channel, the output ring,
    /// and the snapshot directly (no spawned thread, no device), exposing a manual
    /// [`TestHost::pump`] that drains commands and renders one block.
    ///
    /// This is how the command application is verified headlessly: submit
    /// commands, pump, then assert the engine state changed via the read-back. It
    /// mirrors exactly what the spawned render thread does each iteration.
    struct TestHost {
        commands: Producer<Command>,
        loop_state: RenderLoop,
        deck_handles: Vec<DeckHandle>,
    }

    impl TestHost {
        fn new() -> Self {
            let mut engine = Engine::new();
            let deck_handles: Vec<DeckHandle> =
                (0..DECK_COUNT).map(|i| engine.create_deck(i)).collect();
            let telemetry = engine.telemetry();
            let (cmd_tx, cmd_rx) = RingBuffer::<Command>::new(COMMAND_QUEUE_DEPTH);
            let (out_tx, out_rx) =
                RingBuffer::<f32>::new(OUTPUT_RING_FRAMES * CHANNELS as usize);
            let (cue_tx, cue_rx) =
                RingBuffer::<f32>::new(OUTPUT_RING_FRAMES * CHANNELS as usize);
            // Leak the consumers for the lifetime of the test so the ring producers
            // stay valid without a device draining them (full-ring is fine —
            // push_all just drops the overflow).
            std::mem::forget(out_rx);
            std::mem::forget(cue_rx);
            let snapshot = Arc::new(Mutex::new(Snapshot::empty()));
            TestHost {
                commands: cmd_tx,
                loop_state: RenderLoop {
                    engine,
                    commands: cmd_rx,
                    output: out_tx,
                    snapshot,
                    telemetry,
                    telemetry_set: true, // skip FTZ/DAZ in the test
                    block: vec![0.0f32; RENDER_BLOCK_FRAMES * CHANNELS as usize],
                    cue_output: cue_tx,
                    cue_block: vec![0.0f32; RENDER_BLOCK_FRAMES * CHANNELS as usize],
                    recorder: Arc::new(Recorder::new()),
                },
                deck_handles,
            }
        }

        fn send(&mut self, command: Command) {
            self.commands.push(command).expect("command enqueued");
        }

        /// Drain commands and render exactly one block (bypassing the pacing gate),
        /// then publish the snapshot — what the render thread does per iteration.
        fn pump(&mut self) {
            self.loop_state.drain_commands();
            self.loop_state
                .engine
                .render(&mut self.loop_state.block, RENDER_BLOCK_FRAMES);
            self.loop_state.publish_snapshot();
        }

        fn track_status(&self, deck: usize) -> Option<TrackStatus> {
            self.loop_state
                .snapshot
                .lock()
                .unwrap()
                .track_status[deck]
        }

        fn loop_slots(&self, deck: usize) -> [LoopSlotStatus; LOOP_SLOT_COUNT] {
            self.loop_state.snapshot.lock().unwrap().loop_slots[deck]
        }
    }

    /// A small ramp track buffer: frame f maps to a tiny ramp value in both
    /// channels, sub-threshold so the limiter stays idle.
    fn ramp_track(frames: usize) -> Vec<f32> {
        let mut buf = vec![0.0f32; frames * CHANNELS as usize];
        for f in 0..frames {
            let s = 0.2 * (f as f32 / frames as f32);
            buf[2 * f] = s;
            buf[2 * f + 1] = s;
        }
        buf
    }

    /// load_track + play_track applied through the command channel: after a pump,
    /// the read-back snapshot reports the deck Playing — the core "commands reach
    /// the engine and the read-back reflects it" proof.
    #[test]
    fn load_and_play_track_reflected_in_snapshot() {
        let mut host = TestHost::new();
        assert!(host.track_status(0).is_none(), "deck starts Realtime");

        host.send(Command::LoadTrack(0, ramp_track(2_000)));
        host.send(Command::PlayTrack(0));
        host.pump();

        let status = host.track_status(0).expect("deck is now Playback");
        assert!(status.playing, "the track is playing after load+play");
        assert_eq!(status.duration_frames, 2_000);
    }

    /// set_crossfade applied through the channel shifts the mix: with a Playing
    /// track fully on deck A vs fully on deck B, the rendered output is non-silent
    /// only when the crossfader favours the deck holding the track.
    #[test]
    fn set_crossfade_shifts_the_mix() {
        let mut host = TestHost::new();
        host.send(Command::LoadTrack(0, ramp_track(20_000)));
        host.send(Command::PlayTrack(0));
        // Full deck A: the track (on A) is audible.
        host.send(Command::SetCrossfade(0.0));
        host.pump();
        let energy_a: f64 = host
            .loop_state
            .block
            .iter()
            .map(|&s| (s * s) as f64)
            .sum();
        assert!(energy_a > 1e-6, "track audible with the crossfader on its deck");

        // Full deck B: deck A's track is faded out → near silence from A's content.
        host.send(Command::SetCrossfade(1.0));
        // Pump several blocks so the equal-power gain fully takes and the ramp
        // climbs; deck B has no source, so the mix collapses toward silence.
        for _ in 0..4 {
            host.pump();
        }
        let energy_b: f64 = host
            .loop_state
            .block
            .iter()
            .map(|&s| (s * s) as f64)
            .sum();
        assert!(
            energy_b < energy_a,
            "crossfading away from the track's deck drops its level (a={energy_a}, b={energy_b})"
        );
    }

    /// set_eq applied through the channel reaches the engine: killing a deck's low
    /// band attenuates a low tone fed to that deck, vs the same deck flat.
    #[test]
    fn set_eq_reaches_the_engine() {
        // Measure a deck-A low tone's RMS with the low band flat vs killed, driving
        // the feed + render through the host command channel.
        fn measure(kill: bool) -> f64 {
            let mut host = TestHost::new();
            host.send(Command::SetCrossfade(0.0)); // full deck A
            if kill {
                host.send(Command::SetEq(0, EqBand::Low, 0.0));
            }
            host.loop_state.drain_commands();

            // Feed a low sine into deck A's ring and render past the prebuffer.
            let mut handle = host.deck_handles.remove(0);
            let freq = 60.0f32;
            let amp = 0.2f32;
            let mut phase = 0.0f32;
            let dphase = 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE as f32;
            let prime = crate::PREBUFFER_FRAMES + 20 * RENDER_BLOCK_FRAMES;
            let mut buf = vec![0.0f32; prime * CHANNELS as usize];
            for f in 0..prime {
                let s = phase.sin() * amp;
                phase += dphase;
                buf[2 * f] = s;
                buf[2 * f + 1] = s;
            }
            handle.post_pcm(&buf);

            // Skip the EQ settling transient, then measure several blocks.
            for _ in 0..10 {
                host.pump();
            }
            let mut sum_sq = 0.0f64;
            let mut n = 0u64;
            for _ in 0..16 {
                host.pump();
                for f in 0..RENDER_BLOCK_FRAMES {
                    let l = host.loop_state.block[2 * f] as f64;
                    sum_sq += l * l;
                    n += 1;
                }
            }
            (sum_sq / n as f64).sqrt()
        }

        let flat = measure(false);
        let killed = measure(true);
        assert!(flat > 1e-4, "the flat low tone is audible, rms {flat}");
        let db = 20.0 * (killed / flat).log10();
        assert!(db < -15.0, "killing the low band attenuates the low tone, got {db:.1} dB");
    }

    /// The capture family routes through the channel: feed + play a deck, capture a
    /// loop into a slot, play it — the snapshot reports the slot filled then
    /// playing.
    #[test]
    fn capture_and_play_loop_reflected_in_snapshot() {
        let mut host = TestHost::new();

        // Feed a steady sine and render ~1.5 s so the played history has content.
        let mut handle = host.deck_handles.remove(0);
        let mut phase = 0.0f32;
        let dphase = 2.0 * std::f32::consts::PI * 220.0 / SAMPLE_RATE as f32;
        let chunk = SAMPLE_RATE as usize / 2; // 0.5 s per refeed
        let mut buf = vec![0.0f32; chunk * CHANNELS as usize];
        let fill = |phase: &mut f32, buf: &mut [f32]| {
            for f in 0..chunk {
                let s = phase.sin() * 0.2;
                *phase += dphase;
                buf[2 * f] = s;
                buf[2 * f + 1] = s;
            }
        };
        // Prime + play ~2 s.
        fill(&mut phase, &mut buf);
        handle.post_pcm(&buf);
        for _ in 0..(2 * SAMPLE_RATE as usize / RENDER_BLOCK_FRAMES) {
            if host.loop_state.output.slots() == 0 {
                // shouldn't happen (consumer leaked), but guard anyway
            }
            fill(&mut phase, &mut buf);
            handle.post_pcm(&buf);
            host.pump();
        }

        // Capture 1 s into slot 0, then pump so the command applies.
        host.send(Command::CaptureLoop(0, 0, 1.0));
        host.pump();
        assert!(host.loop_slots(0)[0].filled, "slot 0 filled after capture");
        assert!(!host.loop_slots(0)[0].playing, "not playing until play_loop");

        host.send(Command::PlayLoop(0, 0, false));
        host.pump();
        assert!(host.loop_slots(0)[0].playing, "slot 0 plays after play_loop");
    }

    /// capture_sample round-trips a reply: a CaptureSample command with a reply
    /// channel returns the captured samples (or None below the floor) on pump.
    #[test]
    fn capture_sample_replies_with_samples() {
        let mut host = TestHost::new();

        // No history yet: a style sample is refused (None).
        let (reply_tx, mut reply_rx) = RingBuffer::<Option<Vec<f32>>>::new(CAPTURE_REPLY_DEPTH);
        host.send(Command::CaptureSample(0, 10.0, reply_tx));
        host.pump();
        assert_eq!(reply_rx.pop().unwrap(), None, "no history → None");

        // Feed + play ~5 s, then a 4 s capture returns interleaved-stereo samples.
        let mut handle = host.deck_handles.remove(0);
        let chunk = SAMPLE_RATE as usize / 2;
        let mut buf = vec![0.0f32; chunk * CHANNELS as usize];
        let mut counter = 0u64;
        let fill = |counter: &mut u64, buf: &mut [f32]| {
            for f in 0..chunk {
                let v = *counter as f32;
                buf[2 * f] = v;
                buf[2 * f + 1] = -v;
                *counter += 1;
            }
        };
        fill(&mut counter, &mut buf);
        handle.post_pcm(&buf);
        for _ in 0..(5 * SAMPLE_RATE as usize / RENDER_BLOCK_FRAMES) {
            fill(&mut counter, &mut buf);
            handle.post_pcm(&buf);
            host.pump();
        }

        let (reply_tx, mut reply_rx) = RingBuffer::<Option<Vec<f32>>>::new(CAPTURE_REPLY_DEPTH);
        host.send(Command::CaptureSample(0, 4.0, reply_tx));
        host.pump();
        let captured = reply_rx.pop().unwrap().expect("4 s clears the floor");
        let want = (4.0 * SAMPLE_RATE as f64) as usize * CHANNELS as usize;
        assert_eq!(captured.len(), want, "captured 4 s of interleaved stereo");
    }

    /// read_loop_slot round-trips a reply: an empty slot replies `None`; after a
    /// generated pad fills the slot it replies the stored interleaved buffer (the
    /// audio the samples library persists for a freeze/pad).
    #[test]
    fn read_loop_slot_replies_with_the_stored_buffer() {
        let mut host = TestHost::new();

        // Empty slot → None.
        let (reply_tx, mut reply_rx) = RingBuffer::<Option<Vec<f32>>>::new(CAPTURE_REPLY_DEPTH);
        host.send(Command::ReadLoopSlot(0, 0, reply_tx));
        host.pump();
        assert_eq!(reply_rx.pop().unwrap(), None, "empty slot → None");

        // Load a one-shot pad (stored verbatim — no fold), then read it back.
        let pad = vec![0.3f32; 4_800 * CHANNELS as usize]; // 0.1 s stereo
        host.send(Command::LoadGeneratedLoop(
            0,
            0,
            pad.clone(),
            true,
            RingBuffer::<bool>::new(CAPTURE_REPLY_DEPTH).0,
        ));
        host.pump();

        let (reply_tx, mut reply_rx) = RingBuffer::<Option<Vec<f32>>>::new(CAPTURE_REPLY_DEPTH);
        host.send(Command::ReadLoopSlot(0, 0, reply_tx));
        host.pump();
        let read = reply_rx.pop().unwrap().expect("a filled slot reads back");
        assert_eq!(read, pad, "a one-shot reads back its verbatim buffer");
    }

    /// A full spawned Host drives end-to-end with no device: build it, send
    /// control, and read the state back through the public API. Proves the render
    /// thread actually applies commands and publishes the snapshot.
    #[test]
    fn spawned_host_applies_commands_headless() {
        let (host, _output, _cue_output, mut handles) = Host::new();

        // load_track + play_track through the public control surface.
        host.load_track(1, ramp_track(10_000));
        host.play_track(1);
        // Give the render thread a few loop iterations to drain + render + publish.
        let mut status = None;
        for _ in 0..200 {
            if let Some(s) = host.track_status(1) {
                if s.playing {
                    status = Some(s);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        let status = status.expect("the spawned render thread should report the track playing");
        assert_eq!(status.duration_frames, 10_000);

        // The deck handles came back for the sidecar feed; posting to one must not
        // panic and the ring accepts it.
        let written = handles[0].post_pcm(&[0.0f32; 512]);
        assert_eq!(written, 512, "the returned DeckHandle feeds deck 0's ring");

        // Telemetry reads back without a device.
        let health = host.health();
        assert_eq!(health.deck_underruns, host.telemetry.underruns());

        drop(host); // joins the render thread cleanly
    }

    /// `load_generated_loop` round-trips the engine's verdict through the spawned
    /// render thread: a one-shot pad is accepted and fills the slot on a fresh
    /// (Realtime) deck — AND on a Playback deck, since a loaded sample is a
    /// self-contained overlay that layers over the track (ADR-0022). The reply
    /// channel (vs the old fire-and-forget) is what lets the webview show the real
    /// verdict instead of guessing "could not be decoded".
    #[test]
    fn load_generated_loop_reports_its_verdict() {
        let (host, _output, _cue_output, _handles) = Host::new();

        // A fresh deck is Realtime: a one-shot pad is accepted and the slot fills.
        let pad = vec![0.25f32; 4_800 * CHANNELS as usize]; // 0.1 s stereo
        assert!(
            host.load_generated_loop(0, 0, pad.clone(), true),
            "a one-shot pad loads onto a Realtime deck",
        );
        let mut filled = false;
        for _ in 0..200 {
            if host.loop_slots(0)[0].filled {
                filled = true;
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(filled, "the accepted pad fills the slot in the snapshot");

        // Switching the deck to Playback no longer refuses the load — the sample
        // layers over the track, so the verdict comes back accepted there too.
        host.load_track(0, ramp_track(10_000));
        for _ in 0..200 {
            if host.track_status(0).is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            host.load_generated_loop(0, 1, pad, true),
            "a Playback deck accepts a generated pad too",
        );
        let mut filled_on_playback = false;
        for _ in 0..200 {
            if host.loop_slots(0)[1].filled {
                filled_on_playback = true;
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(filled_on_playback, "the pad fills a slot on the Playback deck");

        drop(host);
    }

    #[test]
    fn capture_converts_a_master_block_to_int16_when_active() {
        let rec = Recorder::new();

        // Not recording → capture is a no-op.
        rec.capture(&[0.5, -0.5]);
        assert!(rec.buffer.lock().unwrap().is_empty());

        // Active → blocks are clamped and scaled to int16 in the shared buffer (the
        // writer thread streams that buffer to disk).
        rec.active.store(true, Ordering::Relaxed);
        rec.capture(&[0.5, -0.5]);
        rec.capture(&[2.0, -2.0]); // out of range → clamped to ±full scale
        let buf = rec.buffer.lock().unwrap();
        // 0.5 → round(0.5 * 32767) ≈ 16383; clamp pins the overshoot at ±32767.
        assert!((buf[0] - 16383).abs() <= 1, "0.5 should encode near +16383, got {}", buf[0]);
        assert!((buf[1] + 16383).abs() <= 1);
        assert_eq!(buf[2], 32767);
        assert_eq!(buf[3], -32767);
    }

    #[test]
    fn capture_stops_at_the_wav_size_ceiling() {
        // Past ~6 h a plain WAV's 32-bit size fields would overflow; capture must stop
        // appending at the ceiling rather than wrap the header into a malformed file.
        let rec = Recorder::new();
        rec.active.store(true, Ordering::Relaxed);
        // Pretend all but two samples of the budget are already on disk.
        rec.captured.store(RECORDING_MAX_SAMPLES - 2, Ordering::Relaxed);

        rec.capture(&[0.1, 0.2, 0.3, 0.4]); // a 4-sample block, only 2 of which fit
        assert_eq!(rec.buffer.lock().unwrap().len(), 2, "only the remaining budget is kept");
        assert_eq!(rec.captured.load(Ordering::Relaxed), RECORDING_MAX_SAMPLES);

        rec.capture(&[0.5, 0.6]); // budget exhausted → nothing more is captured
        assert_eq!(rec.buffer.lock().unwrap().len(), 2, "capture is a no-op at the ceiling");
    }

    #[test]
    fn drain_to_wav_streams_a_patched_pcm_wav() {
        use std::io::Cursor;

        // Two stereo frames already captured, stop already requested: drain writes
        // the header, streams the samples, and patches the sizes — all synchronously
        // (the empty-buffer-while-stopping path breaks the loop, no sleeping).
        let buffer = Mutex::new(vec![16383i16, -16383, 8191, -8191]);
        let stop = AtomicBool::new(true);
        let mut out = Cursor::new(Vec::new());
        drain_to_wav(&buffer, &stop, &mut out).expect("drain succeeds");
        let wav = out.into_inner();

        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        // 44-byte header + 4 int16 samples (8 bytes).
        assert_eq!(wav.len(), 44 + 8);
        // Sizes were patched from the placeholder 0: RIFF = 36 + data, data = 8.
        assert_eq!(u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]), 36 + 8);
        assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 8);
        assert_eq!(i16::from_le_bytes([wav[44], wav[45]]), 16383);
        // The buffer was drained.
        assert!(buffer.lock().unwrap().is_empty());

        // Nothing captured → a valid header-only WAV with zero-length data.
        let empty_buffer = Mutex::new(Vec::new());
        let mut empty_out = Cursor::new(Vec::new());
        drain_to_wav(&empty_buffer, &AtomicBool::new(true), &mut empty_out).unwrap();
        let empty = empty_out.into_inner();
        assert_eq!(empty.len(), 44, "no samples → header only");
        assert_eq!(u32::from_le_bytes([empty[40], empty[41], empty[42], empty[43]]), 0);
    }

    /// Switching the MAIN output device: a `SwapMasterRing` command re-points the
    /// render loop's master producer, so the next render fills the NEW master ring
    /// (the hand-off `set_main_device` relies on).
    #[test]
    fn swap_master_ring_repoints_the_render_loop() {
        let mut host = TestHost::new();
        // The new device's master ring; keep the consumer to observe the hand-off.
        let (out_tx, out_rx) = RingBuffer::<f32>::new(OUTPUT_RING_FRAMES * CHANNELS as usize);
        assert_eq!(out_rx.slots(), 0, "new master ring starts empty");

        host.send(Command::SwapMasterRing(out_tx));
        // step() drains the swap, then renders + pushes into the now-current ring.
        host.loop_state.step();

        assert!(
            out_rx.slots() > 0,
            "after the swap the render loop fills the new master ring"
        );
    }

    /// Switching the CUE output device: a `SwapCueRing` command re-points only the
    /// cue producer (the master ring is left alone), so the next render fills the
    /// NEW cue ring — the independent-swap hand-off `set_cue_device` relies on.
    #[test]
    fn swap_cue_ring_repoints_the_render_loop() {
        let mut host = TestHost::new();
        let (cue_tx, cue_rx) = RingBuffer::<f32>::new(OUTPUT_RING_FRAMES * CHANNELS as usize);
        assert_eq!(cue_rx.slots(), 0, "new cue ring starts empty");

        host.send(Command::SwapCueRing(cue_tx));
        host.loop_state.step();

        assert!(
            cue_rx.slots() > 0,
            "after the swap the render loop fills the new cue ring"
        );
    }

    #[test]
    fn swap_output_rings_repoints_master_and_cue_together() {
        let mut host = TestHost::new();
        let (master_tx, master_rx) =
            RingBuffer::<f32>::new(OUTPUT_RING_FRAMES * CHANNELS as usize);
        let (cue_tx, cue_rx) = RingBuffer::<f32>::new(OUTPUT_RING_FRAMES * CHANNELS as usize);

        host.send(Command::SwapOutputRings {
            master: master_tx,
            cue: cue_tx,
        });
        host.loop_state.step();

        assert!(master_rx.slots() > 0, "the new master ring is filled");
        assert!(cue_rx.slots() > 0, "the new cue ring is filled");
    }

    #[test]
    fn combined_ring_install_uses_one_queue_slot() {
        let (mut command_tx, mut command_rx) =
            RingBuffer::<Command>::new(COMMAND_QUEUE_DEPTH);
        for _ in 0..COMMAND_QUEUE_DEPTH - 1 {
            assert!(command_tx.push(Command::SetCrossfade(0.5)).is_ok());
        }
        let (master, _master_rx) = RingBuffer::<f32>::new(1);
        let (cue, _cue_rx) = RingBuffer::<f32>::new(1);
        let commands = Mutex::new(command_tx);

        assert!(enqueue_command(
            &commands,
            Command::SwapOutputRings { master, cue }
        ));
        for _ in 0..COMMAND_QUEUE_DEPTH - 1 {
            assert!(matches!(command_rx.pop(), Ok(Command::SetCrossfade(_))));
        }
        assert!(matches!(
            command_rx.pop(),
            Ok(Command::SwapOutputRings { .. })
        ));
        assert!(command_rx.pop().is_err());
    }

    #[test]
    fn full_queue_rejects_the_combined_ring_pair_without_partial_commands() {
        let (mut command_tx, mut command_rx) =
            RingBuffer::<Command>::new(COMMAND_QUEUE_DEPTH);
        for _ in 0..COMMAND_QUEUE_DEPTH {
            assert!(command_tx.push(Command::SetCrossfade(0.5)).is_ok());
        }
        let (master, _master_rx) = RingBuffer::<f32>::new(1);
        let (cue, _cue_rx) = RingBuffer::<f32>::new(1);
        let commands = Mutex::new(command_tx);

        assert!(!enqueue_command(
            &commands,
            Command::SwapOutputRings { master, cue }
        ));
        for _ in 0..COMMAND_QUEUE_DEPTH {
            assert!(matches!(command_rx.pop(), Ok(Command::SetCrossfade(_))));
        }
        assert!(command_rx.pop().is_err());
    }
}
