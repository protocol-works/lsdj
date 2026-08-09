//! Native, authenticated Magenta render gateway for managed Linux/Windows.
//!
//! The public loopback HTTP service is deliberately separate from Stable Audio
//! 3. It owns one lazy, warm, disposable MRT2 render worker and translates the
//! user-facing `{prompt, seconds}` request into the reviewed binary protocol's
//! authoritative integer frame count and monotonic sequence. Every response is
//! bounded, sequence-bound, byte-counted, and SHA-256 checked before it becomes
//! a WAV. A cancellation, deadline, dropped HTTP request, or protocol mismatch
//! tears down and reaps the complete worker process tree; the next request starts
//! from a freshly revalidated managed generation.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::child_process::SupervisedChild;

const FRAME_STATUS: u8 = 2;
const FRAME_AUTH: u8 = 5;
const FRAME_RENDER_REQUEST: u8 = 6;
const FRAME_RENDER_BEGIN: u8 = 7;
const FRAME_RENDER_CHUNK: u8 = 8;
const FRAME_RENDER_END: u8 = 9;
const FRAME_RENDER_CANCEL: u8 = 10;
const FRAME_RENDER_ERROR: u8 = 11;

const RENDER_SCHEMA_VERSION: u32 = 1;
const RENDER_SAMPLE_RATE: u64 = 48_000;
const RENDER_CHANNELS: u64 = 2;
const RENDER_BYTES_PER_FRAME: u64 = RENDER_CHANNELS * 4;
const MIN_RENDER_FRAMES: u64 = 24_000;
const MAX_RENDER_FRAMES: u64 = 8_640_000;
const MAX_RENDER_PCM_BYTES: usize = (MAX_RENDER_FRAMES * RENDER_BYTES_PER_FRAME) as usize;
const MAX_RENDER_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RENDER_PROMPT_CHARS: usize = 32_000;
const MAX_RENDER_CONTROL_BYTES: usize = 1024;
const MAX_RENDER_METADATA_BYTES: usize = 8 * 1024;
const MAX_RENDER_CHUNK_BYTES: usize = 1024 * 1024;
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const IO_POLL: Duration = Duration::from_millis(50);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const SAFE_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    Unavailable,
    Protocol,
    Deadline,
    Cancelled,
}

#[derive(Debug)]
struct RenderFailure {
    kind: FailureKind,
    detail: &'static str,
}

impl RenderFailure {
    fn unavailable() -> Self {
        Self {
            kind: FailureKind::Unavailable,
            detail: "Magenta runtime is not installed or failed verification",
        }
    }

    fn protocol(detail: &'static str) -> Self {
        Self {
            kind: FailureKind::Protocol,
            detail,
        }
    }

    fn deadline() -> Self {
        Self {
            kind: FailureKind::Deadline,
            detail: "Magenta render timed out",
        }
    }

    fn cancelled() -> Self {
        Self {
            kind: FailureKind::Cancelled,
            detail: "Magenta render was cancelled",
        }
    }
}

impl From<io::Error> for RenderFailure {
    fn from(_: io::Error) -> Self {
        Self::protocol("Magenta render worker connection failed")
    }
}

#[derive(Clone)]
struct RequestCancellation {
    request: Arc<AtomicBool>,
    lifecycle: Arc<AtomicBool>,
}

impl RequestCancellation {
    fn cancelled(&self) -> bool {
        self.request.load(Ordering::Acquire) || self.lifecycle.load(Ordering::Acquire)
    }
}

struct CancelOnDrop {
    flag: Arc<AtomicBool>,
    armed: bool,
}

impl CancelOnDrop {
    fn new(flag: Arc<AtomicBool>) -> Self {
        Self { flag, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(true, Ordering::Release);
        }
    }
}

trait ProcessTree: Send {
    fn shutdown(&mut self) -> io::Result<()>;
}

struct ManagedProcess {
    child: SupervisedChild,
}

impl ProcessTree for ManagedProcess {
    fn shutdown(&mut self) -> io::Result<()> {
        let report = self.child.shutdown(Duration::from_millis(500))?;
        crate::child_process::log_shutdown("MRT2 render worker", Ok(report));
        Ok(())
    }
}

struct ManagedRenderWorker {
    stream: TcpStream,
    process: Box<dyn ProcessTree>,
    next_sequence: u64,
}

impl ManagedRenderWorker {
    fn render(
        &mut self,
        request: &WorkerRenderRequest,
        cancellation: &RequestCancellation,
    ) -> Result<Vec<u8>, RenderFailure> {
        let payload = serde_json::to_vec(request)
            .map_err(|_| RenderFailure::protocol("Magenta render request is invalid"))?;
        if payload.len() > MAX_RENDER_REQUEST_BYTES {
            return Err(RenderFailure::protocol(
                "Magenta render request is too large",
            ));
        }
        self.stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        write_frame(&mut self.stream, FRAME_RENDER_REQUEST, &payload)?;
        let duration = request.frames as f64 / RENDER_SAMPLE_RATE as f64;
        let deadline = Instant::now() + Duration::from_secs_f64((duration * 2.0).max(90.0));
        match read_render_response(&mut self.stream, request, cancellation, deadline) {
            Err(error) if error.kind == FailureKind::Cancelled => {
                let cancel = WorkerRenderCancel {
                    schema_version: RENDER_SCHEMA_VERSION,
                    job_id: request.job_id.clone(),
                    sequence: request.sequence,
                };
                if let Ok(payload) = serde_json::to_vec(&cancel) {
                    if payload.len() <= MAX_RENDER_CONTROL_BYTES {
                        let _ = write_frame(&mut self.stream, FRAME_RENDER_CANCEL, &payload);
                    }
                }
                Err(error)
            }
            result => result,
        }
    }
}

trait WorkerFactory: Send + Sync {
    fn spawn(
        &self,
        cancellation: &RequestCancellation,
    ) -> Result<ManagedRenderWorker, WorkerSpawnFailure>;
}

struct WorkerSpawnFailure {
    failure: RenderFailure,
    uncertain_process: Option<Box<dyn ProcessTree>>,
}

impl WorkerSpawnFailure {
    fn reaped(failure: RenderFailure) -> Self {
        Self {
            failure,
            uncertain_process: None,
        }
    }
}

impl From<RenderFailure> for WorkerSpawnFailure {
    fn from(failure: RenderFailure) -> Self {
        Self::reaped(failure)
    }
}

struct ManagedWorkerFactory;

impl WorkerFactory for ManagedWorkerFactory {
    fn spawn(
        &self,
        cancellation: &RequestCancellation,
    ) -> Result<ManagedRenderWorker, WorkerSpawnFailure> {
        spawn_managed_worker(cancellation)
    }
}

