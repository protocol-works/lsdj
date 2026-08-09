//! Per-deck Python inference sidecar supervision (Phase 2 part 4, ADR-0019).
//!
//! The Rust shell spawns one Python sidecar per deck (replacing `controller.py`'s
//! `DeckProcess`), connected over **loopback TCP** — the transport Spike A chose
//! (`docs/spike-rust-audio.md`; `127.0.0.1`, `TCP_NODELAY`, beat UDS on every
//! percentile under inference load). The sidecar runs the unchanged
//! `run_deck_worker` generation loop (`backend/lsdj/worker.py`) with its
//! queues bridged to the socket.
//!
//! # Wire protocol
//!
//! Type-tagged, length-prefixed frames in both directions on the one socket —
//! the Spike-A `u32`-length framing plus a one-byte type so PCM, status, and
//! control share the stream:
//!
//! ```text
//! [u8 type][u32 little-endian length][length bytes payload]
//! ```
//!
//! - [`FRAME_PCM`] (sidecar → engine): interleaved-stereo f32 LE @ 48 kHz, the
//!   `('audio', bytes)` worker output → [`DeckHandle::post_pcm`].
//! - [`FRAME_STATUS`] (sidecar → engine): UTF-8 JSON, the `('status', dict)`
//!   worker output → a Tauri event the webview subscribes to.
//! - [`FRAME_CONTROL`] (engine → sidecar): UTF-8 JSON, a deck command
//!   (`play`/`stop`/`set_style`/…) the webview drove over IPC.
//!
//! # Testability
//!
//! The protocol ([`write_frame`]/[`read_frame`]) and the read loop
//! ([`run_reader`]) are decoupled from the process spawn: a test drives a real
//! `TcpStream` pair (or any `Read`/`Write`) and asserts PCM reaches a
//! `DeckHandle` and status reaches a sink — no Python, no models. The full
//! model-loaded round-trip is a native-checklist item.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lsdj_engine::DeckHandle;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::analysis::live::AnalysisFeed;
use crate::child_process::SupervisedChild;

/// Per-deck analysis taps: a webview [`Channel`] each deck's realtime PCM is teed
/// to (gap 1). The TS beat/loudness/band analysis (ADR-0017: stays in TypeScript)
/// no longer receives model PCM over a WebSocket in the native shell, so the
/// sidecar reader hands the same raw frames back to the webview here. Cloneable
/// (an `Arc`) so the reader-thread tap closures and the IPC subscribe commands
/// share the per-deck slots; held in Tauri managed state.
#[derive(Clone)]
pub struct PcmTaps {
    decks: Arc<Vec<Mutex<Option<Channel<InvokeResponseBody>>>>>,
}

impl PcmTaps {
    pub fn new(deck_count: usize) -> Self {
        PcmTaps {
            decks: Arc::new((0..deck_count).map(|_| Mutex::new(None)).collect()),
        }
    }

    /// Set (or clear, with `None`) the subscriber channel for a deck. A second
    /// subscribe replaces the first (one subscriber per deck — the one `useDeck`).
    pub fn set(&self, deck: usize, channel: Option<Channel<InvokeResponseBody>>) {
        if let Some(slot) = self.decks.get(deck) {
            *slot.lock().unwrap_or_else(|p| p.into_inner()) = channel;
        }
    }

    /// Tee raw interleaved-stereo f32 LE PCM bytes to a deck's subscriber (a no-op
    /// if none). Called from the NON-RT sidecar reader thread (never the cpal
    /// callback). Drops the subscriber on a send error so a dead webview channel
    /// never wedges the reader.
    pub fn send(&self, deck: usize, bytes: &[u8]) {
        let Some(slot) = self.decks.get(deck) else {
            return;
        };
        // Clone the channel handle out from UNDER the lock, then send without
        // holding it. `channel.send` delivers to the webview (it needs the main
        // thread), and the main thread also takes this lock to (un)subscribe — so
        // holding the lock across the send deadlocks the reader (holding the lock,
        // awaiting the webview) against a subscribe on the main thread (awaiting the
        // lock). That wedged the model-switch reader join: the reader never exited,
        // so the restart hung holding the deck-slot mutex.
        let channel = slot.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(channel) = channel {
            if channel
                .send(InvokeResponseBody::Raw(bytes.to_vec()))
                .is_err()
            {
                *slot.lock().unwrap_or_else(|p| p.into_inner()) = None;
            }
        }
    }
}

/// Sidecar → engine: interleaved-stereo f32 LE PCM (the `('audio', …)` output).
pub const FRAME_PCM: u8 = 1;
/// Sidecar → engine: UTF-8 JSON status (the `('status', …)` output).
pub const FRAME_STATUS: u8 = 2;
/// Engine → sidecar: UTF-8 JSON deck control (`play`/`stop`/`set_style`/…).
pub const FRAME_CONTROL: u8 = 3;
/// Engine → sidecar: a style-sample embed (M15). Binary, not JSON, because it
/// carries raw PCM: `[u32 LE id length][id utf-8][interleaved f32 LE PCM]`.
pub const FRAME_EMBED: u8 = 4;
/// Sidecar -> engine: the per-launch capability. This must be the first frame.
pub const FRAME_AUTH: u8 = 5;

/// Cap on a single frame's payload — a guard against a desynced/hostile stream
/// allocating unbounded memory. A 1 s PCM chunk is 384 000 bytes; 16 MiB is far
/// above any legitimate frame yet bounds a bad `len`.
const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// How long the accept waits for the spawned sidecar to dial back before giving
/// up (it connects immediately on startup; a longer hang means it failed to
/// launch).
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);

/// Write one framed message: a type byte, a little-endian `u32` length, then the
/// payload. Flushes so the consumer sees it promptly (the socket is `nodelay`).
pub fn write_frame(w: &mut impl Write, frame_type: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "sidecar frame payload is too large")
    })?;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("sidecar frame length {len} exceeds the cap"),
        ));
    }
    w.write_all(&[frame_type])?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Read one framed message, or `Ok(None)` on a clean EOF at a frame boundary
/// (the sidecar closed the socket). Errors on a truncated frame or a length
/// above [`MAX_FRAME_BYTES`].
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        // A clean EOF before any byte of the next frame is a normal close.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let frame_type = head[0];
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("sidecar frame length {len} exceeds the cap"),
        ));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    Ok(Some((frame_type, payload)))
}

/// Reinterpret interleaved f32 LE bytes as samples (any trailing partial frame
/// is dropped). The PCM path's per-chunk conversion.
fn pcm_from_le_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

type StatusSink = Box<dyn FnMut(String) + Send>;
type PcmSink = Box<dyn FnMut(&[u8]) + Send>;
type DeckStatusSinks = [StatusSink; lsdj_engine::DECK_COUNT];
type DeckPcmSinks = [PcmSink; lsdj_engine::DECK_COUNT];
type SharedStatusSinks = [Arc<Mutex<StatusSink>>; lsdj_engine::DECK_COUNT];

/// The read loop: drain frames from the sidecar until EOF/error. PCM frames are
/// posted to the deck's ring (the non-RT producer side) and then TEED to `on_pcm`
/// (gap 1: the analysis feed to the webview); status frames go to `on_status` (the
/// Tauri-event sink in production, a recorder in tests).
///
/// Returns the [`DeckHandle`] when the stream closes — the supervisor reclaims it
/// (the engine's ring is permanent across a sidecar exit; the handle outlives any
/// one connection). `on_status` and `on_pcm` are borrowed so the supervisor can
/// still report the exit afterwards / reconstruct the tap on a restart.
pub fn run_reader(
    mut stream: impl Read,
    mut deck_handle: DeckHandle,
    on_status: &mut impl FnMut(String),
    on_pcm: &mut impl FnMut(&[u8]),
) -> DeckHandle {
    loop {
        match read_frame(&mut stream) {
            Ok(Some((FRAME_PCM, payload))) => {
                let samples = pcm_from_le_bytes(&payload);
                // post_pcm (the RT ring producer) FIRST and bit-unchanged — it is
                // non-blocking (an overrun drops the surplus; the worker paces ~3 s
                // ahead, so this is rare). Then tee the SAME raw bytes to the
                // analysis subscriber, strictly AFTER and on this non-RT reader
                // thread, so the ring handoff and the RT path are untouched.
                deck_handle.post_pcm(&samples);
                on_pcm(&payload);
            }
            Ok(Some((FRAME_STATUS, payload))) => {
                if let Ok(text) = String::from_utf8(payload) {
                    on_status(text);
                }
            }
            // An unknown frame type is ignored (forward-compatible), not fatal.
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    deck_handle
}

/// Shared-worker variant of [`run_reader`].  Every PCM/status payload starts
/// with a deck byte (`0` or `1`); the remaining bytes are exactly the existing
/// per-deck payload.  Invalid deck indices are ignored without disturbing the
/// other stream.
pub fn run_shared_reader(
    mut stream: impl Read,
    mut deck_handles: [DeckHandle; lsdj_engine::DECK_COUNT],
    on_status: &mut DeckStatusSinks,
    on_pcm: &mut DeckPcmSinks,
) -> [DeckHandle; lsdj_engine::DECK_COUNT] {
    while let Ok(Some((frame_type, payload))) = read_frame(&mut stream) {
        let Some((&deck, body)) = payload.split_first() else {
            continue;
        };
        let deck = deck as usize;
        if deck >= lsdj_engine::DECK_COUNT {
            continue;
        }
        match frame_type {
            FRAME_PCM => {
                let samples = pcm_from_le_bytes(body);
                deck_handles[deck].post_pcm(&samples);
                on_pcm[deck](body);
            }
            FRAME_STATUS => {
                if let Ok(text) = String::from_utf8(body.to_vec()) {
                    on_status[deck](text);
                }
            }
            _ => {}
        }
    }
    deck_handles
}

/// What a reader thread hands back when its sidecar connection ends: the deck
/// ring producer ([`DeckHandle`]) and the status sink. The engine's input ring is
/// PERMANENT across a sidecar exit (the consumer lives inside the engine), so the
/// producer must be RECLAIMED — never dropped — to feed a respawned sidecar after
/// a model switch. [`Sidecar::restart`] joins the reader to take these back.
struct ReaderExit {
    handle: DeckHandle,
    on_status: StatusSink,
}

/// The freshly-built control writer, child handle, stop flag, and reader thread —
/// the pieces a (re)spawn produces and a [`Sidecar`] installs.
struct ReaderParts {
    control: Arc<Mutex<Option<TcpStream>>>,
    child: Arc<Mutex<Option<SupervisedChild>>>,
    stop: Arc<AtomicBool>,
    reader: JoinHandle<ReaderExit>,
}

struct SharedReaderExit {
    handles: [DeckHandle; lsdj_engine::DECK_COUNT],
}

struct SharedReaderParts {
    control: Arc<Mutex<Option<TcpStream>>>,
    process: Arc<Mutex<SharedProcessState>>,
    stop: Arc<AtomicBool>,
    reader: JoinHandle<SharedReaderExit>,
}

trait SharedProcess: Send {
    fn shutdown(&mut self) -> io::Result<()>;
}

impl SharedProcess for SupervisedChild {
    fn shutdown(&mut self) -> io::Result<()> {
        let report = SupervisedChild::shutdown(self, Duration::from_millis(500))?;
        crate::child_process::log_shutdown("shared sidecar", Ok(report));
        Ok(())
    }
}

enum SharedProcessState {
    Stopped,
    Running(Box<dyn SharedProcess>),
    /// Teardown did not positively reap the process tree. Ownership is retained
    /// and promotion remains blocked until a later retry reaches `Stopped`.
    Uncertain(Box<dyn SharedProcess>),
}

/// One supervised deck sidecar: the spawned Python process, the control writer
/// (engine → sidecar), and the reader thread (sidecar → engine). Dropping it
/// stops the reader, closes the socket, and kills the child.
pub struct Sidecar {
    deck_id: String,
    /// This deck's index, the analysis-tap registry, and the beat-analysis feed
    /// — kept so `restart` can reconstruct the PCM tee closure for the respawned
    /// reader (the tee is reconstructed per spawn from the stable handles +
    /// `deck_idx`, so it is NOT reclaimed via `ReaderExit`).
    deck_idx: usize,
    taps: PcmTaps,
    feed: AnalysisFeed,
    /// The control-writer half of the socket; `None` until the sidecar connects,
    /// and after a teardown. Behind a `Mutex` so IPC callers serialise writes.
    control: Arc<Mutex<Option<TcpStream>>>,
    child: Arc<Mutex<Option<SupervisedChild>>>,
    stop: Arc<AtomicBool>,
    /// The accept+read thread; its result carries the reclaimable [`ReaderExit`].
    reader: Option<JoinHandle<ReaderExit>>,
}

/// Bind a loopback listener and launch the Python sidecar pointed at it — the
/// FALLIBLE prefix, done BEFORE any [`DeckHandle`] is committed, so a bad launch
/// (or a bind failure) never costs the deck its ring producer. [`Sidecar::restart`]
/// runs this first and leaves the running sidecar untouched if it fails.
fn bind_and_launch(
    deck_id: &str,
    model: &str,
) -> io::Result<(TcpListener, SupervisedChild, String)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(false).ok();
    let port = listener.local_addr()?.port();
    let token = crate::local_auth::generate_capability();
    let mut command = authenticated_sidecar_command(deck_id, model, port, &token)?;
    let child = crate::child_process::spawn_grouped(&mut command)?;
    Ok((listener, child, token))
}

/// The PCM tee closure handed to a reader thread: forward each deck PCM frame
/// to its webview subscriber (gap 1 — loudness + the live band scroller) AND
/// into the shell's beat-analysis feed (ADR-0025). Reconstructed per (re)spawn
/// from the stable `taps`/`feed` + `deck_idx`, so it never needs reclaiming
/// across a model switch. Both sends are non-blocking (the reader must never
/// stall behind a consumer).
fn pcm_tee(
    taps: PcmTaps,
    feed: AnalysisFeed,
    deck_idx: usize,
) -> impl FnMut(&[u8]) + Send + 'static {
    move |bytes: &[u8]| {
        taps.send(deck_idx, bytes);
        feed.pcm_bytes(deck_idx, bytes);
    }
}

/// Start the accept+read thread for an already-launched `child`, moving the deck
/// `handle` and `on_status` sink into it. The thread accepts the sidecar's
/// connection, stashes the control writer, runs [`run_reader`], and returns the
/// reclaimable [`ReaderExit`] when the connection ends.
///
/// Infallible by design: a reader-thread spawn failure is resource exhaustion and
/// PANICS (like the engine render thread, `host.rs`). The alternative — returning
/// the `handle` on a recoverable error — is moot when the OS is out of threads,
/// and a fallible signature would risk DROPPING the deck's permanent ring producer
/// in a half-built state. The fallible prefix (bind + launch) lives in
/// [`bind_and_launch`], BEFORE the handle is committed, so a restart can leave the
/// running sidecar untouched on the only recoverable failures.
fn start_reader(
    listener: TcpListener,
    deck_id: &str,
    child: SupervisedChild,
    token: String,
    handle: DeckHandle,
    mut on_status: StatusSink,
    mut on_pcm: impl FnMut(&[u8]) + Send + 'static,
) -> ReaderParts {
    let control: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let control_for_reader = control.clone();
    let stop_for_reader = stop.clone();
    let deck_label = deck_id.to_string();
    let reader = thread::Builder::new()
        .name(format!("lsdj-sidecar-{deck_id}"))
        .spawn(move || {
            // Bound the accept so a sidecar that never connects cannot hang the
            // thread forever; poll the listener until the deadline OR until `stop`
            // is set — a teardown / restart wakes a never-connected accept promptly
            // instead of waiting out ACCEPT_TIMEOUT (which would freeze the deck's
            // control while the supervisor joins this thread).
            let stream = match accept_authenticated_with_timeout(
                &listener,
                &stop_for_reader,
                ACCEPT_TIMEOUT,
                token.as_bytes(),
            ) {
                Some(s) => s,
                None => {
                    eprintln!("lsdj-sidecar-{deck_label}: sidecar never connected");
                    return ReaderExit { handle, on_status };
                }
            };
            stream.set_nodelay(true).ok();
            match stream.try_clone() {
                Ok(writer) => {
                    *control_for_reader.lock().unwrap_or_else(|p| p.into_inner()) = Some(writer)
                }
                Err(e) => {
                    eprintln!("lsdj-sidecar-{deck_label}: cannot split socket: {e}");
                    return ReaderExit { handle, on_status };
                }
            }
            let handle = run_reader(stream, handle, &mut on_status, &mut on_pcm);
            // Reader returned → the sidecar exited / disconnected. Report it unless
            // we asked it to stop (a clean shutdown / model switch).
            *control_for_reader.lock().unwrap_or_else(|p| p.into_inner()) = None;
            if !stop_for_reader.load(Ordering::Acquire) {
                on_status(format!(
                    "{{\"event\":\"worker_died\",\"deck\":\"{deck_label}\"}}"
                ));
            }
            ReaderExit { handle, on_status }
        })
        .expect("failed to spawn lsdj sidecar reader thread");
    ReaderParts {
        control,
        child: Arc::new(Mutex::new(Some(child))),
        stop,
        reader,
    }
}

fn bind_and_launch_shared(
    models: &[String; lsdj_engine::DECK_COUNT],
) -> io::Result<(TcpListener, SupervisedChild, String)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(false).ok();
    let port = listener.local_addr()?.port();
    let token = crate::local_auth::generate_capability();
    let mut command = authenticated_shared_sidecar_command(models, port, &token)?;
    let child = crate::child_process::spawn_grouped(&mut command)?;
    Ok((listener, child, token))
}