enum WorkerState {
    Stopped,
    Running(ManagedRenderWorker),
    /// A failed teardown left process-tree ownership uncertain. No new worker
    /// may launch, and promotion may not rename, until shutdown later succeeds.
    Uncertain {
        process: Box<dyn ProcessTree>,
        was_warm: bool,
    },
}

struct GatewayCore {
    worker: Mutex<WorkerState>,
    factory: Arc<dyn WorkerFactory>,
    lifecycle: Mutex<Arc<AtomicBool>>,
    quiescing: AtomicBool,
}

impl GatewayCore {
    fn new(factory: Arc<dyn WorkerFactory>) -> Self {
        Self {
            worker: Mutex::new(WorkerState::Stopped),
            factory,
            lifecycle: Mutex::new(Arc::new(AtomicBool::new(false))),
            quiescing: AtomicBool::new(false),
        }
    }

    fn cancellation(&self, request: Arc<AtomicBool>) -> RequestCancellation {
        RequestCancellation {
            request,
            lifecycle: self
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        }
    }

    fn render(
        &self,
        prompt: String,
        frames: u64,
        request_cancel: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, RenderFailure> {
        if self.quiescing.load(Ordering::Acquire) {
            return Err(RenderFailure::unavailable());
        }
        let cancellation = self.cancellation(request_cancel);
        if cancellation.cancelled() {
            return Err(RenderFailure::cancelled());
        }
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cancellation.cancelled() || self.quiescing.load(Ordering::Acquire) {
            return Err(RenderFailure::cancelled());
        }
        if matches!(&*worker, WorkerState::Stopped) {
            match self.factory.spawn(&cancellation) {
                Ok(spawned) => *worker = WorkerState::Running(spawned),
                Err(spawn) => {
                    if let Some(process) = spawn.uncertain_process {
                        *worker = WorkerState::Uncertain {
                            process,
                            was_warm: false,
                        };
                    }
                    return Err(spawn.failure);
                }
            }
        }
        let resident = match &mut *worker {
            WorkerState::Running(resident) => resident,
            WorkerState::Uncertain { .. } => {
                return Err(RenderFailure::protocol(
                    "Magenta render worker could not be reaped",
                ))
            }
            WorkerState::Stopped => unreachable!("worker spawn installed a running state"),
        };
        let sequence = resident.next_sequence;
        let request = WorkerRenderRequest {
            schema_version: RENDER_SCHEMA_VERSION,
            job_id: format!("render-{:032x}", rand::random::<u128>()),
            sequence,
            prompt,
            frames,
        };
        let result = resident.render(&request, &cancellation);
        if result.is_ok() && sequence < u64::MAX {
            resident.next_sequence = sequence + 1;
            return result;
        }
        let WorkerState::Running(mut finished) =
            std::mem::replace(&mut *worker, WorkerState::Stopped)
        else {
            unreachable!("render state stayed running while its lock was held")
        };
        let _ = finished.stream.shutdown(Shutdown::Both);
        if finished.process.shutdown().is_err() {
            *worker = WorkerState::Uncertain {
                process: finished.process,
                was_warm: true,
            };
            return Err(RenderFailure::protocol(
                "Magenta render worker could not be reaped",
            ));
        }
        result
    }

    /// Cancel in-flight/queued renders, then kill and reap the warm worker.
    /// Returns whether a worker was resident before the quiesce.
    fn quiesce(&self) -> Result<bool, String> {
        self.quiescing.store(true, Ordering::Release);
        self.lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .store(true, Ordering::Release);
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::mem::replace(&mut *worker, WorkerState::Stopped);
        let (mut process, was_warm) = match previous {
            WorkerState::Stopped => return Ok(false),
            WorkerState::Running(resident) => {
                let _ = resident.stream.shutdown(Shutdown::Both);
                (resident.process, true)
            }
            WorkerState::Uncertain { process, was_warm } => (process, was_warm),
        };
        match process.shutdown() {
            Ok(()) => Ok(was_warm),
            Err(_) => {
                *worker = WorkerState::Uncertain { process, was_warm };
                Err("Magenta render worker could not be reaped".to_string())
            }
        }
    }

    /// Open a fresh request generation. If an update displaced a previously warm
    /// renderer, eagerly restore it from the now-current verified generation.
    fn resume(&self, restore_warm_worker: bool) -> Result<(), String> {
        *self
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(AtomicBool::new(false));
        self.quiescing.store(false, Ordering::Release);
        if !restore_warm_worker {
            return Ok(());
        }
        let cancellation = self.cancellation(Arc::new(AtomicBool::new(false)));
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*worker {
            WorkerState::Running(_) => return Ok(()),
            WorkerState::Uncertain { .. } => {
                return Err("Magenta render worker could not be reaped".to_string())
            }
            WorkerState::Stopped => {}
        }
        match self.factory.spawn(&cancellation) {
            Ok(spawned) => *worker = WorkerState::Running(spawned),
            Err(spawn) => {
                if let Some(process) = spawn.uncertain_process {
                    *worker = WorkerState::Uncertain {
                        process,
                        was_warm: false,
                    };
                }
                return Err(spawn.failure.detail.to_string());
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct HttpState {
    core: Arc<GatewayCore>,
}

#[derive(Clone)]
struct AuthState {
    capability: Arc<str>,
}

/// The always-available public HTTP gateway. Absence of an MRT2 runtime affects
/// only render requests; it never prevents the window, model manager, or model
/// status endpoint from starting.
pub struct MagentaGateway {
    port: Option<u16>,
    capability: String,
    cancel: CancellationToken,
    core: Arc<GatewayCore>,
}

impl MagentaGateway {
    pub fn start() -> Self {
        let capability = crate::local_auth::generate_capability();
        let core = Arc::new(GatewayCore::new(Arc::new(ManagedWorkerFactory)));
        match bind_loopback() {
            Ok((listener, port)) => {
                let cancel = serve(listener, port, &capability, core.clone());
                Self {
                    port: Some(port),
                    capability,
                    cancel,
                    core,
                }
            }
            Err(error) => {
                eprintln!("lsdj-app: Magenta gateway bind failed: {error}");
                Self {
                    port: None,
                    capability,
                    cancel: CancellationToken::new(),
                    core,
                }
            }
        }
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    pub fn capability(&self) -> Option<String> {
        self.port.map(|_| self.capability.clone())
    }

    pub fn quiesce(&self) -> Result<bool, String> {
        self.core.quiesce()
    }

    pub fn resume(&self, restore_warm_worker: bool) -> Result<(), String> {
        self.core.resume(restore_warm_worker)
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
        if let Err(error) = self.core.quiesce() {
            eprintln!("lsdj-app: Magenta gateway shutdown failed: {error}");
        }
    }
}

impl Drop for MagentaGateway {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpRenderRequest {
    prompt: String,
    seconds: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkerRenderRequest {
    schema_version: u32,
    job_id: String,
    sequence: u64,
    prompt: String,
    frames: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkerRenderCancel {
    schema_version: u32,
    job_id: String,
    sequence: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenderReady {
    schema_version: u32,
    event: String,
    model: String,
    runtime: String,
    next_sequence: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenderBegin {
    schema_version: u32,
    job_id: String,
    sequence: u64,
    sample_rate: u64,
    channels: u64,
    sample_format: String,
    frames: u64,
    pcm_bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenderEnd {
    schema_version: u32,
    job_id: String,
    sequence: u64,
    frames: u64,
    pcm_bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenderError {
    schema_version: u32,
    job_id: Option<String>,
    sequence: u64,
    code: String,
    message: String,
}

async fn render_clip(State(state): State<HttpState>, body: Bytes) -> Response {
    if body.len() > MAX_RENDER_REQUEST_BYTES {
        return json_error(StatusCode::PAYLOAD_TOO_LARGE, "request body is too large");
    }
    let parsed: HttpRenderRequest = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(_) => return json_error(StatusCode::UNPROCESSABLE_ENTITY, "body must be JSON"),
    };
    let prompt = parsed.prompt.trim().to_string();
    if prompt.is_empty() {
        return json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "'prompt' must be a non-empty string",
        );
    }
    if prompt.chars().count() > MAX_RENDER_PROMPT_CHARS {
        return json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "'prompt' must be at most 32000 characters",
        );
    }
    let frames = match frames_for_seconds(parsed.seconds) {
        Some(frames) => frames,
        None => {
            return json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "'seconds' must be 0.5-180",
            )
        }
    };

    let request_cancel = Arc::new(AtomicBool::new(false));
    let mut drop_guard = CancelOnDrop::new(request_cancel.clone());
    let core = state.core.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || core.render(prompt, frames, request_cancel))
            .await;
    drop_guard.disarm();
    match result {
        Ok(Ok(pcm)) => match float32_wav(&pcm) {
            Ok(wav) => (StatusCode::OK, [(header::CONTENT_TYPE, "audio/wav")], wav).into_response(),
            Err(_) => json_error(StatusCode::BAD_GATEWAY, "Magenta returned invalid audio"),
        },
        Ok(Err(error)) => failure_response(error),
        Err(_) => json_error(StatusCode::BAD_GATEWAY, "Magenta render task failed"),
    }
}

async fn model_info() -> Response {
    let mut estimates = BTreeMap::new();
    estimates.insert("mrt2_small", 2.0);
    estimates.insert("mrt2_base", 6.0);
    axum::Json(serde_json::json!({
        "models": crate::models::magenta_models_for_gateway(),
        "sample_rate": RENDER_SAMPLE_RATE,
        "channels": RENDER_CHANNELS,
        "chunk_seconds": 1.0,
        "total_ram_gb": total_ram_gb(),
        "model_ram_estimate_gb": estimates,
    }))
    .into_response()
}

fn failure_response(error: RenderFailure) -> Response {
    let status = match error.kind {
        FailureKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        FailureKind::Protocol => StatusCode::BAD_GATEWAY,
        FailureKind::Deadline => StatusCode::GATEWAY_TIMEOUT,
        FailureKind::Cancelled => StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_GATEWAY),
    };
    json_error(status, error.detail)
}

fn json_error(status: StatusCode, detail: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "detail": detail }))).into_response()
}

fn frames_for_seconds(seconds: f64) -> Option<u64> {
    if !seconds.is_finite() || !(0.5..=180.0).contains(&seconds) {
        return None;
    }
    let frames = (seconds * RENDER_SAMPLE_RATE as f64 + 0.5).floor() as u64;
    (MIN_RENDER_FRAMES..=MAX_RENDER_FRAMES)
        .contains(&frames)
        .then_some(frames)
}

fn float32_wav(pcm: &[u8]) -> Result<Vec<u8>, ()> {
    if pcm.len() > MAX_RENDER_PCM_BYTES
        || !pcm.len().is_multiple_of(RENDER_BYTES_PER_FRAME as usize)
    {
        return Err(());
    }
    let data_len = u32::try_from(pcm.len()).map_err(|_| ())?;
    let riff_len = 36u32.checked_add(data_len).ok_or(())?;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&3u16.to_le_bytes());
    wav.extend_from_slice(&(RENDER_CHANNELS as u16).to_le_bytes());
    wav.extend_from_slice(&(RENDER_SAMPLE_RATE as u32).to_le_bytes());
    wav.extend_from_slice(&((RENDER_SAMPLE_RATE * RENDER_BYTES_PER_FRAME) as u32).to_le_bytes());
    wav.extend_from_slice(&(RENDER_BYTES_PER_FRAME as u16).to_le_bytes());
    wav.extend_from_slice(&32u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    Ok(wav)
}

fn spawn_managed_worker(
    cancellation: &RequestCancellation,
) -> Result<ManagedRenderWorker, WorkerSpawnFailure> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|_| WorkerSpawnFailure::reaped(RenderFailure::unavailable()))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| WorkerSpawnFailure::reaped(RenderFailure::unavailable()))?;
    let port = listener
        .local_addr()
        .map_err(|_| WorkerSpawnFailure::reaped(RenderFailure::unavailable()))?
        .port();
    let token = crate::local_auth::generate_capability();
    let mut command =
        crate::sidecar::authenticated_render_worker_command(crate::DEFAULT_MODEL, port, &token)
            .map_err(|_| WorkerSpawnFailure::reaped(RenderFailure::unavailable()))?;
    let mut child = crate::child_process::spawn_grouped(&mut command)
        .map_err(|_| WorkerSpawnFailure::reaped(RenderFailure::unavailable()))?;