fn start_shared_reader(
    listener: TcpListener,
    child: SupervisedChild,
    token: String,
    handles: [DeckHandle; lsdj_engine::DECK_COUNT],
    on_status: SharedStatusSinks,
    mut on_pcm: DeckPcmSinks,
) -> SharedReaderParts {
    let control: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let control_for_reader = control.clone();
    let stop_for_reader = stop.clone();
    let reader = thread::Builder::new()
        .name("lsdj-sidecar-shared".to_string())
        .spawn(move || {
            let mut reader_status: DeckStatusSinks = std::array::from_fn(|deck| {
                let sink = on_status[deck].clone();
                let status_stop = stop_for_reader.clone();
                Box::new(move |message| {
                    if !status_stop.load(Ordering::Acquire) {
                        (sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))(message)
                    }
                }) as StatusSink
            });
            let stream = match accept_authenticated_with_timeout(
                &listener,
                &stop_for_reader,
                ACCEPT_TIMEOUT,
                token.as_bytes(),
            ) {
                Some(stream) => stream,
                None => {
                    eprintln!("lsdj-sidecar-shared: sidecar never connected");
                    return SharedReaderExit { handles };
                }
            };
            stream.set_nodelay(true).ok();
            match stream.try_clone() {
                Ok(writer) => {
                    *control_for_reader.lock().unwrap_or_else(|p| p.into_inner()) = Some(writer)
                }
                Err(error) => {
                    eprintln!("lsdj-sidecar-shared: cannot split socket: {error}");
                    return SharedReaderExit { handles };
                }
            }
            let handles = run_shared_reader(stream, handles, &mut reader_status, &mut on_pcm);
            *control_for_reader.lock().unwrap_or_else(|p| p.into_inner()) = None;
            if !stop_for_reader.load(Ordering::Acquire) {
                for (deck, sink) in on_status.iter().enumerate() {
                    (sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))(format!(
                        "{{\"event\":\"worker_died\",\"deck\":\"{}\"}}",
                        ["a", "b"][deck]
                    ));
                }
            }
            SharedReaderExit { handles }
        })
        .expect("failed to spawn shared LSDJ sidecar reader thread");
    SharedReaderParts {
        control,
        process: Arc::new(Mutex::new(SharedProcessState::Running(Box::new(child)))),
        stop,
        reader,
    }
}

impl Sidecar {
    /// Spawn and supervise the sidecar for `deck_id`, feeding `deck_handle` and
    /// reporting status through `on_status`. Binds a loopback listener, launches
    /// the Python sidecar pointed at the bound port, accepts its connection, and
    /// starts the reader thread. The spawn command is built from
    /// [`sidecar_base_command`]
    /// (`LSDJ_BACKEND_BIN` in a release; overridable via `LSDJ_SIDECAR_CMD` in dev).
    ///
    /// Errors if the listener cannot bind or the process cannot launch — the
    /// caller logs and leaves that deck without a sidecar (the engine still runs,
    /// silent on that deck), exactly like the graceful no-audio-device path.
    pub fn spawn(
        deck_id: &str,
        deck_idx: usize,
        model: &str,
        deck_handle: DeckHandle,
        on_status: impl FnMut(String) + Send + 'static,
        taps: PcmTaps,
        feed: AnalysisFeed,
    ) -> io::Result<Sidecar> {
        let (listener, child, token) = bind_and_launch(deck_id, model)?;
        let parts = start_reader(
            listener,
            deck_id,
            child,
            token,
            deck_handle,
            Box::new(on_status),
            pcm_tee(taps.clone(), feed.clone(), deck_idx),
        );
        Ok(Sidecar {
            deck_id: deck_id.to_string(),
            deck_idx,
            taps,
            feed,
            control: parts.control,
            child: parts.child,
            stop: parts.stop,
            reader: Some(parts.reader),
        })
    }

    /// Restart this deck's sidecar with `new_model`, REUSING the deck's permanent
    /// ring producer (an in-process model switch). The new child is launched
    /// FIRST, so a bind/launch failure leaves the running sidecar untouched and
    /// returns `Err`; only once it is up is the old child torn down, its
    /// [`DeckHandle`] reclaimed (the reader returns it), and handed to the new
    /// reader. The engine's input ring stays open throughout — `render` just
    /// under-runs to silence on that deck while the new model loads. A model that
    /// fails to LOAD surfaces as `worker_died` and leaves the deck silent until a
    /// valid model is selected; the ring is preserved, so recovery is a re-select.
    ///
    /// Emits a `model_loading` status across the switch (parity with the Web
    /// path), so the deck resets its channel and shows the loading state. (Flushing
    /// the old model's already-buffered ~3 s of ring PCM needs an engine-side ring
    /// reset — a documented follow-up; until then a brief old-model tail can play
    /// out as the new stream takes over.)
    pub fn restart(&mut self, new_model: &str) -> io::Result<()> {
        // Launch the new child FIRST. On a bind/launch failure the running sidecar
        // — and its ring producer — are completely untouched; only after this
        // succeeds do we reclaim the handle, so it is never at risk on a recoverable
        // error.
        let (listener, child, token) = bind_and_launch(&self.deck_id, new_model)?;

        // `stop` suppresses the old reader's `worker_died` across the deliberate
        // switch (and wakes a never-connected accept).
        self.stop.store(true, Ordering::Release);
        // SHUT DOWN the control socket — do not merely drop our clone. The reader
        // holds its own dup of this FD (`try_clone`), so dropping the writer leaves
        // the socket open and the reader blocked in `read_frame`; `shutdown` tears
        // down the SHARED socket so the reader's read returns EOF at once (and
        // signals the old sidecar to exit). The child kill then terminates it.
        if let Some(writer) = self
            .control
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
        if let Some(mut old) = self.child.lock().unwrap_or_else(|p| p.into_inner()).take() {
            crate::child_process::log_shutdown(
                &format!("sidecar {} restart", self.deck_id),
                old.shutdown(Duration::from_millis(500)),
            );
        }
        let exit = self
            .reader
            .take()
            .ok_or_else(|| io::Error::other("sidecar has no reader to reclaim"))?
            .join()
            .map_err(|_| io::Error::other("sidecar reader thread panicked"))?;

        let mut on_status = exit.on_status;
        on_status(format!(
            "{{\"event\":\"model_loading\",\"deck\":\"{}\",\"model\":\"{new_model}\"}}",
            self.deck_id
        ));

        let parts = start_reader(
            listener,
            &self.deck_id,
            child,
            token,
            exit.handle,
            on_status,
            pcm_tee(self.taps.clone(), self.feed.clone(), self.deck_idx),
        );
        self.control = parts.control;
        self.child = parts.child;
        self.stop = parts.stop;
        self.reader = Some(parts.reader);
        Ok(())
    }

    /// Send a JSON deck command to the sidecar (`{"type":"play"}`, `set_style`,
    /// …). A no-op (logged) if the sidecar is not connected — control must never
    /// block or panic the IPC thread.
    pub fn send_control(&self, json: &str) {
        let mut guard = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stream) = guard.as_mut() {
            if let Err(e) = write_frame(stream, FRAME_CONTROL, json.as_bytes()) {
                eprintln!("lsdj-sidecar-{}: control write failed: {e}", self.deck_id);
                *guard = None;
            }
        }
    }

    /// Send a style-sample embed (M15) to the sidecar over the control socket: the
    /// captured PCM is framed as `[u32 LE id length][id][PCM]`. A no-op (logged) if
    /// the sidecar is not connected.
    pub fn send_embed(&self, id: &str, pcm: &[u8]) {
        let mut payload = Vec::with_capacity(4 + id.len() + pcm.len());
        payload.extend_from_slice(&(id.len() as u32).to_le_bytes());
        payload.extend_from_slice(id.as_bytes());
        payload.extend_from_slice(pcm);
        let mut guard = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stream) = guard.as_mut() {
            if let Err(e) = write_frame(stream, FRAME_EMBED, &payload) {
                eprintln!("lsdj-sidecar-{}: embed write failed: {e}", self.deck_id);
                *guard = None;
            }
        }
    }
}

/// One supervised PyTorch process owning both deck states.  Different model
/// selections remain legal, but load two model instances inside this one process;
/// equal selections share one upstream model and keep independent continuation.
pub struct SharedSidecar {
    models: [String; lsdj_engine::DECK_COUNT],
    taps: PcmTaps,
    feed: AnalysisFeed,
    on_status: SharedStatusSinks,
    control: Arc<Mutex<Option<TcpStream>>>,
    process: Arc<Mutex<SharedProcessState>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<SharedReaderExit>>,
    /// Reclaimed ring producers parked after a replacement launch failure. A
    /// later selection can recover without reconstructing the native engine.
    parked: Option<SharedReaderExit>,
}

impl SharedSidecar {
    /// Construct the shared CUDA topology without launching Python.  The native
    /// engine's permanent ring producers remain parked here until a verified
    /// managed MRT2 generation is installed (or a later retry succeeds).
    pub fn parked(
        models: [String; lsdj_engine::DECK_COUNT],
        handles: [DeckHandle; lsdj_engine::DECK_COUNT],
        on_status: DeckStatusSinks,
        taps: PcmTaps,
        feed: AnalysisFeed,
    ) -> Self {
        Self {
            models,
            taps,
            feed,
            on_status: on_status.map(|sink| Arc::new(Mutex::new(sink))),
            control: Arc::new(Mutex::new(None)),
            process: Arc::new(Mutex::new(SharedProcessState::Stopped)),
            stop: Arc::new(AtomicBool::new(true)),
            reader: None,
            parked: Some(SharedReaderExit { handles }),
        }
    }