    let result =
        accept_worker(&listener, &mut child, &token, cancellation).and_then(|mut stream| {
            stream.set_nodelay(true).ok();
            let next_sequence = read_worker_ready(
                &mut stream,
                crate::DEFAULT_MODEL,
                "pytorch-cuda",
                cancellation,
                Instant::now() + READY_TIMEOUT,
            )?;
            Ok((stream, next_sequence))
        });
    match result {
        Ok((stream, next_sequence)) => Ok(ManagedRenderWorker {
            stream,
            process: Box::new(ManagedProcess { child }),
            next_sequence,
        }),
        Err(error) => {
            let mut process: Box<dyn ProcessTree> = Box::new(ManagedProcess { child });
            match process.shutdown() {
                Ok(()) => Err(WorkerSpawnFailure::reaped(error)),
                Err(_) => Err(WorkerSpawnFailure {
                    failure: RenderFailure::protocol(
                        "Magenta render worker startup failed and could not be reaped",
                    ),
                    uncertain_process: Some(process),
                }),
            }
        }
    }
}

fn read_worker_ready(
    stream: &mut TcpStream,
    model: &str,
    runtime: &str,
    cancellation: &RequestCancellation,
    deadline: Instant,
) -> Result<u64, RenderFailure> {
    let (frame_type, payload) = read_bounded_frame(
        stream,
        &[FRAME_STATUS, FRAME_RENDER_ERROR],
        MAX_RENDER_METADATA_BYTES,
        cancellation,
        deadline,
    )?;
    if frame_type == FRAME_RENDER_ERROR {
        validate_startup_error(&payload)?;
        return Err(RenderFailure::unavailable());
    }
    let ready: RenderReady = serde_json::from_slice(&payload)
        .map_err(|_| RenderFailure::protocol("Magenta worker readiness is invalid"))?;
    if ready.schema_version != RENDER_SCHEMA_VERSION
        || ready.event != "render_ready"
        || ready.model != model
        || ready.runtime != runtime
        || ready.next_sequence != 1
    {
        return Err(RenderFailure::protocol(
            "Magenta worker readiness is invalid",
        ));
    }
    Ok(ready.next_sequence)
}