    #[cfg(all(test, not(feature = "managed-runtime")))]
    pub fn spawn(
        models: [String; lsdj_engine::DECK_COUNT],
        handles: [DeckHandle; lsdj_engine::DECK_COUNT],
        on_status: DeckStatusSinks,
        taps: PcmTaps,
        feed: AnalysisFeed,
    ) -> Result<Self, (io::Error, [DeckHandle; lsdj_engine::DECK_COUNT])> {
        let mut sidecar = Self::parked(models, handles, on_status, taps, feed);
        if let Err(error) = sidecar.activate() {
            let handles = sidecar
                .parked
                .take()
                .expect("failed shared activation preserves deck handles")
                .handles;
            return Err((error, handles));
        }
        Ok(sidecar)
    }

    /// Start a worker from parked handles.  Resolution and manifest validation
    /// occur inside `bind_and_launch_shared` immediately before spawn, so an
    /// install that completed after app startup becomes usable without restart.
    pub fn activate(&mut self) -> io::Result<()> {
        match &*self.process.lock().unwrap_or_else(|p| p.into_inner()) {
            SharedProcessState::Running(_) => return Ok(()),
            SharedProcessState::Uncertain(_) => {
                return Err(io::Error::other(
                    "shared sidecar process reap is still uncertain",
                ))
            }
            SharedProcessState::Stopped => {}
        }
        if self.parked.is_none() {
            return Err(io::Error::other(
                "shared sidecar has no parked deck handles",
            ));
        }
        let (listener, child, token) = bind_and_launch_shared(&self.models)?;
        let exit = self
            .parked
            .take()
            .ok_or_else(|| io::Error::other("shared sidecar has no parked deck handles"))?;
        let on_pcm: DeckPcmSinks = [
            Box::new(pcm_tee(self.taps.clone(), self.feed.clone(), 0)),
            Box::new(pcm_tee(self.taps.clone(), self.feed.clone(), 1)),
        ];
        let parts = start_shared_reader(
            listener,
            child,
            token,
            exit.handles,
            self.on_status.clone(),
            on_pcm,
        );
        self.control = parts.control;
        self.process = parts.process;
        self.stop = parts.stop;
        self.reader = Some(parts.reader);
        Ok(())
    }

    /// Stop and fully reap the worker while retaining the permanent deck ring
    /// producers.  Promotion may rename the managed generation only after this
    /// succeeds (notably on Windows, where a live Python process holds DLLs).
    #[cfg(feature = "managed-runtime")]
    pub fn quiesce(&mut self) -> io::Result<()> {
        let exit = self.stop_and_reclaim()?;
        self.parked = Some(exit);
        Ok(())
    }

    fn send_control(&self, deck: usize, json: &str) {
        if deck >= lsdj_engine::DECK_COUNT {
            return;
        }
        let mut payload = Vec::with_capacity(1 + json.len());
        payload.push(deck as u8);
        payload.extend_from_slice(json.as_bytes());
        let mut guard = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stream) = guard.as_mut() {
            if let Err(error) = write_frame(stream, FRAME_CONTROL, &payload) {
                eprintln!("lsdj-sidecar-shared: control write failed: {error}");
                *guard = None;
            }
        }
    }

    fn send_embed(&self, deck: usize, id: &str, pcm: &[u8]) {
        if deck >= lsdj_engine::DECK_COUNT {
            return;
        }
        let mut payload = Vec::with_capacity(1 + 4 + id.len() + pcm.len());
        payload.push(deck as u8);
        payload.extend_from_slice(&(id.len() as u32).to_le_bytes());
        payload.extend_from_slice(id.as_bytes());
        payload.extend_from_slice(pcm);
        let mut guard = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stream) = guard.as_mut() {
            if let Err(error) = write_frame(stream, FRAME_EMBED, &payload) {
                eprintln!("lsdj-sidecar-shared: embed write failed: {error}");
                *guard = None;
            }
        }
    }

    fn restart(&mut self, deck: usize, model: &str) -> io::Result<()> {
        if deck >= lsdj_engine::DECK_COUNT {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid deck"));
        }
        let mut models = self.models.clone();
        models[deck] = model.to_string();

        // A single shared CUDA worker owns both deck states, so both become
        // unavailable together. Publish that fact before stopping the old
        // generation and before any replacement model allocation can begin.
        // Gate the old reader first so a final stale `ready` cannot race after
        // these loading events; the process itself is stopped below.
        self.stop.store(true, Ordering::Release);
        for (index, sink) in self.on_status.iter().enumerate() {
            let deck_label = ["a", "b"][index];
            (sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))(
                serde_json::json!({
                    "event": "model_loading",
                    "deck": deck_label,
                    "model": &models[index],
                })
                .to_string(),
            );
        }

        let exit = self.stop_and_reclaim()?;
        // Stop-and-reap is intentional for shared CUDA. Launch-first remains the
        // per-deck policy above, but would temporarily require two resident model
        // generations here and can OOM a minimum-VRAM host.
        let (listener, child, token) = match bind_and_launch_shared(&models) {
            Ok(launch) => launch,
            Err(error) => {
                self.parked = Some(exit);
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "shared CUDA replacement failed after the old worker was stopped; reselect a model to retry: {error}"
                    ),
                ));
            }
        };

        let on_pcm: DeckPcmSinks = [
            Box::new(pcm_tee(self.taps.clone(), self.feed.clone(), 0)),
            Box::new(pcm_tee(self.taps.clone(), self.feed.clone(), 1)),
        ];
        let parts = start_shared_reader(
            listener,
            child,
            token,
            exit.handles,
            self.on_status.clone(),
            on_pcm,
        );
        self.models = models;
        self.control = parts.control;
        self.process = parts.process;
        self.stop = parts.stop;
        self.reader = Some(parts.reader);
        Ok(())
    }

    fn stop_and_reclaim(&mut self) -> io::Result<SharedReaderExit> {
        self.stop.store(true, Ordering::Release);
        if let Some(writer) = self
            .control
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
        let shutdown_result =
            stop_shared_process(&mut self.process.lock().unwrap_or_else(|p| p.into_inner()));
        let exit = if let Some(reader) = self.reader.take() {
            reader
                .join()
                .map_err(|_| io::Error::other("shared sidecar reader thread panicked"))?
        } else {
            self.parked
                .take()
                .ok_or_else(|| io::Error::other("shared sidecar has no deck handles to reclaim"))?
        };
        if let Err(error) = shutdown_result {
            self.parked = Some(exit);
            return Err(error);
        }
        Ok(exit)
    }
}

fn stop_shared_process(state: &mut SharedProcessState) -> io::Result<()> {
    let previous = std::mem::replace(state, SharedProcessState::Stopped);
    let mut process = match previous {
        SharedProcessState::Stopped => return Ok(()),
        SharedProcessState::Running(process) | SharedProcessState::Uncertain(process) => process,
    };
    match process.shutdown() {
        Ok(()) => Ok(()),
        Err(error) => {
            *state = SharedProcessState::Uncertain(process);
            Err(error)
        }
    }
}

impl Drop for SharedSidecar {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(writer) = self
            .control
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
        if let Err(error) =
            stop_shared_process(&mut self.process.lock().unwrap_or_else(|p| p.into_inner()))
        {
            crate::child_process::log_shutdown("shared sidecar", Err(error));
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// All model sidecars, held in Tauri managed state. The deck-control commands
/// forward validated JSON to the matching sidecar; a deck with no sidecar (spawn
/// failed, or sidecars disabled) silently drops the command. Each slot is a
/// `Mutex` so `deck_set_model` can mutate one sidecar (a model switch) through the
/// shared `tauri::State` without a supervisor thread.
pub struct Sidecars {
    decks: Vec<Mutex<Option<Sidecar>>>,
    shared: Mutex<Option<SharedSidecar>>,
}

impl Sidecars {
    pub fn new(decks: Vec<Option<Sidecar>>) -> Self {
        Sidecars {
            decks: decks.into_iter().map(Mutex::new).collect(),
            shared: Mutex::new(None),
        }
    }

    pub fn new_shared(shared: SharedSidecar) -> Self {
        Sidecars {
            decks: (0..lsdj_engine::DECK_COUNT)
                .map(|_| Mutex::new(None))
                .collect(),
            shared: Mutex::new(Some(shared)),
        }
    }

    /// Quiesce the managed shared worker before its verified generation is
    /// renamed. A no-op for the macOS per-deck topology.
    #[cfg(feature = "managed-runtime")]
    pub fn quiesce_shared(&self) -> Result<(), String> {
        let mut shared = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(shared) = shared.as_mut() {
            shared.quiesce().map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Revalidate and activate a parked shared worker. This covers both first
    /// install and post-promotion restart without reconstructing the audio host.
    #[cfg(feature = "managed-runtime")]
    pub fn resume_shared(&self) -> Result<(), String> {
        let mut shared = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        let shared = shared
            .as_mut()
            .ok_or_else(|| "shared MRT2 decks are unavailable on this platform".to_string())?;
        shared.activate().map_err(|error| error.to_string())
    }

    /// Forward a JSON deck command to the sidecar for `deck` (a no-op for a deck
    /// without a live sidecar). `deck` is validated by the IPC layer.
    pub fn send(&self, deck: usize, json: &str) {
        if let Some(shared) = self
            .shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            shared.send_control(deck, json);
            return;
        }
        if let Some(slot) = self.decks.get(deck) {
            if let Some(sidecar) = slot.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                sidecar.send_control(json);
            }
        }
    }

    /// Route a style-sample embed (M15) to a deck's sidecar (a no-op for a deck
    /// without a live sidecar). `deck` is validated by the IPC layer.
    pub fn embed(&self, deck: usize, id: &str, pcm: &[u8]) {
        if let Some(shared) = self
            .shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            shared.send_embed(deck, id, pcm);
            return;
        }
        if let Some(slot) = self.decks.get(deck) {
            if let Some(sidecar) = slot.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                sidecar.send_embed(id, pcm);
            }
        }
    }