fn accept_worker(
    listener: &TcpListener,
    child: &mut SupervisedChild,
    token: &str,
    cancellation: &RequestCancellation,
) -> Result<TcpStream, RenderFailure> {
    let deadline = Instant::now() + ACCEPT_TIMEOUT;
    loop {
        if cancellation.cancelled() {
            return Err(RenderFailure::cancelled());
        }
        if Instant::now() >= deadline {
            return Err(RenderFailure::deadline());
        }
        if child
            .try_wait()
            .map_err(|_| RenderFailure::unavailable())?
            .is_some()
        {
            return Err(RenderFailure::unavailable());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                // The launch token is single-use: the first connection attempt
                // consumes it, even when authentication fails.
                stream
                    .set_nonblocking(false)
                    .map_err(|_| RenderFailure::protocol("Magenta worker connection failed"))?;
                stream
                    .set_read_timeout(Some(IO_POLL))
                    .map_err(|_| RenderFailure::protocol("Magenta worker connection failed"))?;
                let (frame_type, payload) = read_bounded_frame(
                    &mut stream,
                    &[FRAME_AUTH],
                    256,
                    cancellation,
                    deadline.min(Instant::now() + Duration::from_secs(1)),
                )?;
                if frame_type != FRAME_AUTH
                    || !(32..=256).contains(&payload.len())
                    || !crate::local_auth::constant_time_eq(&payload, token.as_bytes())
                {
                    return Err(RenderFailure::protocol(
                        "Magenta worker authentication failed",
                    ));
                }
                return Ok(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(IO_POLL);
            }
            Err(_) => return Err(RenderFailure::unavailable()),
        }
    }
}

fn validate_startup_error(payload: &[u8]) -> Result<(), RenderFailure> {
    let error: RenderError = serde_json::from_slice(payload)
        .map_err(|_| RenderFailure::protocol("Magenta worker error is invalid"))?;
    if error.schema_version != RENDER_SCHEMA_VERSION
        || error.job_id.is_some()
        || error.sequence != 0
        || error.code.is_empty()
        || error.code.len() > 64
        || error.message.len() > 512
    {
        return Err(RenderFailure::protocol("Magenta worker error is invalid"));
    }
    Ok(())
}

fn validate_render_error(
    payload: &[u8],
    request: &WorkerRenderRequest,
) -> Result<(), RenderFailure> {
    let error: RenderError = serde_json::from_slice(payload)
        .map_err(|_| RenderFailure::protocol("Magenta worker error is invalid"))?;
    if error.schema_version != RENDER_SCHEMA_VERSION
        || error.job_id.as_deref() != Some(&request.job_id)
        || error.sequence != request.sequence
        || error.code.is_empty()
        || error.code.len() > 64
        || error.message.len() > 512
    {
        return Err(RenderFailure::protocol("Magenta worker error is invalid"));
    }
    Err(RenderFailure::protocol("Magenta render worker failed"))
}

fn read_render_response(
    stream: &mut TcpStream,
    request: &WorkerRenderRequest,
    cancellation: &RequestCancellation,
    deadline: Instant,
) -> Result<Vec<u8>, RenderFailure> {
    let (frame_type, payload) = read_bounded_frame(
        stream,
        &[FRAME_RENDER_BEGIN, FRAME_RENDER_ERROR],
        MAX_RENDER_METADATA_BYTES,
        cancellation,
        deadline,
    )?;
    if frame_type == FRAME_RENDER_ERROR {
        validate_render_error(&payload, request)?;
        unreachable!("a valid worker error is returned as a render failure");
    }
    let begin: RenderBegin = serde_json::from_slice(&payload)
        .map_err(|_| RenderFailure::protocol("Magenta render begin is invalid"))?;
    let expected_bytes = request
        .frames
        .checked_mul(RENDER_BYTES_PER_FRAME)
        .ok_or_else(|| RenderFailure::protocol("Magenta render size overflow"))?;
    if begin.schema_version != RENDER_SCHEMA_VERSION
        || begin.job_id != request.job_id
        || begin.sequence != request.sequence
        || begin.sample_rate != RENDER_SAMPLE_RATE
        || begin.channels != RENDER_CHANNELS
        || begin.sample_format != "f32le"
        || begin.frames != request.frames
        || begin.pcm_bytes != expected_bytes
        || begin.pcm_bytes as usize > MAX_RENDER_PCM_BYTES
    {
        return Err(RenderFailure::protocol("Magenta render begin is invalid"));
    }

    let mut pcm = Vec::with_capacity(expected_bytes as usize);
    let mut digest = Sha256::new();
    loop {
        let (frame_type, payload) = read_bounded_frame(
            stream,
            &[FRAME_RENDER_CHUNK, FRAME_RENDER_END, FRAME_RENDER_ERROR],
            MAX_RENDER_CHUNK_BYTES,
            cancellation,
            deadline,
        )?;
        match frame_type {
            FRAME_RENDER_CHUNK => {
                if payload.is_empty()
                    || !payload
                        .len()
                        .is_multiple_of(RENDER_BYTES_PER_FRAME as usize)
                    || pcm.len().saturating_add(payload.len()) > expected_bytes as usize
                {
                    return Err(RenderFailure::protocol("Magenta PCM chunk is invalid"));
                }
                digest.update(&payload);
                pcm.extend_from_slice(&payload);
            }
            FRAME_RENDER_ERROR => {
                if payload.len() > MAX_RENDER_METADATA_BYTES {
                    return Err(RenderFailure::protocol("Magenta worker error is too large"));
                }
                validate_render_error(&payload, request)?;
                unreachable!("a valid worker error is returned as a render failure");
            }
            FRAME_RENDER_END => {
                if payload.len() > MAX_RENDER_METADATA_BYTES {
                    return Err(RenderFailure::protocol("Magenta render end is too large"));
                }
                let end: RenderEnd = serde_json::from_slice(&payload)
                    .map_err(|_| RenderFailure::protocol("Magenta render end is invalid"))?;
                let actual_hash = hex::encode(digest.finalize());
                if end.schema_version != RENDER_SCHEMA_VERSION
                    || end.job_id != request.job_id
                    || end.sequence != request.sequence
                    || end.frames != request.frames
                    || end.pcm_bytes != expected_bytes
                    || pcm.len() != expected_bytes as usize
                    || end.sha256.len() != 64
                    || !end.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
                    || end.sha256 != actual_hash
                {
                    return Err(RenderFailure::protocol("Magenta render end is invalid"));
                }
                return Ok(pcm);
            }
            _ => unreachable!("frame type was checked"),
        }
    }
}

fn write_frame(writer: &mut impl Write, frame_type: u8, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame is too large"))?;
    writer.write_all(&[frame_type])?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

fn read_bounded_frame(
    reader: &mut impl Read,
    allowed_types: &[u8],
    maximum: usize,
    cancellation: &RequestCancellation,
    deadline: Instant,
) -> Result<(u8, Vec<u8>), RenderFailure> {
    let mut header = [0u8; 5];
    read_exact_cancellable(reader, &mut header, cancellation, deadline)?;
    let frame_type = header[0];
    if !allowed_types.contains(&frame_type) {
        return Err(RenderFailure::protocol("Magenta frame is out of order"));
    }
    let length = u32::from_le_bytes(header[1..5].try_into().expect("four bytes")) as usize;
    // Metadata and PCM share this helper. A caller that accepts chunks passes
    // the chunk cap, but control metadata remains capped before allocation or
    // reading so an END/ERROR frame cannot consume a chunk-sized buffer.
    let maximum = if frame_type == FRAME_RENDER_CHUNK {
        maximum
    } else {
        maximum.min(MAX_RENDER_METADATA_BYTES)
    };
    if length > maximum {
        return Err(RenderFailure::protocol(
            "Magenta frame exceeds its size cap",
        ));
    }
    let mut payload = vec![0u8; length];
    read_exact_cancellable(reader, &mut payload, cancellation, deadline)?;
    Ok((frame_type, payload))
}

fn read_exact_cancellable(
    reader: &mut impl Read,
    mut output: &mut [u8],
    cancellation: &RequestCancellation,
    deadline: Instant,
) -> Result<(), RenderFailure> {
    while !output.is_empty() {
        if cancellation.cancelled() {
            return Err(RenderFailure::cancelled());
        }
        if Instant::now() >= deadline {
            return Err(RenderFailure::deadline());
        }
        match reader.read(output) {
            Ok(0) => {
                return Err(RenderFailure::protocol(
                    "Magenta render response was truncated",
                ))
            }
            Ok(read) => output = &mut output[read..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => {
                return Err(RenderFailure::protocol(
                    "Magenta render worker connection failed",
                ))
            }
        }
    }
    Ok(())
}

fn bind_loopback() -> io::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    Ok((listener, port))
}

fn serve(
    listener: TcpListener,
    port: u16,
    capability: &str,
    core: Arc<GatewayCore>,
) -> CancellationToken {
    let router = gateway_router(capability, core);
    let cancel = CancellationToken::new();
    let serve_cancel = cancel.clone();
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("lsdj-app: Magenta gateway listener failed: {error}");
                return;
            }
        };
        println!("lsdj-app: Magenta gateway on http://127.0.0.1:{port}");
        if let Err(error) = axum::serve(listener, router)
            .with_graceful_shutdown(async move { serve_cancel.cancelled().await })
            .await
        {
            eprintln!("lsdj-app: Magenta gateway stopped: {error}");
        }
    });
    cancel
}

fn gateway_router(capability: &str, core: Arc<GatewayCore>) -> Router {
    let auth = AuthState {
        capability: Arc::from(capability),
    };
    Router::new()
        .route("/api/render", post(render_clip).options(preflight))
        .route("/api/models", get(model_info).options(preflight))
        .layer(DefaultBodyLimit::max(MAX_RENDER_REQUEST_BYTES))
        .layer(axum::middleware::from_fn_with_state(auth, authenticate))
        .with_state(HttpState { core })
}

async fn preflight() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn authenticate(State(auth): State<AuthState>, request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if origin
        .as_deref()
        .is_some_and(|origin| !SAFE_ORIGINS.contains(&origin))
    {
        return json_error(StatusCode::FORBIDDEN, "origin is not allowed");
    }
    if request.method() == Method::OPTIONS {
        let requested_method = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|value| value.to_str().ok());
        let requested_headers = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .split(',')
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if origin.is_none()
            || !matches!(requested_method, Some("GET" | "POST"))
            || requested_headers
                .iter()
                .any(|value| !matches!(value.as_str(), "content-type" | "x-lsdj-capability"))
        {
            return json_error(StatusCode::FORBIDDEN, "preflight rejected");
        }
        let mut response = StatusCode::NO_CONTENT.into_response();
        add_cors(&mut response, origin.as_deref().expect("origin checked"));
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("content-type, x-lsdj-capability"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
        return response;
    }
    let supplied = request
        .headers()
        .get("x-lsdj-capability")
        .map(|value| value.as_bytes())
        .unwrap_or_default();
    if !crate::local_auth::constant_time_eq(supplied, auth.capability.as_bytes()) {
        return json_error(StatusCode::UNAUTHORIZED, "authentication required");
    }
    let mut response = next.run(request).await;
    if let Some(origin) = origin.as_deref() {
        add_cors(&mut response, origin);
    }
    response
}

fn add_cors(response: &mut Response, origin: &str) {
    if let Ok(origin) = HeaderValue::from_str(origin) {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
    }
}

#[cfg(target_os = "linux")]
fn total_ram_gb() -> Option<f64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .map(|kilobytes| kilobytes / 1024.0 / 1024.0)
}