    /// Restart a deck's sidecar with `model` (an in-process model switch). Errors
    /// if the deck index is invalid, the deck has no sidecar, or the respawn fails
    /// (in which case the running sidecar is left untouched). `deck` is validated
    /// by the IPC layer.
    pub fn restart(&self, deck: usize, model: &str) -> Result<(), String> {
        let mut shared = self.shared.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(shared) = shared.as_mut() {
            return shared
                .restart(deck, model)
                .map_err(|error| error.to_string());
        }
        drop(shared);
        let slot = self.decks.get(deck).ok_or("invalid deck")?;
        let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_mut() {
            Some(sidecar) => sidecar.restart(model).map_err(|e| e.to_string()),
            None => Err("deck has no sidecar".to_string()),
        }
    }

    /// Tear down every sidecar (each `Sidecar`'s `Drop` kills + reaps its child).
    /// Called explicitly from the app's `RunEvent::Exit` handler because Tauri does
    /// NOT drop managed state on a macOS quit (`process::exit` skips destructors);
    /// the Python sidecars also self-terminate on the socket EOF, but this makes
    /// the teardown deterministic.
    pub fn shutdown(&self) {
        self.shared.lock().unwrap_or_else(|p| p.into_inner()).take();
        for slot in &self.decks {
            slot.lock().unwrap_or_else(|p| p.into_inner()).take();
        }
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The reader owns a clone of this socket, so merely dropping the control
        // writer does NOT wake its blocking read. Shut down the shared socket
        // first; this also tells a healthy Python worker to stop cleanly. Then
        // kill the whole process group so a `uv run` wrapper cannot leave that
        // worker alive holding the peer socket open.
        if let Some(writer) = self
            .control
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
        if let Some(mut child) = self.child.lock().unwrap_or_else(|p| p.into_inner()).take() {
            crate::child_process::log_shutdown(
                &format!("sidecar {}", self.deck_id),
                child.shutdown(Duration::from_millis(500)),
            );
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Poll-accept the first connection within `timeout`, or `None` on timeout / when
/// `stop` is set. Uses a brief non-blocking poll loop so the wait is bounded
/// without a dedicated timer thread, and checks `stop` each iteration so a
/// teardown (`Drop`) or a model switch (`restart`) unblocks a never-connected
/// accept promptly rather than waiting out the whole `timeout`.
fn accept_authenticated_with_timeout(
    listener: &TcpListener,
    stop: &AtomicBool,
    timeout: Duration,
    expected_token: &[u8],
) -> Option<TcpStream> {
    let deadline = std::time::Instant::now() + timeout;
    listener.set_nonblocking(true).ok();
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).ok();
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let handshake_timeout = remaining.min(Duration::from_secs(1));
                let _ = stream.set_read_timeout(Some(handshake_timeout));
                let authenticated = matches!(
                    read_frame(&mut stream),
                    Ok(Some((FRAME_AUTH, ref supplied)))
                        if crate::local_auth::constant_time_eq(supplied, expected_token)
                );
                let _ = stream.set_read_timeout(None);
                if authenticated {
                    return Some(stream);
                }
                let _ = stream.shutdown(std::net::Shutdown::Both);
                if stop.load(Ordering::Acquire) || std::time::Instant::now() >= deadline {
                    return None;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if stop.load(Ordering::Acquire) || std::time::Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// The base sidecar launch command — program + base args + CWD — with NO mode
/// flags. A packaged build uses the exact `LSDJ_BACKEND_BIN` path; otherwise
/// `LSDJ_SIDECAR_CMD` is overridable (whitespace-split) and dev defaults to
/// `uv run python -m lsdj.sidecar`. The authenticated deck path and the model
/// manager's installer (issue #43: `--init-resources` / `--download-model`) both
/// build on this, so the resolution lives in one place — a download is NOT a
/// deck, so it must not inherit `--deck`/`--model`/`--port`.
#[cfg_attr(feature = "managed-runtime", allow(dead_code))]
pub fn sidecar_base_command() -> io::Result<Command> {
    #[cfg(feature = "managed-runtime")]
    {
        let paths = crate::platform_paths::get();
        crate::managed_runtime::resolve(
            paths.assets(),
            crate::managed_runtime::Service::Mrt2,
        )
        .and_then(|resolved| resolved.into_command([], paths.backend_env()))
        .map_err(io::Error::other)
    }

    // A distributable app sets this to the exact bundled executable during
    // Tauri setup. Keep it as an OsString and pass it directly to Command so an
    // app copied into a path containing spaces still works.
    #[cfg(not(feature = "managed-runtime"))]
    if let Some(program) = std::env::var_os("LSDJ_BACKEND_BIN") {
        return Ok(Command::new(program));
    }

    #[cfg(not(feature = "managed-runtime"))]
    {
        let overridden = std::env::var("LSDJ_SIDECAR_CMD");
        let spec = overridden
            .clone()
            .unwrap_or_else(|_| "uv run python -m lsdj.sidecar".to_string());
        let mut parts = spec.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty LSDJ_SIDECAR_CMD"))?;
        let mut cmd = Command::new(program);
        cmd.args(parts);
        if overridden.is_err() {
            // The default `uv run` needs the backend project dir as its CWD. A packaged
            // build returned through LSDJ_BACKEND_BIN above and never reaches this path.
            cmd.current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../backend"));
        }
        Ok(cmd)
    }
}

#[cfg(feature = "managed-runtime")]
fn authenticated_sidecar_base_command(token: &str) -> io::Result<Command> {
    use std::ffi::OsString;

    let paths = crate::platform_paths::get();
    let ephemeral = paths
        .backend_env()
        .into_iter()
        .filter(|(name, _)| {
            !matches!(
                name.to_str(),
                Some("SA3_HOME" | "SA3_MLX_HOME" | "SA3_LORAS_HOME")
            )
        })
        .chain(std::iter::once((
            OsString::from("LSDJ_WORKER_LAUNCH_TOKEN"),
            OsString::from(token),
        )));
    crate::managed_runtime::resolve(
        paths.assets(),
        crate::managed_runtime::Service::Mrt2,
    )
    .and_then(|resolved| resolved.into_command([], ephemeral))
    .map_err(io::Error::other)
}

/// Build the dedicated managed MRT2 renderer command.  The render worker is a
/// separate, disposable process from both realtime decks and the SA3 server;
/// every launch re-resolves and revalidates the promoted MRT2 manifest and gets
/// a fresh one-use loopback capability.
#[cfg(feature = "managed-runtime")]
pub(crate) fn authenticated_render_worker_command(
    model: &str,
    port: u16,
    token: &str,
) -> io::Result<Command> {
    let mut command = authenticated_sidecar_base_command(token)?;
    command.args([
        "--render-worker",
        "--model",
        model,
        "--runtime",
        "pytorch-cuda",
        "--port",
        &port.to_string(),
    ]);
    Ok(command)
}

/// Runtime selected by the native platform.  The value is always sent over the
/// process boundary: Python never guesses and never falls back from CUDA to CPU.
/// The override exists for model-free contract tests and qualification hosts;
/// the Python policy layer still rejects an impossible platform/runtime pair.
pub fn mrt2_runtime_for_platform() -> io::Result<String> {
    let runtime = std::env::var("LSDJ_MRT2_RUNTIME").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "mlx".to_string()
        } else if cfg!(any(target_os = "linux", target_os = "windows")) {
            "pytorch-cuda".to_string()
        } else {
            "unsupported".to_string()
        }
    });
    match runtime.as_str() {
        "mlx" | "pytorch-cuda" => Ok(runtime),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unsupported MRT2 runtime {runtime:?} for this platform"),
        )),
    }
}

/// Whether a status event ends the deck's transport: the worker stopped
/// generating — it died, is reloading for a model switch, or halted ITSELF
/// (`stopped`, a generation failure) — so the interface store's `playing`
/// must drop with it. This is the Rust half of the transport derivation
/// (ADR-0020: the store owns `playing`); the status relay consults it before
/// forwarding the event to the webview. Missing the self-stop left the store
/// claiming `playing` after a failure, so the next deck_play round-tripped as
/// a value-equal no-op and wedged the webview's in-flight guard (the
/// play-button-swallows-presses bug, found on the device). Unparseable JSON
/// is not a transport signal.
pub fn transport_ended(status_json: &str) -> bool {
    matches!(
        status_event(status_json).as_deref(),
        Some("worker_died") | Some("startup_failed") | Some("model_loading") | Some("stopped")
    )
}