#[cfg(target_os = "windows")]
fn total_ram_gb() -> Option<f64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `status` is writable and advertises its exact structure size.
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0)
        .then_some(status.ullTotalPhys as f64 / 1024.0 / 1024.0 / 1024.0)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn total_ram_gb() -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::*;

    #[derive(Clone, Copy)]
    enum Scenario {
        Valid,
        WrongSequence,
        WrongFrames,
        WrongTotals,
        WrongHash,
        OutOfOrder,
        OversizeEnd,
        MisalignedChunk,
        Stall,
        LongStall,
    }

    struct FakeProcess {
        shutdowns: Arc<AtomicUsize>,
        shutdown_failures: Arc<AtomicUsize>,
    }

    impl ProcessTree for FakeProcess {
        fn shutdown(&mut self) -> io::Result<()> {
            self.shutdowns.fetch_add(1, Ordering::AcqRel);
            if self
                .shutdown_failures
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(io::Error::other("fake process is not reaped"));
            }
            Ok(())
        }
    }

    struct FakeFactory {
        scenarios: Mutex<VecDeque<Scenario>>,
        spawns: Arc<AtomicUsize>,
        shutdowns: Arc<AtomicUsize>,
        shutdown_failures: Arc<AtomicUsize>,
        sequences: Arc<Mutex<Vec<u64>>>,
    }

    impl FakeFactory {
        fn new(scenarios: impl IntoIterator<Item = Scenario>) -> Arc<Self> {
            Arc::new(Self {
                scenarios: Mutex::new(scenarios.into_iter().collect()),
                spawns: Arc::new(AtomicUsize::new(0)),
                shutdowns: Arc::new(AtomicUsize::new(0)),
                shutdown_failures: Arc::new(AtomicUsize::new(0)),
                sequences: Arc::new(Mutex::new(Vec::new())),
            })
        }

        fn fail_shutdowns(&self, count: usize) {
            self.shutdown_failures.store(count, Ordering::Release);
        }
    }

    impl WorkerFactory for FakeFactory {
        fn spawn(
            &self,
            _cancellation: &RequestCancellation,
        ) -> Result<ManagedRenderWorker, WorkerSpawnFailure> {
            let scenario = self
                .scenarios
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .ok_or_else(RenderFailure::unavailable)?;
            self.spawns.fetch_add(1, Ordering::AcqRel);
            let listener =
                TcpListener::bind("127.0.0.1:0").map_err(|_| RenderFailure::unavailable())?;
            let address = listener
                .local_addr()
                .map_err(|_| RenderFailure::unavailable())?;
            let client = TcpStream::connect(address).map_err(RenderFailure::from)?;
            client
                .set_read_timeout(Some(IO_POLL))
                .map_err(RenderFailure::from)?;
            let (server, _) = listener.accept().map_err(RenderFailure::from)?;
            let sequences = self.sequences.clone();
            thread::spawn(move || serve_scenario(server, scenario, sequences));
            Ok(ManagedRenderWorker {
                stream: client,
                process: Box::new(FakeProcess {
                    shutdowns: self.shutdowns.clone(),
                    shutdown_failures: self.shutdown_failures.clone(),
                }),
                next_sequence: 1,
            })
        }
    }

    fn read_test_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut header = [0u8; 5];
        stream.read_exact(&mut header).expect("request header");
        let length = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; length];
        stream.read_exact(&mut payload).expect("request payload");
        (header[0], payload)
    }

    fn serve_scenario(mut stream: TcpStream, scenario: Scenario, sequences: Arc<Mutex<Vec<u64>>>) {
        let (frame_type, payload) = read_test_frame(&mut stream);
        assert_eq!(frame_type, FRAME_RENDER_REQUEST);
        let request: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        let job_id = request["jobId"].as_str().unwrap();
        let sequence = request["sequence"].as_u64().unwrap();
        sequences
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(sequence);
        let frames = request["frames"].as_u64().unwrap();
        if matches!(scenario, Scenario::Stall | Scenario::LongStall) {
            let delay = if matches!(scenario, Scenario::LongStall) {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(250)
            };
            thread::sleep(delay);
            return;
        }
        if matches!(scenario, Scenario::OutOfOrder) {
            write_frame(&mut stream, FRAME_RENDER_END, b"{}").ok();
            return;
        }

        let pcm = vec![0x3fu8; frames as usize * RENDER_BYTES_PER_FRAME as usize];
        let begin_sequence = if matches!(scenario, Scenario::WrongSequence) {
            sequence + 1
        } else {
            sequence
        };
        let begin_frames = if matches!(scenario, Scenario::WrongFrames) {
            frames + 1
        } else {
            frames
        };
        let begin = serde_json::json!({
            "schemaVersion": RENDER_SCHEMA_VERSION,
            "jobId": job_id,
            "sequence": begin_sequence,
            "sampleRate": RENDER_SAMPLE_RATE,
            "channels": RENDER_CHANNELS,
            "sampleFormat": "f32le",
            "frames": begin_frames,
            "pcmBytes": pcm.len(),
        });
        if write_frame(
            &mut stream,
            FRAME_RENDER_BEGIN,
            &serde_json::to_vec(&begin).unwrap(),
        )
        .is_err()
        {
            return;
        }
        if matches!(scenario, Scenario::OversizeEnd) {
            let _ = stream.write_all(&[FRAME_RENDER_END]);
            let _ = stream.write_all(&((MAX_RENDER_METADATA_BYTES + 1) as u32).to_le_bytes());
            return;
        }
        let chunk = if matches!(scenario, Scenario::MisalignedChunk) {
            &pcm[..3]
        } else {
            &pcm
        };
        if write_frame(&mut stream, FRAME_RENDER_CHUNK, chunk).is_err() {
            return;
        }
        let reported_bytes = if matches!(scenario, Scenario::WrongTotals) {
            pcm.len() as u64 + RENDER_BYTES_PER_FRAME
        } else {
            pcm.len() as u64
        };
        let hash = if matches!(scenario, Scenario::WrongHash) {
            "0".repeat(64)
        } else {
            hex::encode(Sha256::digest(&pcm))
        };
        let end = serde_json::json!({
            "schemaVersion": RENDER_SCHEMA_VERSION,
            "jobId": job_id,
            "sequence": sequence,
            "frames": frames,
            "pcmBytes": reported_bytes,
            "sha256": hash,
        });
        write_frame(
            &mut stream,
            FRAME_RENDER_END,
            &serde_json::to_vec(&end).unwrap(),
        )
        .ok();
    }

    fn render_with(
        core: &GatewayCore,
        cancellation: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, RenderFailure> {
        core.render("test prompt".to_string(), 2, cancellation)
    }

    #[test]
    fn seconds_are_converted_to_authoritative_integer_frames() {
        assert_eq!(frames_for_seconds(0.5), Some(24_000));
        assert_eq!(
            frames_for_seconds((24_000.5) / RENDER_SAMPLE_RATE as f64),
            Some(24_001)
        );
        assert_eq!(frames_for_seconds(180.0), Some(MAX_RENDER_FRAMES));
        assert_eq!(frames_for_seconds(0.499), None);
        assert_eq!(frames_for_seconds(f64::NAN), None);
    }

    #[test]
    fn valid_fake_worker_response_is_accepted_exactly() {
        let factory = FakeFactory::new([Scenario::Valid]);
        let core = GatewayCore::new(factory.clone());
        let pcm = render_with(&core, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(pcm, vec![0x3f; 2 * RENDER_BYTES_PER_FRAME as usize]);
        assert_eq!(factory.spawns.load(Ordering::Acquire), 1);
        assert_eq!(core.quiesce(), Ok(true));
        assert_eq!(factory.shutdowns.load(Ordering::Acquire), 1);
    }

    #[test]
    fn every_protocol_violation_discards_and_reaps_the_worker() {
        for scenario in [
            Scenario::WrongSequence,
            Scenario::WrongFrames,
            Scenario::WrongTotals,
            Scenario::WrongHash,
            Scenario::OutOfOrder,
            Scenario::OversizeEnd,
            Scenario::MisalignedChunk,
        ] {
            let factory = FakeFactory::new([scenario]);
            let core = GatewayCore::new(factory.clone());
            let error = render_with(&core, Arc::new(AtomicBool::new(false))).unwrap_err();
            assert_eq!(error.kind, FailureKind::Protocol);
            assert_eq!(factory.shutdowns.load(Ordering::Acquire), 1);
            assert!(matches!(
                &*core.worker.lock().unwrap(),
                WorkerState::Stopped
            ));
        }
    }

    #[test]
    fn next_request_recovers_with_a_fresh_worker_after_failure() {
        let factory = FakeFactory::new([Scenario::WrongHash, Scenario::Valid]);
        let core = GatewayCore::new(factory.clone());
        assert!(render_with(&core, Arc::new(AtomicBool::new(false))).is_err());
        assert!(render_with(&core, Arc::new(AtomicBool::new(false))).is_ok());
        assert_eq!(factory.spawns.load(Ordering::Acquire), 2);
        assert_eq!(*factory.sequences.lock().unwrap(), [1, 1]);
        assert_eq!(factory.shutdowns.load(Ordering::Acquire), 1);
        assert_eq!(core.quiesce(), Ok(true));
        assert_eq!(factory.shutdowns.load(Ordering::Acquire), 2);
    }

    #[test]
    fn uncertain_reap_state_is_retained_and_blocks_promotion() {
        let factory = FakeFactory::new([Scenario::Valid]);
        let core = GatewayCore::new(factory.clone());
        assert!(render_with(&core, Arc::new(AtomicBool::new(false))).is_ok());
        factory.fail_shutdowns(1);

        assert!(core.quiesce().is_err());
        assert!(matches!(
            &*core.worker.lock().unwrap(),
            WorkerState::Uncertain { .. }
        ));
        assert_eq!(factory.shutdowns.load(Ordering::Acquire), 1);

        assert_eq!(core.quiesce(), Ok(true));
        assert!(matches!(
            &*core.worker.lock().unwrap(),
            WorkerState::Stopped
        ));
        assert_eq!(factory.shutdowns.load(Ordering::Acquire), 2);
    }

    struct FailedStartupFactory {
        shutdowns: Arc<AtomicUsize>,
    }

    impl WorkerFactory for FailedStartupFactory {
        fn spawn(
            &self,
            _cancellation: &RequestCancellation,
        ) -> Result<ManagedRenderWorker, WorkerSpawnFailure> {
            // The production factory reaches this shape only after failed
            // accept/readiness cleanup. Count that first failed reap here; the
            // retained process succeeds when quiesce retries it.
            self.shutdowns.store(1, Ordering::Release);
            Err(WorkerSpawnFailure {
                failure: RenderFailure::protocol(
                    "Magenta render worker startup failed and could not be reaped",
                ),
                uncertain_process: Some(Box::new(FakeProcess {
                    shutdowns: self.shutdowns.clone(),
                    shutdown_failures: Arc::new(AtomicUsize::new(0)),
                })),
            })
        }
    }

    #[test]
    fn failed_startup_cleanup_retains_process_until_positive_reap() {
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let core = GatewayCore::new(Arc::new(FailedStartupFactory {
            shutdowns: shutdowns.clone(),
        }));

        assert!(render_with(&core, Arc::new(AtomicBool::new(false))).is_err());
        assert!(matches!(
            &*core.worker.lock().unwrap(),
            WorkerState::Uncertain { .. }
        ));
        assert_eq!(shutdowns.load(Ordering::Acquire), 1);

        assert_eq!(core.quiesce(), Ok(false));
        assert!(matches!(
            &*core.worker.lock().unwrap(),
            WorkerState::Stopped
        ));
        assert_eq!(shutdowns.load(Ordering::Acquire), 2);
    }

    async fn host_test_router(
        core: Arc<GatewayCore>,
        capability: &str,
    ) -> (std::net::SocketAddr, CancellationToken) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let serve_cancel = cancel.clone();
        let router = gateway_router(capability, core);
        tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { serve_cancel.cancelled().await })
                .await
                .unwrap();
        });
        (address, cancel)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn router_enforces_auth_cors_body_and_request_bounds() {
        let capability = "c".repeat(64);
        let core = Arc::new(GatewayCore::new(FakeFactory::new([])));
        let (address, cancel) = host_test_router(core, &capability).await;
        let client = reqwest::Client::new();
        let render = format!("http://{address}/api/render");

        assert_eq!(
            client.get(&render).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(&render)
                .header("x-lsdj-capability", "wrong")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(&render)
                .header("x-lsdj-capability", &capability)
                .header(header::ORIGIN, "https://hostile.example")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );

        let allowed = client
            .get(&render)
            .header("x-lsdj-capability", &capability)
            .header(header::ORIGIN, SAFE_ORIGINS[0])
            .send()
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            allowed.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("tauri://localhost"))
        );

        let preflight = client
            .request(Method::OPTIONS, &render)
            .header(header::ORIGIN, SAFE_ORIGINS[0])
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "content-type, x-lsdj-capability",
            )
            .send()
            .await
            .unwrap();
        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            preflight.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("tauri://localhost"))
        );

        let oversized = client
            .post(&render)
            .header("x-lsdj-capability", &capability)
            .header(header::CONTENT_TYPE, "application/json")
            .body(vec![b'x'; MAX_RENDER_REQUEST_BYTES + 1])
            .send()
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

        for invalid in [
            serde_json::json!({"prompt": "ok", "seconds": 2.0, "extra": true}),
            serde_json::json!({"prompt": "   ", "seconds": 2.0}),
            serde_json::json!({"prompt": "x".repeat(MAX_RENDER_PROMPT_CHARS + 1), "seconds": 2.0}),
            serde_json::json!({"prompt": "ok", "seconds": 0.49}),
            serde_json::json!({"prompt": "ok", "seconds": 180.01}),
        ] {
            let response = client
                .post(&render)
                .header("x-lsdj-capability", &capability)
                .json(&invalid)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
        cancel.cancel();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_http_disconnect_cancels_kills_and_reaps_worker() {
        let capability = "d".repeat(64);
        let factory = FakeFactory::new([Scenario::LongStall]);
        let core = Arc::new(GatewayCore::new(factory.clone()));
        let (address, cancel) = host_test_router(core.clone(), &capability).await;
        let body = br#"{"prompt":"disconnect me","seconds":2.0}"#;
        let mut stream = TcpStream::connect(address).unwrap();
        write!(
            stream,
            "POST /api/render HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nx-lsdj-capability: {capability}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
        stream.flush().unwrap();

        let spawn_deadline = Instant::now() + Duration::from_secs(1);
        while factory.spawns.load(Ordering::Acquire) == 0 && Instant::now() < spawn_deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(factory.spawns.load(Ordering::Acquire), 1);
        drop(stream);

        let reap_deadline = Instant::now() + Duration::from_secs(1);
        while factory.shutdowns.load(Ordering::Acquire) == 0 && Instant::now() < reap_deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(factory.shutdowns.load(Ordering::Acquire), 1);
        assert!(matches!(
            &*core.worker.lock().unwrap(),
            WorkerState::Stopped
        ));
        cancel.cancel();
    }

    #[test]
    fn cancellation_interrupts_a_stalled_worker_and_reaps_it() {
        let factory = FakeFactory::new([Scenario::Stall]);
        let core = Arc::new(GatewayCore::new(factory.clone()));
        let cancellation = Arc::new(AtomicBool::new(false));
        let flag = cancellation.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            flag.store(true, Ordering::Release);
        });
        let error = render_with(&core, cancellation).unwrap_err();
        assert_eq!(error.kind, FailureKind::Cancelled);
        assert_eq!(factory.shutdowns.load(Ordering::Acquire), 1);
        assert!(matches!(
            &*core.worker.lock().unwrap(),
            WorkerState::Stopped
        ));
    }

    #[test]
    fn deadline_and_drop_cancellation_are_observed_while_reading() {
        let request = Arc::new(AtomicBool::new(false));
        {
            let _guard = CancelOnDrop::new(request.clone());
        }
        assert!(request.load(Ordering::Acquire));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(IO_POLL)).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let cancellation = RequestCancellation {
            request: Arc::new(AtomicBool::new(false)),
            lifecycle: Arc::new(AtomicBool::new(false)),
        };
        let error = read_bounded_frame(
            &mut client,
            &[FRAME_RENDER_BEGIN],
            MAX_RENDER_METADATA_BYTES,
            &cancellation,
            Instant::now() + Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(error.kind, FailureKind::Deadline);
    }

    fn protocol_test_python() -> Option<std::path::PathBuf> {
        let mut candidates = Vec::new();
        if let Some(configured) = std::env::var_os("LSDJ_TEST_PYTHON") {
            candidates.push(configured.into());
        }
        candidates.push("/opt/homebrew/bin/python3".into());
        candidates.push("python3".into());
        candidates.push("python".into());
        candidates.into_iter().find(|candidate| {
            let Ok(output) = std::process::Command::new(candidate)
                .arg("--version")
                .output()
            else {
                return false;
            };
            let version = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let Some(version) = version.split_whitespace().nth(1) else {
                return false;
            };
            let mut parts = version
                .split('.')
                .filter_map(|part| part.parse::<u32>().ok());
            matches!(
                (parts.next(), parts.next()),
                (Some(major), Some(minor)) if major > 3 || (major == 3 && minor >= 11)
            )
        })
    }

    #[test]
    fn rust_gateway_round_trips_two_requests_with_the_real_python_protocol() {
        let Some(python) = protocol_test_python() else {
            eprintln!("skipping Python protocol compatibility test: Python 3.11+ unavailable");
            return;
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let token = crate::local_auth::generate_capability();
        let sidecar =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../backend/lsdj/sidecar.py");
        let harness = r#"
import importlib.util
import socket
import sys
import types

package = types.ModuleType("lsdj")
package.__path__ = []
sys.modules["lsdj"] = package
mrt2 = types.ModuleType("lsdj.mrt2")
mrt2.AUTO_RUNTIME = "auto"
mrt2.PYTORCH_CUDA_RUNTIME = "pytorch-cuda"
mrt2.RUNTIME_CHOICES = ("auto", "mlx", "pytorch-cuda")
mrt2.create_engine = lambda **kwargs: None
mrt2.public_startup_error = lambda error: str(error)
mrt2.runtime_manifest = lambda: {}
sys.modules["lsdj.mrt2"] = mrt2
worker = types.ModuleType("lsdj.worker")
worker.run_deck_worker = lambda *args, **kwargs: None
sys.modules["lsdj.worker"] = worker
spec = importlib.util.spec_from_file_location("lsdj.sidecar", sys.argv[3])
sidecar = importlib.util.module_from_spec(spec)
sys.modules["lsdj.sidecar"] = sidecar
spec.loader.exec_module(sidecar)

class Engine:
    def warm_up(self):
        pass
    def render_clip(self, prompt, seconds):
        frames = int(seconds * sidecar.RENDER_SAMPLE_RATE + 0.5)
        return b"\0" * (frames * sidecar.RENDER_BYTES_PER_FRAME)

sock = socket.create_connection(("127.0.0.1", int(sys.argv[1])))
sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
sidecar.write_frame(sock, sidecar.FRAME_AUTH, sys.argv[2].encode())
sidecar.run_render_worker(
    sock,
    "mrt2_small",
    runtime="pytorch-cuda",
    engine_factory=lambda model: Engine(),
)
"#;
        let mut command = std::process::Command::new(python);
        command
            .arg("-c")
            .arg(harness)
            .arg(port.to_string())
            .arg(&token)
            .arg(&sidecar);
        let mut child = crate::child_process::spawn_grouped(&mut command).unwrap();
        let cancellation = RequestCancellation {
            request: Arc::new(AtomicBool::new(false)),
            lifecycle: Arc::new(AtomicBool::new(false)),
        };
        let mut stream = accept_worker(&listener, &mut child, &token, &cancellation).unwrap();
        let next_sequence = read_worker_ready(
            &mut stream,
            "mrt2_small",
            "pytorch-cuda",
            &cancellation,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(next_sequence, 1);

        let core = GatewayCore::new(FakeFactory::new(std::iter::empty()));
        *core.worker.lock().unwrap() = WorkerState::Running(ManagedRenderWorker {
            stream,
            process: Box::new(ManagedProcess { child }),
            next_sequence,
        });
        let first = core
            .render(
                "first compatibility render".to_string(),
                MIN_RENDER_FRAMES,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let second = core
            .render(
                "second compatibility render".to_string(),
                MIN_RENDER_FRAMES,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_eq!(
            first.len(),
            MIN_RENDER_FRAMES as usize * RENDER_BYTES_PER_FRAME as usize
        );
        assert_eq!(second.len(), first.len());
        assert!(matches!(
            &*core.worker.lock().unwrap(),
            WorkerState::Running(ManagedRenderWorker {
                next_sequence: 3,
                ..
            })
        ));
        assert_eq!(core.quiesce(), Ok(true));
    }
}