/// The status line's event name, if the JSON parses to one — the relay's
/// single parse point for transport and worker-health derivation.
pub fn status_event(status_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(status_json)
        .ok()?
        .get("event")?
        .as_str()
        .map(str::to_owned)
}

fn authenticated_sidecar_command(
    deck_id: &str,
    model: &str,
    port: u16,
    token: &str,
) -> io::Result<Command> {
    #[cfg(feature = "managed-runtime")]
    let mut cmd = authenticated_sidecar_base_command(token)?;
    #[cfg(not(feature = "managed-runtime"))]
    let mut cmd = {
        let mut cmd = sidecar_base_command()?;
        cmd.env("LSDJ_WORKER_LAUNCH_TOKEN", token);
        cmd
    };
    let runtime = mrt2_runtime_for_platform()?;
    cmd.args([
        "--deck",
        deck_id,
        "--model",
        model,
        "--runtime",
        &runtime,
        "--port",
        &port.to_string(),
    ]);
    Ok(cmd)
}

fn authenticated_shared_sidecar_command(
    models: &[String; lsdj_engine::DECK_COUNT],
    port: u16,
    token: &str,
) -> io::Result<Command> {
    #[cfg(feature = "managed-runtime")]
    let mut cmd = authenticated_sidecar_base_command(token)?;
    #[cfg(not(feature = "managed-runtime"))]
    let mut cmd = {
        let mut cmd = sidecar_base_command()?;
        cmd.env("LSDJ_WORKER_LAUNCH_TOKEN", token);
        cmd
    };
    let runtime = mrt2_runtime_for_platform()?;
    if runtime != "pytorch-cuda" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("shared MRT2 worker requires pytorch-cuda, got {runtime}"),
        ));
    }
    cmd.args([
        "--shared",
        "--model-a",
        &models[0],
        "--model-b",
        &models[1],
        "--runtime",
        &runtime,
        "--port",
        &port.to_string(),
    ]);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsdj_engine::Engine;
    use std::net::TcpStream;
    #[cfg(all(unix, not(feature = "managed-runtime")))]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicUsize;

    #[cfg(all(unix, not(feature = "managed-runtime")))]
    static SIDECAR_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn native_platform_selects_one_explicit_mrt2_runtime() {
        let runtime = mrt2_runtime_for_platform().expect("supported build target");
        if cfg!(target_os = "macos") {
            assert_eq!(runtime, "mlx");
        } else {
            assert_eq!(runtime, "pytorch-cuda");
        }
    }

    #[test]
    fn shared_deck_handles_can_park_until_the_first_managed_install() {
        let mut engine = Engine::new();
        let handles = [engine.create_deck(0), engine.create_deck(1)];
        let sinks: DeckStatusSinks = std::array::from_fn(|_| {
            Box::new(|_message| {}) as StatusSink
        });
        let mut shared = SharedSidecar::parked(
            ["mrt2_small".into(), "mrt2_small".into()],
            handles,
            sinks,
            PcmTaps::new(lsdj_engine::DECK_COUNT),
            AnalysisFeed::disconnected(lsdj_engine::DECK_COUNT),
        );

        assert!(shared.reader.is_none());
        assert!(shared.parked.is_some());
        assert!(matches!(
            &*shared
                .process
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            SharedProcessState::Stopped
        ));
        assert!(shared.stop.load(Ordering::Acquire));

        let shutdowns = Arc::new(AtomicUsize::new(1));
        *shared.process.lock().unwrap() =
            SharedProcessState::Uncertain(Box::new(RetrySharedProcess {
                shutdowns: shutdowns.clone(),
            }));
        assert!(shared.reader.is_none());
        assert!(shared.activate().is_err());
        assert_eq!(shutdowns.load(Ordering::Acquire), 1);
    }

    struct RetrySharedProcess {
        shutdowns: Arc<AtomicUsize>,
    }

    impl SharedProcess for RetrySharedProcess {
        fn shutdown(&mut self) -> io::Result<()> {
            if self.shutdowns.fetch_add(1, Ordering::AcqRel) == 0 {
                Err(io::Error::other("first reap is uncertain"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn failed_shared_reap_retains_supervisor_until_a_positive_retry() {
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let mut state = SharedProcessState::Running(Box::new(RetrySharedProcess {
            shutdowns: shutdowns.clone(),
        }));

        assert!(stop_shared_process(&mut state).is_err());
        assert!(matches!(state, SharedProcessState::Uncertain(_)));
        assert_eq!(shutdowns.load(Ordering::Acquire), 1);

        assert!(stop_shared_process(&mut state).is_ok());
        assert!(matches!(state, SharedProcessState::Stopped));
        assert_eq!(shutdowns.load(Ordering::Acquire), 2);
    }

    #[test]
    fn transport_ended_matches_only_worker_end_events() {
        // The three events after which the worker is no longer generating.
        assert!(transport_ended(r#"{"event":"worker_died","deck":"a"}"#));
        assert!(transport_ended(
            r#"{"event":"startup_failed","deck":"a","error":"CUDA unavailable"}"#
        ));
        assert!(transport_ended(
            r#"{"event":"model_loading","deck":"a","model":"mrt2_base"}"#
        ));
        // The worker halting itself (a generation failure) ends the transport
        // too — missing it wedged the play button behind a stale store.
        assert!(transport_ended(
            r#"{"event":"stopped","reason":"generation failed"}"#
        ));
        // Everything else — including the events of a healthy stream — is not a
        // transport signal, and neither is garbage. A plain error is NOT a
        // stop: the worker survives bad payloads without ending the stream.
        assert!(!transport_ended(
            r#"{"event":"ready","deck":"a","model":"mrt2_small"}"#
        ));
        assert!(!transport_ended(r#"{"event":"chunk","index":3,"rtf":1.2}"#));
        assert!(!transport_ended(r#"{"event":"error","error":"boom"}"#));
        assert!(!transport_ended("not json"));
        assert!(!transport_ended("{}"));
    }

    #[test]
    fn frame_round_trips_through_a_buffer() {
        let mut buf = Vec::new();
        write_frame(&mut buf, FRAME_STATUS, b"{\"event\":\"ready\"}").unwrap();
        write_frame(&mut buf, FRAME_PCM, &[1, 2, 3, 4]).unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let (t1, p1) = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(t1, FRAME_STATUS);
        assert_eq!(p1, b"{\"event\":\"ready\"}");
        let (t2, p2) = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(t2, FRAME_PCM);
        assert_eq!(p2, vec![1, 2, 3, 4]);
        // Clean EOF at a boundary → None.
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn over_cap_length_is_rejected() {
        let mut buf = Vec::new();
        buf.push(FRAME_PCM);
        buf.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());
        let mut cursor = std::io::Cursor::new(buf);
        assert!(read_frame(&mut cursor).is_err());
    }

    /// The read loop routes a PCM frame into the deck's ring and a status frame to
    /// the sink — the production data path minus the Python process. `run_reader`
    /// returns the handle on EOF, so the test reclaims it and asserts the ring's
    /// free space dropped by exactly the posted sample count.
    #[test]
    fn reader_routes_pcm_to_the_deck_and_status_to_the_sink() {
        let mut engine = Engine::new();
        let handle = engine.create_deck(0);
        let free_before = handle.free_samples();

        // A mock sidecar stream: one 256-frame stereo PCM chunk + one status,
        // then EOF — built in a buffer the reader drains synchronously.
        let frames = 256usize;
        let samples = frames * 2; // interleaved stereo
        let mut pcm = Vec::with_capacity(samples * 4);
        for _ in 0..samples {
            pcm.extend_from_slice(&0.1f32.to_le_bytes());
        }
        let mut wire = Vec::new();
        write_frame(&mut wire, FRAME_PCM, &pcm).unwrap();
        write_frame(&mut wire, FRAME_STATUS, b"{\"event\":\"chunk\"}").unwrap();

        let mut statuses = Vec::<String>::new();
        let mut teed = Vec::<Vec<u8>>::new();
        let handle = {
            let mut sink = |s: String| statuses.push(s);
            let mut tee = |b: &[u8]| teed.push(b.to_vec());
            run_reader(std::io::Cursor::new(wire), handle, &mut sink, &mut tee)
        };
        // The PCM frame was teed to the analysis sink byte-for-byte (gap 1).
        assert_eq!(teed, vec![pcm.clone()]);

        assert_eq!(
            free_before - handle.free_samples(),
            samples,
            "the deck ring should hold exactly the posted PCM"
        );
        assert_eq!(statuses, vec!["{\"event\":\"chunk\"}".to_string()]);
    }

    #[test]
    fn shared_reader_routes_prefixed_frames_to_independent_decks() {
        let mut engine = Engine::new();
        let handles = [engine.create_deck(0), engine.create_deck(1)];
        let free_before = [handles[0].free_samples(), handles[1].free_samples()];
        let pcm = [0.25f32, -0.25f32]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        let mut wire = Vec::new();
        let mut deck_one_pcm = vec![1u8];
        deck_one_pcm.extend_from_slice(&pcm);
        write_frame(&mut wire, FRAME_PCM, &deck_one_pcm).unwrap();
        let mut deck_zero_status = vec![0u8];
        deck_zero_status.extend_from_slice(br#"{"event":"ready"}"#);
        write_frame(&mut wire, FRAME_STATUS, &deck_zero_status).unwrap();

        let statuses = Arc::new(Mutex::new(Vec::<(usize, String)>::new()));
        let teed = Arc::new(Mutex::new(Vec::<(usize, Vec<u8>)>::new()));
        let statuses_zero = statuses.clone();
        let statuses_one = statuses.clone();
        let teed_zero = teed.clone();
        let teed_one = teed.clone();
        let mut status_sinks: DeckStatusSinks = [
            Box::new(move |value| statuses_zero.lock().unwrap().push((0, value))),
            Box::new(move |value| statuses_one.lock().unwrap().push((1, value))),
        ];
        let mut pcm_sinks: DeckPcmSinks = [
            Box::new(move |value| teed_zero.lock().unwrap().push((0, value.to_vec()))),
            Box::new(move |value| teed_one.lock().unwrap().push((1, value.to_vec()))),
        ];
        let handles = run_shared_reader(
            std::io::Cursor::new(wire),
            handles,
            &mut status_sinks,
            &mut pcm_sinks,
        );

        assert_eq!(handles[0].free_samples(), free_before[0]);
        assert_eq!(free_before[1] - handles[1].free_samples(), 2);
        assert_eq!(
            *statuses.lock().unwrap(),
            vec![(0, "{\"event\":\"ready\"}".to_string())]
        );
        assert_eq!(*teed.lock().unwrap(), vec![(1, pcm)]);
    }

    /// A status frame arriving over a real loopback socket reaches the sink — the
    /// transport itself (accept/connect/nodelay), end to end without Python.
    #[test]
    fn status_routes_over_a_loopback_socket() {
        let mut engine = Engine::new();
        let handle = engine.create_deck(0);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();

        write_frame(&mut client, FRAME_STATUS, b"{\"event\":\"ready\"}").unwrap();
        drop(client); // EOF → reader returns

        let mut statuses = Vec::<String>::new();
        let mut sink = |s: String| statuses.push(s);
        let mut tee = |_: &[u8]| {};
        let _handle = run_reader(server, handle, &mut sink, &mut tee);
        assert_eq!(statuses, vec!["{\"event\":\"ready\"}".to_string()]);
    }

    #[test]
    fn wrong_first_client_cannot_capture_the_sidecar_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let token = b"correct-sidecar-token-0123456789abcdef".to_vec();
        let expected = token.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_accept = stop.clone();
        let accept = thread::spawn(move || {
            let mut stream = accept_authenticated_with_timeout(
                &listener,
                &stop_for_accept,
                Duration::from_secs(2),
                &expected,
            )
            .expect("legitimate sidecar should be accepted after rejecting the racer");
            read_frame(&mut stream).unwrap()
        });

        let mut attacker = TcpStream::connect(addr).unwrap();
        write_frame(&mut attacker, FRAME_AUTH, b"wrong-sidecar-token-0123456789abcdef")
            .unwrap();
        drop(attacker);

        let mut legitimate = TcpStream::connect(addr).unwrap();
        write_frame(&mut legitimate, FRAME_AUTH, &token).unwrap();
        write_frame(&mut legitimate, FRAME_STATUS, br#"{"event":"ready"}"#).unwrap();
        drop(legitimate);

        assert_eq!(
            accept.join().unwrap(),
            Some((FRAME_STATUS, br#"{"event":"ready"}"#.to_vec()))
        );
    }

    #[cfg(all(windows, feature = "managed-runtime"))]
    fn windows_python() -> std::path::PathBuf {
        let search = std::env::var_os("PATH").expect("Python is available on CI PATH");
        for directory in std::env::split_paths(&search) {
            for name in ["python.exe", "python3.exe"] {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        panic!("Python executable is unavailable for Windows protocol qualification");
    }

    /// Native Windows qualification for the real Rust/Python wire boundary.
    /// The stand-in is stdlib-only and model-free, but the socket, authenticated
    /// handshake, bidirectional framing, structured argv, and supervised Python
    /// process are the same primitives used by the packaged sidecar.
    #[cfg(all(windows, feature = "managed-runtime"))]
    #[test]
    fn windows_python_round_trips_authenticated_control_status_and_pcm() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = "windows-protocol-token-0123456789abcdef";
        let root = std::env::temp_dir().join(format!(
            "lsdj-windows-python-protocol-{}-{port}-资产",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("sidecar stand-in.py");
        std::fs::write(
            &script,
            r#"import json
import os
import socket
import struct
import sys

def receive_exact(sock, length):
    payload = b""
    while len(payload) < length:
        chunk = sock.recv(length - len(payload))
        if not chunk:
            raise RuntimeError("truncated frame")
        payload += chunk
    return payload

def receive_frame(sock):
    header = receive_exact(sock, 5)
    frame_type, length = struct.unpack("<BI", header)
    return frame_type, receive_exact(sock, length)

def send_frame(sock, frame_type, payload):
    sock.sendall(struct.pack("<BI", frame_type, len(payload)) + payload)

with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=10) as sock:
    sock.settimeout(10)
    send_frame(sock, 5, os.environ["LSDJ_WORKER_LAUNCH_TOKEN"].encode("utf-8"))
    frame_type, payload = receive_frame(sock)
    assert frame_type == 3
    assert json.loads(payload) == {"command": "play", "deck": "a"}
    send_frame(sock, 2, json.dumps({"event": "ready", "runtime": "python"}).encode("utf-8"))
    send_frame(sock, 1, struct.pack("<ff", 0.25, -0.5))
"#,
        )
        .unwrap();

        let mut command = Command::new(windows_python());
        command
            .arg(&script)
            .arg(port.to_string())
            .env("LSDJ_WORKER_LAUNCH_TOKEN", token);
        let mut child = crate::child_process::spawn_grouped(&mut command).unwrap();
        let stop = AtomicBool::new(false);
        let mut stream = accept_authenticated_with_timeout(
            &listener,
            &stop,
            Duration::from_secs(10),
            token.as_bytes(),
        )
        .expect("Python authenticated over loopback");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write_frame(
            &mut stream,
            FRAME_CONTROL,
            br#"{"command":"play","deck":"a"}"#,
        )
        .unwrap();
        assert_eq!(
            read_frame(&mut stream).unwrap(),
            Some((
                FRAME_STATUS,
                br#"{"event": "ready", "runtime": "python"}"#.to_vec()
            ))
        );
        let pcm = [0.25f32, -0.5f32]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(read_frame(&mut stream).unwrap(), Some((FRAME_PCM, pcm)));
        drop(stream);
        let status = child.wait().unwrap();
        assert!(
            status.success(),
            "Python protocol stand-in failed: {status}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// In-process model switch: `restart` respawns the sidecar with a new model,
    /// reusing the deck's permanent ring producer, and suppresses a false
    /// `worker_died` across the deliberate switch. Wires a minimal stdlib-only
    /// wrapper + Python stand-in (no models) via `LSDJ_SIDECAR_CMD`, matching the
    /// `uv run` parent/grandchild topology used in development.
    #[cfg(all(unix, not(feature = "managed-runtime")))]
    #[test]
    fn restart_switches_model_without_a_worker_died() {
        let _env_guard = SIDECAR_ENV_LOCK.lock().unwrap();
        // A stand-in sidecar: connect to --port, announce ready with --model, then
        // deliberately ignore socket EOF. Teardown must kill it as the wrapper's
        // process-group child; killing only the wrapper leaves this process and
        // the Rust reader alive forever. No backend deps.
        let script = r#"import socket, struct, json, argparse, time, os
p = argparse.ArgumentParser()
p.add_argument('--port', type=int)
p.add_argument('--model')
p.add_argument('--deck')
a, _ = p.parse_known_args()
s = socket.create_connection(('127.0.0.1', a.port))
token = os.environ['LSDJ_WORKER_LAUNCH_TOKEN'].encode()
s.sendall(struct.pack('<BI', 5, len(token)) + token)
b = json.dumps({'event': 'ready', 'model': a.model}).encode()
s.sendall(struct.pack('<BI', 2, len(b)) + b)
while True:
    time.sleep(60)
"#;
        let tmp =
            std::env::temp_dir().join(format!("lsdj-sidecar-lifecycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let python = tmp.join("sidecar.py");
        let wrapper = tmp.join("sidecar-wrapper.sh");
        let pidfile = tmp.join("python-pids");
        std::fs::write(&python, script).unwrap();
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\npython3 \"{}\" \"$@\" &\nchild=$!\necho \"$child\" >> \"{}\"\nwait \"$child\"\n",
                python.display(),
                pidfile.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&wrapper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&wrapper, permissions).unwrap();
        // SAFETY-ish: no other test reads LSDJ_SIDECAR_CMD or calls
        // Sidecar::spawn, so this process-global is uncontended; removed at the end.
        std::env::set_var("LSDJ_SIDECAR_CMD", wrapper.as_os_str());

        let mut engine = Engine::new();
        let handle = engine.create_deck(0);
        let statuses = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = {
            let statuses = statuses.clone();
            move |s: String| statuses.lock().unwrap().push(s)
        };

        let taps = PcmTaps::new(2);
        let feed = AnalysisFeed::disconnected(2);
        let mut sidecar = Sidecar::spawn("a", 0, "model_a", handle, sink, taps, feed)
            .expect("spawn fake sidecar");

        // Wait for a `ready` status carrying `model` — distinct from the
        // `model_loading` status restart also emits (which is not a `ready`).
        let saw_ready = |model: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if statuses
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|s| s.contains("ready") && s.contains(model))
                {
                    return true;
                }
                thread::sleep(Duration::from_millis(20));
            }
            false
        };

        assert!(
            saw_ready("model_a"),
            "first child should report ready with model_a"
        );
        sidecar.restart("model_b").expect("restart");
        assert!(
            saw_ready("model_b"),
            "the restarted child should report ready with model_b"
        );
        let log = statuses.lock().unwrap();
        assert!(
            !log.iter().any(|s| s.contains("worker_died")),
            "a deliberate model switch must not emit worker_died"
        );
        assert!(
            log.iter()
                .any(|s| s.contains("model_loading") && s.contains("model_b")),
            "the switch should emit model_loading for the new model"
        );
        drop(log);

        // Quit teardown is synchronous. Keep a watchdog around the drop so a
        // regression fails instead of wedging the entire test process forever.
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        let teardown = thread::spawn(move || {
            drop(sidecar);
            let _ = dropped_tx.send(());
        });
        if dropped_rx.recv_timeout(Duration::from_secs(5)).is_err() {
            // Best-effort cleanup for the failure path: killing the stubborn
            // Python peers releases the blocked reader join.
            if let Ok(contents) = std::fs::read_to_string(&pidfile) {
                for pid in contents.lines().filter_map(|line| line.parse().ok()) {
                    // SAFETY: these pids came from the test wrapper we launched.
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
            let _ = teardown.join();
            panic!("sidecar teardown did not finish within five seconds");
        }
        teardown.join().expect("teardown thread panicked");

        // Both the pre-restart and final Python grandchildren must be gone. A
        // wrapper-only kill can return while leaving either one orphaned.
        let pids: Vec<libc::pid_t> = std::fs::read_to_string(&pidfile)
            .expect("wrapper recorded child pids")
            .lines()
            .map(|line| line.parse().expect("pidfile contains numeric pids"))
            .collect();
        assert_eq!(pids.len(), 2, "restart should launch two Python children");
        for pid in pids {
            let mut gone = false;
            for _ in 0..1000 {
                // SAFETY: signal 0 only probes the test child's liveness.
                if unsafe { libc::kill(pid, 0) } == -1 {
                    gone = true;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            if !gone {
                // SAFETY: failure cleanup for the known test child.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
            assert!(
                gone,
                "Python sidecar child {pid} survived process-group teardown"
            );
        }
        std::env::remove_var("LSDJ_SIDECAR_CMD");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A shared CUDA model switch cannot use the per-deck launch-first policy:
    /// two generations resident at once can OOM the minimum supported card.
    /// This model-free process test proves stop/reap-before-spawn, both-deck
    /// loading state, a failed replacement parked for retry, and recovery.
    #[cfg(all(unix, not(feature = "managed-runtime")))]
    #[test]
    fn shared_restart_serializes_cuda_generations_and_recovers_after_launch_failure() {
        let _env_guard = SIDECAR_ENV_LOCK.lock().unwrap();
        let tmp =
            std::env::temp_dir().join(format!("lsdj-shared-sidecar-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let python = tmp.join("shared-sidecar.py");
        let wrapper = tmp.join("shared-sidecar-wrapper.sh");
        let pidfile = tmp.join("python-pids");
        let overlap = tmp.join("overlap-detected");
        std::fs::write(
            &python,
            r#"import argparse, json, os, pathlib, socket, struct, sys, time
p = argparse.ArgumentParser()
p.add_argument('--port', type=int)
p.add_argument('--model-a')
p.add_argument('--model-b')
p.add_argument('--shared', action='store_true')
a, _ = p.parse_known_args()
pidfile = pathlib.Path(os.environ['LSDJ_TEST_PIDFILE'])
overlap = pathlib.Path(os.environ['LSDJ_TEST_OVERLAP'])
for line in pidfile.read_text().splitlines():
    pid = int(line)
    if pid == os.getpid():
        continue
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        continue
    overlap.write_text(f'{pid} still alive when {os.getpid()} started')
s = socket.create_connection(('127.0.0.1', a.port))
token = os.environ['LSDJ_WORKER_LAUNCH_TOKEN'].encode()
s.sendall(struct.pack('<BI', 5, len(token)) + token)
if 'load_fail' in (a.model_a, a.model_b):
    for deck, model in enumerate((a.model_a, a.model_b)):
        body = bytes([deck]) + json.dumps({'event': 'startup_failed', 'model': model}).encode()
        s.sendall(struct.pack('<BI', 2, len(body)) + body)
    s.close()
    sys.exit(3)
for deck, model in enumerate((a.model_a, a.model_b)):
    body = bytes([deck]) + json.dumps({'event': 'ready', 'model': model}).encode()
    s.sendall(struct.pack('<BI', 2, len(body)) + body)
while True:
    time.sleep(60)
"#,
        )
        .unwrap();
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\npython3 \"{}\" \"$@\" &\nchild=$!\necho \"$child\" >> \"{}\"\nwait \"$child\"\n",
                python.display(),
                pidfile.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&wrapper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&wrapper, permissions).unwrap();
        std::env::set_var("LSDJ_SIDECAR_CMD", wrapper.as_os_str());
        std::env::set_var("LSDJ_MRT2_RUNTIME", "pytorch-cuda");
        std::env::set_var("LSDJ_TEST_PIDFILE", pidfile.as_os_str());
        std::env::set_var("LSDJ_TEST_OVERLAP", overlap.as_os_str());

        let mut engine = Engine::new();
        let handles = [engine.create_deck(0), engine.create_deck(1)];
        let statuses = Arc::new(Mutex::new(Vec::<String>::new()));
        let sinks: DeckStatusSinks = std::array::from_fn(|_| {
            let statuses = statuses.clone();
            Box::new(move |message| statuses.lock().unwrap().push(message)) as StatusSink
        });
        let taps = PcmTaps::new(2);
        let feed = AnalysisFeed::disconnected(2);
        let mut shared = SharedSidecar::spawn(
            ["model_a".into(), "model_b".into()],
            handles,
            sinks,
            taps,
            feed,
        )
        .map_err(|(error, _)| error)
        .expect("spawn shared stand-in");

        let saw_ready = |model: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if statuses
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|status| status.contains("ready") && status.contains(model))
                {
                    return true;
                }
                thread::sleep(Duration::from_millis(20));
            }
            false
        };
        assert!(saw_ready("model_a"));
        assert!(saw_ready("model_b"));

        shared.restart(0, "model_c").expect("serialized switch");
        assert!(saw_ready("model_c"));
        assert!(!overlap.exists(), "old and replacement workers overlapped");

        std::env::set_var("LSDJ_SIDECAR_CMD", tmp.join("missing-sidecar"));
        let error = shared.restart(1, "model_x").unwrap_err();
        assert!(error.to_string().contains("reselect a model to retry"));
        std::env::set_var("LSDJ_SIDECAR_CMD", wrapper.as_os_str());
        shared
            .restart(0, "model_d")
            .expect("retry from parked handles");
        assert!(saw_ready("model_d"));
        assert!(!overlap.exists(), "recovery overlapped CUDA generations");

        assert!(
            !statuses
                .lock()
                .unwrap()
                .iter()
                .any(|status| status.contains("worker_died")),
            "deliberate switches and launch failure must suppress worker_died"
        );
        shared
            .restart(1, "load_fail")
            .expect("replacement process launched before model-load failure");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline
            && !statuses
                .lock()
                .unwrap()
                .iter()
                .any(|status| status.contains("startup_failed"))
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            statuses
                .lock()
                .unwrap()
                .iter()
                .any(|status| status.contains("startup_failed"))
        );
        shared
            .restart(1, "model_e")
            .expect("recover after replacement model-load failure");
        assert!(saw_ready("model_e"));
        assert!(!overlap.exists(), "load recovery overlapped CUDA generations");

        let log = statuses.lock().unwrap();
        for model in [
            "model_c",
            "model_b",
            "model_x",
            "model_d",
            "load_fail",
            "model_e",
        ] {
            assert!(
                log.iter()
                    .any(|status| status.contains("model_loading") && status.contains(model)),
                "missing both-deck loading state for {model}"
            );
        }
        assert!(log.iter().any(|status| status.contains("worker_died")));
        drop(log);

        drop(shared);
        let pids: Vec<libc::pid_t> = std::fs::read_to_string(&pidfile)
            .unwrap()
            .lines()
            .map(|line| line.parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 5, "failed launch must not create a child");
        for pid in pids {
            let mut gone = false;
            for _ in 0..1000 {
                if unsafe { libc::kill(pid, 0) } == -1 {
                    gone = true;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            assert!(gone, "shared worker {pid} survived serialized transition");
        }

        for name in [
            "LSDJ_SIDECAR_CMD",
            "LSDJ_MRT2_RUNTIME",
            "LSDJ_TEST_PIDFILE",
            "LSDJ_TEST_OVERLAP",
        ] {
            std::env::remove_var(name);
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
