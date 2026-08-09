"""Native inference sidecar transport (ADR-0019 and ADR-0037).

The Rust shell (`src-tauri/src/sidecar.rs`) spawns one process per MLX deck on
macOS, or one shared PyTorch/CUDA process holding two independent deck states on
Linux and Windows. It accepts a loopback-TCP connection and runs the unchanged
`run_deck_worker` generation loop (`worker.py`) with its `cmd_queue` / `out_queue`
bridged to the socket. Only transport and process topology differ.

Wire protocol (mirrors `src-tauri/src/sidecar.rs`):

    [u8 type][u32 little-endian length][length bytes payload]

- PCM (sidecar → engine): the worker's ``('audio', bytes)`` — interleaved-stereo
  f32 LE @ 48 kHz.
- STATUS (sidecar → engine): the worker's ``('status', dict)`` as UTF-8 JSON.
- CONTROL (engine → sidecar): a deck command (``play``/``stop``/``set_style``…)
  as UTF-8 JSON.

The dedicated MRT2 clip renderer uses the same authenticated connection but a
separate strict protocol: serial JSON RENDER_REQUEST messages with authoritative
integer frame counts and monotonically increasing sequence numbers (or a cancel
matching the active job/sequence), then exact RENDER_BEGIN metadata, bounded
aligned RENDER_CHUNK frames, and a RENDER_END carrying the exact identity, byte
count, and SHA-256. It never accepts deck control frames or unbounded PCM.

Shared-worker frames prefix payloads with a single deck byte (0 or 1). The Rust
and Python transport tests cover both forms without loading either model stack.

The transport (framing + the queue adapters) is testable against a socketpair
with a fake engine — no model, no Rust; see `tests/test_sidecar.py`. The
model-loaded round-trip is a native-checklist item.
"""

import argparse
import hashlib
import json
import math
import os
import queue
import re
import socket
import struct
import sys
import threading
import time
from dataclasses import dataclass
from typing import Any, BinaryIO, Callable, Mapping, MutableMapping

from .mrt2 import (
    AUTO_RUNTIME,
    PYTORCH_CUDA_RUNTIME,
    RUNTIME_CHOICES,
    create_engine,
    public_startup_error,
    runtime_manifest,
)
from .worker import run_deck_worker

FRAME_PCM = 1
FRAME_STATUS = 2
FRAME_CONTROL = 3
# Engine → sidecar: a style-sample embed (M15). Binary, not JSON, because it
# carries raw PCM: [u32 LE id length][id utf-8][interleaved f32 LE PCM].
FRAME_EMBED = 4
# First sidecar -> host frame. The per-launch token is delivered only through the
# child's scrubbed environment and proves that the connector is the process Rust
# just spawned, rather than another local process racing the loopback accept.
FRAME_AUTH = 5
# Native host -> dedicated MRT2 render worker. The JSON payload is a single
# strict request; render workers never accept deck-control frames.
FRAME_RENDER_REQUEST = 6
# Render worker -> native host. BEGIN fixes the expected audio identity and byte
# count before any payload; CHUNK carries aligned f32le PCM; END authenticates
# the completed byte stream with its exact count and SHA-256.
FRAME_RENDER_BEGIN = 7
FRAME_RENDER_CHUNK = 8
FRAME_RENDER_END = 9
# Native host -> render worker. In-flight model calls are not cooperatively
# cancellable, so this frame makes the disposable worker exit and release CUDA.
FRAME_RENDER_CANCEL = 10
# Render worker -> native host. Diagnostics are bounded and path-free.
FRAME_RENDER_ERROR = 11

MAX_FRAME_BYTES = 16 * 1024 * 1024
MAX_EMBED_ID_BYTES = 4 * 1024
WORKER_TOKEN_ENV = "LSDJ_WORKER_LAUNCH_TOKEN"

RENDER_SCHEMA_VERSION = 1
RENDER_SAMPLE_RATE = 48_000
RENDER_CHANNELS = 2
RENDER_SAMPLE_WIDTH = 4
RENDER_BYTES_PER_FRAME = RENDER_CHANNELS * RENDER_SAMPLE_WIDTH
MIN_RENDER_SECONDS = 0.5
MAX_RENDER_SECONDS = 180.0
MIN_RENDER_FRAMES = 24_000
MAX_RENDER_FRAMES = 8_640_000
MAX_RENDER_PROMPT_CHARS = 32_000
MAX_RENDER_REQUEST_BYTES = 64 * 1024
MAX_RENDER_CONTROL_BYTES = 1024
MAX_RENDER_PCM_BYTES = MAX_RENDER_FRAMES * RENDER_BYTES_PER_FRAME
RENDER_PCM_CHUNK_BYTES = 1024 * 1024
MAX_RENDER_METADATA_BYTES = 8 * 1024
RENDER_WRITE_POLL_SECONDS = 0.01
RENDER_WRITE_TIMEOUT_SECONDS = 5.0
MAX_U64 = (1 << 64) - 1
_RENDER_JOB_ID = re.compile(r"^[A-Za-z0-9_-]{16,80}$")

# u8 frame type, u32 little-endian payload length.
_HEADER = struct.Struct("<BI")
_FRAME_TYPES = frozenset(
    {
        FRAME_PCM,
        FRAME_STATUS,
        FRAME_CONTROL,
        FRAME_EMBED,
        FRAME_AUTH,
        FRAME_RENDER_REQUEST,
        FRAME_RENDER_BEGIN,
        FRAME_RENDER_CHUNK,
        FRAME_RENDER_END,
        FRAME_RENDER_CANCEL,
        FRAME_RENDER_ERROR,
    }
)


class RenderProtocolError(ValueError):
    """The dedicated render connection violated its bounded wire contract."""


@dataclass(frozen=True)
class RenderRequest:
    job_id: str
    sequence: int
    prompt: str
    frames: int

    @property
    def seconds(self) -> float:
        # Frames are authoritative on the wire. Seconds exist only at the
        # upstream engine boundary, so Python never independently rounds the
        # user's duration.
        return self.frames / RENDER_SAMPLE_RATE

    @property
    def pcm_bytes(self) -> int:
        return self.frames * RENDER_BYTES_PER_FRAME


@dataclass(frozen=True)
class RenderCancel:
    job_id: str
    sequence: int


@dataclass(frozen=True)
class _RenderReaderFailure:
    message: str


_RenderCommand = RenderRequest | RenderCancel | _RenderReaderFailure | None


class _RenderWorkerStopped(Exception):
    """Internal control-flow marker after the disposable worker is stopped."""


class _RenderWriteError(Exception):
    """A response frame could not be committed within the bounded deadline."""


def write_frame(sock: socket.socket, frame_type: int, payload: bytes) -> None:
    """Send one framed message. `sendall` is atomic enough here: the worker loop
    is the only writer, so frames never interleave."""
    if frame_type not in _FRAME_TYPES:
        raise ValueError(f"unknown sidecar frame type {frame_type}")
    if len(payload) > MAX_FRAME_BYTES:
        raise ValueError(f"sidecar frame length {len(payload)} exceeds the cap")
    sock.sendall(_HEADER.pack(frame_type, len(payload)) + payload)


def read_frame(reader) -> tuple[int, bytes] | None:
    """Read one framed message from a buffered reader (`sock.makefile('rb')`), or
    None on a clean EOF / truncation at a frame boundary."""
    head = reader.read(_HEADER.size)
    if len(head) < _HEADER.size:
        return None
    frame_type, length = _HEADER.unpack(head)
    if length > MAX_FRAME_BYTES:
        raise ValueError(f"sidecar frame length {length} exceeds the cap")
    payload = reader.read(length)
    if len(payload) < length:
        return None
    return frame_type, payload


def authenticate_to_host(
    sock: socket.socket, env: MutableMapping[str, str] | None = None
) -> None:
    """Consume and send the per-child capability before any worker traffic.

    The token is removed before the write, including on failure, so a child can
    never reconnect or retry with the same capability. The native listener must
    accept it as the exact first frame, compare it in constant time, and consume
    its expected token after that one connection attempt.
    """

    env = os.environ if env is None else env
    token = env.pop(WORKER_TOKEN_ENV, "")
    if not 32 <= len(token) <= 256 or not token.isascii():
        raise RuntimeError("the authenticated sidecar launch token is missing")
    write_frame(sock, FRAME_AUTH, token.encode("ascii"))


def _strict_json_object(payload: bytes) -> dict[str, object]:
    def pairs(values: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for name, value in values:
            if name in result:
                raise RenderProtocolError("render JSON contains a duplicate field")
            result[name] = value
        return result

    try:
        value = json.loads(payload.decode("utf-8"), object_pairs_hook=pairs)
    except RenderProtocolError:
        raise
    except (
        UnicodeDecodeError,
        json.JSONDecodeError,
        ValueError,
        RecursionError,
        OverflowError,
    ):
        raise RenderProtocolError("render JSON is invalid") from None
    if not isinstance(value, dict):
        raise RenderProtocolError("render JSON must be an object")
    return value


def _read_bounded_render_frame(
    reader: BinaryIO, limits: Mapping[int, int]
) -> tuple[int, bytes] | None:
    head = reader.read(_HEADER.size)
    if not head:
        return None
    if len(head) != _HEADER.size:
        raise RenderProtocolError("render frame header is truncated")
    frame_type, length = _HEADER.unpack(head)
    limit = limits.get(frame_type)
    if limit is None:
        raise RenderProtocolError(f"render frame type {frame_type} is out of order")
    if length > limit:
        raise RenderProtocolError(
            f"render frame type {frame_type} exceeds its {limit}-byte cap"
        )
    payload = reader.read(length)
    if len(payload) != length:
        raise RenderProtocolError("render frame payload is truncated")
    return frame_type, payload


def _validate_job_id(value: object) -> str:
    if not isinstance(value, str) or _RENDER_JOB_ID.fullmatch(value) is None:
        raise RenderProtocolError("render jobId is invalid")
    return value


def _validate_exact_int(value: object, *, name: str, minimum: int, maximum: int) -> int:
    if type(value) is not int or not minimum <= value <= maximum:
        raise RenderProtocolError(f"render {name} is invalid")
    return value


def render_frames_for_seconds(seconds: float) -> int:
    """Reference conversion for the native gateway's user-facing duration.

    The gateway performs this once, before sending an integer frame count:
    ``floor(seconds_f64 * 48000 + 0.5)``. This deliberately avoids Python's
    ties-to-even ``round`` behavior at half-frame boundaries.
    """

    if (
        isinstance(seconds, bool)
        or not isinstance(seconds, (int, float))
        or not math.isfinite(seconds)
        or not MIN_RENDER_SECONDS <= float(seconds) <= MAX_RENDER_SECONDS
    ):
        raise RenderProtocolError(
            f"render seconds must be {MIN_RENDER_SECONDS:g}-{MAX_RENDER_SECONDS:g}"
        )
    frames = math.floor(float(seconds) * RENDER_SAMPLE_RATE + 0.5)
    return _validate_exact_int(
        frames,
        name="frames",
        minimum=MIN_RENDER_FRAMES,
        maximum=MAX_RENDER_FRAMES,
    )


def read_render_command(reader: BinaryIO) -> RenderRequest | RenderCancel | None:
    """Read one strict host command, distinguishing clean EOF from truncation."""

    frame = _read_bounded_render_frame(
        reader,
        {
            FRAME_RENDER_REQUEST: MAX_RENDER_REQUEST_BYTES,
            FRAME_RENDER_CANCEL: MAX_RENDER_CONTROL_BYTES,
        },
    )
    if frame is None:
        return None
    frame_type, payload = frame
    value = _strict_json_object(payload)
    schema_version = value.get("schemaVersion")
    if type(schema_version) is not int or schema_version != RENDER_SCHEMA_VERSION:
        raise RenderProtocolError("render command schema is unsupported")
    job_id = _validate_job_id(value.get("jobId"))
    sequence = _validate_exact_int(
        value.get("sequence"), name="sequence", minimum=1, maximum=MAX_U64
    )
    if frame_type == FRAME_RENDER_CANCEL:
        if set(value) != {"schemaVersion", "jobId", "sequence"}:
            raise RenderProtocolError("render cancel contains unknown fields")
        return RenderCancel(job_id=job_id, sequence=sequence)

    if set(value) != {"schemaVersion", "jobId", "sequence", "prompt", "frames"}:
        raise RenderProtocolError("render request contains missing or unknown fields")
    prompt = value.get("prompt")
    if not isinstance(prompt, str) or not prompt.strip():
        raise RenderProtocolError("render prompt must be a non-empty string")
    prompt = prompt.strip()
    if len(prompt) > MAX_RENDER_PROMPT_CHARS:
        raise RenderProtocolError("render prompt exceeds its character cap")
    frames = _validate_exact_int(
        value.get("frames"),
        name="frames",
        minimum=MIN_RENDER_FRAMES,
        maximum=MAX_RENDER_FRAMES,
    )
    return RenderRequest(
        job_id=job_id,
        sequence=sequence,
        prompt=prompt,
        frames=frames,
    )


def _render_json(value: Mapping[str, object]) -> bytes:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    if len(payload) > MAX_RENDER_METADATA_BYTES:
        raise RenderProtocolError("render metadata exceeds its cap")
    return payload


def write_render_error(
    sock: socket.socket,
    *,
    job_id: str | None,
    sequence: int,
    code: str,
    message: str,
    send_frame: Callable[[int, bytes], None] | None = None,
) -> None:
    sequence = _validate_exact_int(
        sequence, name="sequence", minimum=0, maximum=MAX_U64
    )
    if job_id is not None:
        _validate_job_id(job_id)
    sender = (
        (lambda frame_type, payload: write_frame(sock, frame_type, payload))
        if send_frame is None
        else send_frame
    )
    sender(
        FRAME_RENDER_ERROR,
        _render_json(
            {
                "schemaVersion": RENDER_SCHEMA_VERSION,
                "jobId": job_id,
                "sequence": sequence,
                "code": code[:64],
                "message": message[:512],
            }
        ),
    )


def write_render_response(
    sock: socket,
    request: RenderRequest,
    pcm: bytes,
    *,
    send_frame: Callable[[int, bytes], None] | None = None,
    before_frame: Callable[[], None] | None = None,
) -> None:
    """Write one complete, size-checked f32le render response."""

    _validate_job_id(request.job_id)
    _validate_exact_int(request.sequence, name="sequence", minimum=1, maximum=MAX_U64)
    _validate_exact_int(
        request.frames,
        name="frames",
        minimum=MIN_RENDER_FRAMES,
        maximum=MAX_RENDER_FRAMES,
    )
    if not isinstance(pcm, bytes):
        raise RenderProtocolError("render engine returned a non-bytes payload")
    if len(pcm) != request.pcm_bytes or len(pcm) > MAX_RENDER_PCM_BYTES:
        raise RenderProtocolError(
            f"render engine returned {len(pcm)} PCM bytes; expected {request.pcm_bytes}"
        )
    sender = (
        (lambda frame_type, payload: write_frame(sock, frame_type, payload))
        if send_frame is None
        else send_frame
    )
    check = (lambda: None) if before_frame is None else before_frame
    check()
    sender(
        FRAME_RENDER_BEGIN,
        _render_json(
            {
                "schemaVersion": RENDER_SCHEMA_VERSION,
                "jobId": request.job_id,
                "sequence": request.sequence,
                "sampleRate": RENDER_SAMPLE_RATE,
                "channels": RENDER_CHANNELS,
                "sampleFormat": "f32le",
                "frames": request.frames,
                "pcmBytes": request.pcm_bytes,
            }
        ),
    )
    digest = hashlib.sha256()
    for start in range(0, len(pcm), RENDER_PCM_CHUNK_BYTES):
        check()
        chunk = pcm[start : start + RENDER_PCM_CHUNK_BYTES]
        digest.update(chunk)
        sender(FRAME_RENDER_CHUNK, chunk)
    check()
    sender(
        FRAME_RENDER_END,
        _render_json(
            {
                "schemaVersion": RENDER_SCHEMA_VERSION,
                "jobId": request.job_id,
                "sequence": request.sequence,
                "frames": request.frames,
                "pcmBytes": request.pcm_bytes,
                "sha256": digest.hexdigest(),
            }
        ),
    )


def _validate_render_error(value: Mapping[str, object], request: RenderRequest) -> str:
    if (
        set(value) != {"schemaVersion", "jobId", "sequence", "code", "message"}
        or type(value.get("schemaVersion")) is not int
        or value.get("schemaVersion") != RENDER_SCHEMA_VERSION
        or value.get("jobId") != request.job_id
        or type(value.get("sequence")) is not int
        or value.get("sequence") != request.sequence
        or type(value.get("code")) is not str
        or not 1 <= len(value["code"]) <= 64
        or type(value.get("message")) is not str
        or len(value["message"]) > 512
    ):
        raise RenderProtocolError("render error metadata is invalid")
    return value["code"]


def read_render_response(
    reader: BinaryIO, request: RenderRequest, *, require_eof: bool = False
) -> bytes:
    """Validate and assemble one response; the native host mirrors this parser."""

    _validate_job_id(request.job_id)
    _validate_exact_int(request.sequence, name="sequence", minimum=1, maximum=MAX_U64)
    _validate_exact_int(
        request.frames,
        name="frames",
        minimum=MIN_RENDER_FRAMES,
        maximum=MAX_RENDER_FRAMES,
    )
    first = _read_bounded_render_frame(
        reader,
        {
            FRAME_RENDER_BEGIN: MAX_RENDER_METADATA_BYTES,
            FRAME_RENDER_ERROR: MAX_RENDER_METADATA_BYTES,
        },
    )
    if first is None:
        raise RenderProtocolError("render response ended before begin")
    frame_type, payload = first
    value = _strict_json_object(payload)
    if frame_type == FRAME_RENDER_ERROR:
        code = _validate_render_error(value, request)
        raise RenderProtocolError(f"render worker returned {code}")
    expected_fields = {
        "schemaVersion",
        "jobId",
        "sequence",
        "sampleRate",
        "channels",
        "sampleFormat",
        "frames",
        "pcmBytes",
    }
    if (
        set(value) != expected_fields
        or type(value.get("schemaVersion")) is not int
        or value.get("schemaVersion") != RENDER_SCHEMA_VERSION
    ):
        raise RenderProtocolError("render begin metadata is invalid")
    if (
        value.get("jobId") != request.job_id
        or type(value.get("sequence")) is not int
        or value.get("sequence") != request.sequence
    ):
        raise RenderProtocolError("render response identity is out of turn")
    frames = value.get("frames")
    pcm_bytes = value.get("pcmBytes")
    if (
        type(value.get("sampleRate")) is not int
        or value.get("sampleRate") != RENDER_SAMPLE_RATE
        or type(value.get("channels")) is not int
        or value.get("channels") != RENDER_CHANNELS
        or value.get("sampleFormat") != "f32le"
        or type(frames) is not int
        or frames != request.frames
        or type(pcm_bytes) is not int
        or pcm_bytes != request.pcm_bytes
    ):
        raise RenderProtocolError("render begin audio identity is invalid")

    output = bytearray()
    digest = hashlib.sha256()
    while True:
        frame = _read_bounded_render_frame(
            reader,
            {
                FRAME_RENDER_CHUNK: RENDER_PCM_CHUNK_BYTES,
                FRAME_RENDER_END: MAX_RENDER_METADATA_BYTES,
                FRAME_RENDER_ERROR: MAX_RENDER_METADATA_BYTES,
            },
        )
        if frame is None:
            raise RenderProtocolError("render response is truncated")
        frame_type, payload = frame
        if frame_type == FRAME_RENDER_CHUNK:
            if not payload or len(payload) % RENDER_BYTES_PER_FRAME:
                raise RenderProtocolError("render PCM chunk is empty or misaligned")
            if len(output) + len(payload) > pcm_bytes:
                raise RenderProtocolError("render response contains extra PCM bytes")
            output.extend(payload)
            digest.update(payload)
            continue

        if frame_type == FRAME_RENDER_ERROR:
            code = _validate_render_error(_strict_json_object(payload), request)
            raise RenderProtocolError(f"render worker returned {code}")

        end = _strict_json_object(payload)
        if (
            set(end)
            != {
                "schemaVersion",
                "jobId",
                "sequence",
                "frames",
                "pcmBytes",
                "sha256",
            }
            or type(end.get("schemaVersion")) is not int
            or end.get("schemaVersion") != RENDER_SCHEMA_VERSION
            or end.get("jobId") != request.job_id
            or type(end.get("sequence")) is not int
            or end.get("sequence") != request.sequence
            or type(end.get("frames")) is not int
            or end.get("frames") != request.frames
            or type(end.get("pcmBytes")) is not int
            or end.get("pcmBytes") != pcm_bytes
            or type(end.get("sha256")) is not str
            or end.get("sha256") != digest.hexdigest()
            or len(output) != pcm_bytes
        ):
            raise RenderProtocolError(
                "render end metadata or exact byte total is invalid"
            )
        if require_eof and reader.read(1):
            raise RenderProtocolError("render response contains frames after end")
        return bytes(output)


class SocketOutQueue:
    """`run_deck_worker`'s `out_queue`, writing to the socket: ``('audio', bytes)``
    → a PCM frame, ``('status', dict)`` → a status frame."""

    def __init__(self, sock: socket.socket) -> None:
        self._sock = sock
        self._lock = threading.Lock()

    def put(self, item: tuple[str, object]) -> None:
        kind, payload = item
        with self._lock:
            if kind == "audio":
                write_frame(self._sock, FRAME_PCM, payload)  # type: ignore[arg-type]
            elif kind == "status":
                write_frame(
                    self._sock, FRAME_STATUS, json.dumps(payload).encode("utf-8")
                )


class SocketCmdQueue:
    """`run_deck_worker`'s `cmd_queue`, fed from the socket: a daemon thread parses
    CONTROL frames into an internal queue; `get` / `get_nowait` delegate to it (so
    the worker's blocking/throttle semantics are unchanged). A socket close enqueues
    a synthetic ``shutdown`` so the worker exits cleanly."""

    def __init__(self, reader) -> None:
        self._queue: queue.Queue = queue.Queue()
        self._reader = reader
        self._thread = threading.Thread(target=self._pump, daemon=True)
        self._thread.start()

    def _pump(self) -> None:
        while True:
            try:
                frame = read_frame(self._reader)
            except (OSError, ValueError):
                frame = None
            if frame is None:
                self._queue.put({"type": "shutdown"})
                return
            frame_type, payload = frame
            if frame_type == FRAME_EMBED:
                # Style-sample embed (M15): [u32 LE id length][id][PCM] → an
                # embed_sample command the worker handles like the WS path's.
                if len(payload) < 4:
                    continue
                id_len = int.from_bytes(payload[:4], "little")
                if id_len > MAX_EMBED_ID_BYTES or id_len > len(payload) - 4:
                    continue
                try:
                    sample_id = payload[4 : 4 + id_len].decode("utf-8")
                except UnicodeDecodeError:
                    continue
                if not sample_id:
                    continue
                pcm = bytes(payload[4 + id_len :])
                self._queue.put({"type": "embed_sample", "id": sample_id, "pcm": pcm})
                continue
            if frame_type != FRAME_CONTROL:
                continue  # ignore other frames (forward-compatible)
            try:
                command = json.loads(payload)
            except json.JSONDecodeError:
                continue
            if isinstance(command, dict) and "type" in command:
                self._queue.put(command)

    def get(self, timeout=None):
        return self._queue.get(timeout=timeout)

    def get_nowait(self):
        return self._queue.get_nowait()


class SharedSocketOutQueue:
    """Multiplex one worker process's per-deck output onto one socket."""

    def __init__(self, sock: socket.socket, deck: int, lock: threading.Lock) -> None:
        self._sock = sock
        self._deck = deck
        self._lock = lock

    def put(self, item: tuple[str, object]) -> None:
        kind, payload = item
        if kind == "audio":
            frame_type = FRAME_PCM
            encoded = payload
        elif kind == "status":
            frame_type = FRAME_STATUS
            encoded = json.dumps(payload).encode("utf-8")
        else:
            return
        with self._lock:
            write_frame(self._sock, frame_type, bytes([self._deck]) + encoded)


class SharedSocketCmdQueues:
    """Demultiplex deck-prefixed control/embed frames for a shared worker."""

    def __init__(self, reader, deck_count: int = 2) -> None:
        self.queues = [queue.Queue() for _ in range(deck_count)]
        self._reader = reader
        self._thread = threading.Thread(target=self._pump, daemon=True)
        self._thread.start()

    def _pump(self) -> None:
        while True:
            try:
                frame = read_frame(self._reader)
            except (OSError, ValueError):
                frame = None
            if frame is None:
                for target in self.queues:
                    target.put({"type": "shutdown"})
                return
            frame_type, payload = frame
            if not payload or payload[0] >= len(self.queues):
                continue
            target = self.queues[payload[0]]
            body = payload[1:]
            if frame_type == FRAME_EMBED:
                if len(body) < 4:
                    continue
                id_len = int.from_bytes(body[:4], "little")
                if id_len > MAX_EMBED_ID_BYTES or id_len > len(body) - 4:
                    continue
                try:
                    sample_id = body[4 : 4 + id_len].decode("utf-8")
                except UnicodeDecodeError:
                    continue
                if not sample_id:
                    continue
                target.put(
                    {
                        "type": "embed_sample",
                        "id": sample_id,
                        "pcm": bytes(body[4 + id_len :]),
                    }
                )
                continue
            if frame_type != FRAME_CONTROL:
                continue
            try:
                command = json.loads(body)
            except json.JSONDecodeError:
                continue
            if isinstance(command, dict) and "type" in command:
                target.put(command)


def run_sidecar(
    sock: socket.socket,
    deck_id: str,
    model: str,
    *,
    runtime: str = AUTO_RUNTIME,
    engine_factory=None,
) -> None:
    """Bridge `sock` to `run_deck_worker` for `deck_id` and run the generation loop
    until the socket closes. `engine_factory` is injectable for tests."""
    reader = sock.makefile("rb")
    cmd_queue = SocketCmdQueue(reader)
    out_queue = SocketOutQueue(sock)
    if engine_factory is None:

        def engine_factory(*, model):
            return create_engine(model=model, runtime=runtime)

    run_deck_worker(deck_id, model, cmd_queue, out_queue, engine_factory=engine_factory)


def run_shared_sidecar(
    sock: socket.socket,
    models: tuple[str, str],
    *,
    runtime: str,
    engine_factory=None,
) -> None:
    """Run both decks in one process, sharing a model when their pins match."""

    reader = sock.makefile("rb")
    commands = SharedSocketCmdQueues(reader)
    send_lock = threading.Lock()
    outputs = [SharedSocketOutQueue(sock, deck, send_lock) for deck in range(2)]
    for deck, model in enumerate(models):
        outputs[deck].put(
            (
                "status",
                {"event": "warming", "deck": "ab"[deck], "model": model},
            )
        )
    try:
        if engine_factory is not None:
            engines = [engine_factory(model=model) for model in models]
        elif models[0] == models[1]:
            primary = create_engine(model=models[0], runtime=runtime)
            shared_deck = getattr(primary, "shared_deck", None)
            if not callable(shared_deck):
                raise RuntimeError(
                    f"runtime {runtime!r} does not implement shared two-state topology"
                )
            engines = [primary, shared_deck()]
        else:
            # Preserve independent per-deck model selection.  Two different
            # models share one supervised process but necessarily load twice.
            engines = [create_engine(model=model, runtime=runtime) for model in models]

        # Warm each distinct loaded model once before either deck can report
        # readiness.  The shared clone marks itself as a non-owner/no-op warmup.
        for engine in engines:
            warm_up = getattr(engine, "warm_up", None)
            if callable(warm_up):
                warm_up()
            if hasattr(engine, "_warmup_owner"):
                engine._warmup_owner = False
    except Exception as error:
        for deck, model in enumerate(models):
            outputs[deck].put(
                (
                    "status",
                    {
                        "event": "startup_failed",
                        "deck": "ab"[deck],
                        "model": model,
                        "error": public_startup_error(error),
                    },
                )
            )
        return

    threads = []
    for deck, (model, engine) in enumerate(zip(models, engines, strict=True)):
        thread = threading.Thread(
            target=run_deck_worker,
            args=("ab"[deck], model, commands.queues[deck], outputs[deck]),
            kwargs={
                "engine_factory": lambda *, model, value=engine: value,
                "perform_warmup": False,
            },
            name=f"mrt2-deck-{'ab'[deck]}",
        )
        thread.start()
        threads.append(thread)
    for thread in threads:
        thread.join()


class _RenderCommandReader:
    """Continuously read the control half so cancellation can preempt rendering."""

    def __init__(self, reader: BinaryIO) -> None:
        # Socket backpressure bounds requests that arrive faster than the single
        # render slot can consume them; no unbounded JSON queue exists.
        self.commands: queue.Queue[_RenderCommand] = queue.Queue(maxsize=1)
        self._reader = reader
        self._thread = threading.Thread(
            target=self._pump, name="mrt2-render-control", daemon=True
        )
        self._thread.start()

    def _pump(self) -> None:
        while True:
            try:
                command = read_render_command(self._reader)
            except RenderProtocolError as error:
                message = str(error)
                self.commands.put(_RenderReaderFailure(message[:512]))
                return
            except Exception:  # noqa: BLE001 - never expose unexpected details
                message = "render control connection failed"
                self.commands.put(_RenderReaderFailure(message[:512]))
                return
            self.commands.put(command)
            if command is None:
                return


def _render_startup_error(error: Exception) -> str:
    # `public_startup_error` preserves deliberately bounded RuntimeUnavailable
    # diagnostics and collapses unknown exceptions to their class only.
    return public_startup_error(error)[:512]


def _shutdown_render_socket(sock: socket.socket) -> None:
    try:
        sock.shutdown(socket.SHUT_RDWR)
    except (AttributeError, OSError):
        pass


def _write_render_frame_bounded(
    sock: socket.socket,
    frame_type: int,
    payload: bytes,
    *,
    poll: Callable[[], None] | None = None,
    timeout: float = RENDER_WRITE_TIMEOUT_SECONDS,
) -> None:
    """Commit a frame without blocking the cancellation/control loop.

    ``sendall`` runs in a daemon because Python cannot portably make a socket's
    send side nonblocking without also disturbing the concurrent buffered read
    side. The caller remains live, polls control every 10 ms, and enforces one
    absolute write deadline. Closing the socket before process termination
    prevents a blocked writer from emitting late output in injected tests too.
    """

    result: queue.Queue[Exception | None] = queue.Queue(maxsize=1)

    def send() -> None:
        try:
            write_frame(sock, frame_type, payload)
        except Exception as error:  # noqa: BLE001 - sanitized below
            result.put(error)
        else:
            result.put(None)

    if poll is not None:
        poll()
    threading.Thread(
        target=send,
        name=f"mrt2-render-write-{frame_type}",
        daemon=True,
    ).start()
    deadline = time.monotonic() + timeout
    while True:
        if poll is not None:
            poll()
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            _shutdown_render_socket(sock)
            raise _RenderWriteError("render connection write timed out")
        try:
            error = result.get(timeout=min(RENDER_WRITE_POLL_SECONDS, remaining))
        except queue.Empty:
            continue
        if error is not None:
            raise _RenderWriteError("render connection write failed") from None
        return


def run_render_worker(
    sock: socket.socket,
    model: str,
    *,
    runtime: str = PYTORCH_CUDA_RUNTIME,
    engine_factory=None,
    terminate: Callable[[int], Any] = os._exit,
) -> None:
    """Serve serial, authenticated MRT2 clip renders over one bounded socket.

    Authentication is emitted by :func:`main` before this function runs. The
    loaded model remains warm across successful requests. A render call cannot
    be interrupted safely inside upstream PyTorch, so cancellation or EOF while
    it is active terminates this disposable process and releases its CUDA
    context; the native supervisor may start a fresh worker for the next job.

    The supervisor owns an outer startup/job deadline and must kill and reap the
    full child tree on cancellation, disconnect, deadline, or owner drop,
    including before ``render_ready``. A child has one connection and one
    consumed auth token; it never reconnects or retries a sequence.
    """

    try:
        engine = (
            create_engine(model=model, runtime=runtime)
            if engine_factory is None
            else engine_factory(model=model)
        )
        warm_up = getattr(engine, "warm_up", None)
        if callable(warm_up):
            warm_up()
    except Exception as error:
        try:
            write_render_error(
                sock,
                job_id=None,
                sequence=0,
                code="startup_failed",
                message=_render_startup_error(error),
                send_frame=lambda frame_type, payload: _write_render_frame_bounded(
                    sock, frame_type, payload, timeout=1.0
                ),
            )
        except _RenderWriteError:
            pass
        return

    try:
        _write_render_frame_bounded(
            sock,
            FRAME_STATUS,
            _render_json(
                {
                    "schemaVersion": RENDER_SCHEMA_VERSION,
                    "event": "render_ready",
                    "model": model,
                    "runtime": runtime,
                    "nextSequence": 1,
                }
            ),
        )
    except _RenderWriteError:
        return
    commands = _RenderCommandReader(sock.makefile("rb"))
    expected_sequence: int | None = 1

    def stop(code: int) -> None:
        _shutdown_render_socket(sock)
        terminate(code)
        raise _RenderWorkerStopped

    def send_error(
        *, job_id: str | None, sequence: int, code: str, message: str
    ) -> None:
        try:
            write_render_error(
                sock,
                job_id=job_id,
                sequence=sequence,
                code=code,
                message=message,
                send_frame=lambda frame_type, payload: _write_render_frame_bounded(
                    sock, frame_type, payload, timeout=1.0
                ),
            )
        except _RenderWriteError:
            pass

    try:
        while True:
            command = commands.commands.get()
            if command is None:
                return
            if isinstance(command, _RenderReaderFailure):
                send_error(
                    job_id=None,
                    sequence=expected_sequence or MAX_U64,
                    code="protocol_error",
                    message=command.message,
                )
                stop(2)
            if isinstance(command, RenderCancel):
                send_error(
                    job_id=command.job_id,
                    sequence=command.sequence,
                    code="no_active_job",
                    message="render job is not active",
                )
                stop(2)
            if expected_sequence is None or command.sequence != expected_sequence:
                send_error(
                    job_id=command.job_id,
                    sequence=command.sequence,
                    code="sequence_error",
                    message="render sequence is duplicate or out of order",
                )
                stop(2)
            expected_sequence = (
                None if command.sequence == MAX_U64 else command.sequence + 1
            )

            result: queue.Queue[tuple[bool, bytes | Exception]] = queue.Queue(maxsize=1)

            def render() -> None:
                try:
                    result.put(
                        (True, engine.render_clip(command.prompt, command.seconds))
                    )
                except Exception as error:  # noqa: BLE001 - boundary collapse
                    result.put((False, error))

            threading.Thread(
                target=render,
                name=f"mrt2-render-{command.job_id[:16]}",
                daemon=True,
            ).start()

            def poll_active(*, can_reply: bool = True) -> None:
                try:
                    pending = commands.commands.get_nowait()
                except queue.Empty:
                    return

                if pending is None:
                    stop(0)
                if (
                    isinstance(pending, RenderCancel)
                    and pending.job_id == command.job_id
                    and pending.sequence == command.sequence
                ):
                    if can_reply:
                        send_error(
                            job_id=command.job_id,
                            sequence=command.sequence,
                            code="cancelled",
                            message="render job was cancelled",
                        )
                    stop(2)
                message = (
                    pending.message
                    if isinstance(pending, _RenderReaderFailure)
                    else "render command is overlapping or out of turn"
                )
                if can_reply:
                    send_error(
                        job_id=command.job_id,
                        sequence=command.sequence,
                        code="protocol_error",
                        message=message,
                    )
                stop(2)

            # Control always gets first look. Once the result arrives, poll
            # again before BEGIN so a cancel already queued behind it wins.
            while True:
                poll_active()
                try:
                    succeeded, value = result.get(timeout=RENDER_WRITE_POLL_SECONDS)
                    break
                except queue.Empty:
                    continue
            poll_active()

            if not succeeded:
                send_error(
                    job_id=command.job_id,
                    sequence=command.sequence,
                    code="render_failed",
                    message="MRT2 render failed; the worker must be restarted",
                )
                return
            try:
                if not isinstance(value, bytes):
                    raise RenderProtocolError(
                        "render engine returned a non-bytes payload"
                    )
                write_render_response(
                    sock,
                    command,
                    value,
                    before_frame=poll_active,
                    send_frame=lambda frame_type, payload: _write_render_frame_bounded(
                        sock,
                        frame_type,
                        payload,
                        poll=lambda: poll_active(can_reply=False),
                    ),
                )
            except RenderProtocolError:
                send_error(
                    job_id=command.job_id,
                    sequence=command.sequence,
                    code="invalid_audio",
                    message="MRT2 render returned an invalid PCM payload",
                )
                return
            except _RenderWriteError:
                stop(2)
    except _RenderWorkerStopped:
        return


# --- Model tooling (the in-app model manager, issue #43) -------------------
#
# The Rust shell spawns this same binary to install Magenta assets without a
# terminal: `--init-resources` fetches the shared resources `mrt models init`
# pulls (musiccoca + spectrostream — a model cannot load without them), and
# `--download-model NAME` fetches an exported model. Both reuse the upstream
# `magenta_rt.cli.models_commands` code path verbatim (the HF repo, the file
# list, the source dispatch); the only addition is a machine-readable progress
# contract on stdout — one JSON object per line:
#
#   {"event": "stage", "stage": "init"|"download"}       # phase; UI keys the label
#   {"event": "file", "file": "<repo-relative path>"}    # a file started
#   {"event": "done"}                                     # success
#   {"event": "error", "message": "<cause>"}              # the reason, then exit 1
#
# The upstream code echoes human text and calls sys.exit(1) on failure; we route
# its click output into the progress contract and translate any exit/exception
# into an `error` line so the shell sees structured failure, not a dead pipe.

_ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def _emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def run_model_tooling(
    *, init_resources: bool = False, download_model: str | None = None
) -> None:
    """Install Magenta assets via the upstream `mrt models` code path, emitting
    the JSON progress contract. Raises SystemExit(1) on failure (after an
    `error` event that carries the tooling's own reason — `mrt` echoes the cause,
    e.g. an auth or network error, then exits). Resources are fetched first so a
    freshly downloaded model is actually loadable."""
    from magenta_rt import paths
    from magenta_rt.cli import models_commands as mc

    import click  # noqa: PLC0415 - deferred; only the tooling modes need it

    root = str(paths.magenta_home())
    source = mc._DEFAULT_SOURCE
    # The `models` group's commands, reached by name (a `checkpoints.download`
    # shadows the module-level `download`, so the group registry is the safe
    # handle).
    init_cmd = mc.models.commands["init"]
    download_cmd = mc.models.commands["download"]

    # The last human line the tooling printed — surfaced as the failure cause if
    # it exits non-zero. Stage labels carry the user-facing wording (keyed in the
    # frontend), so these messages are diagnostics, not localised UI.
    last_message: list[str] = []

    def echo(message: object = "", *_args, **_kwargs) -> None:
        text = _ANSI_RE.sub("", str(message)).strip()
        if not text:
            return
        if text.startswith("Downloading ") and text.endswith("…"):
            _emit(
                {
                    "event": "file",
                    "file": text[len("Downloading ") :].rstrip("… ").strip(),
                }
            )
        else:
            last_message.append(text)

    saved_echo = click.echo
    click.echo = echo
    try:
        if init_resources:
            _emit({"event": "stage", "stage": "init"})
            init_cmd.callback(download_path=root, source=source)
        if download_model:
            _emit({"event": "stage", "stage": "download"})
            download_cmd.callback(
                name=download_model, download_path=root, source=source
            )
    except SystemExit as exc:
        cause = last_message[-1] if last_message else "install failed"
        _emit({"event": "error", "message": f"{cause} (exit {exc.code})"})
        raise SystemExit(1) from exc
    except Exception as exc:  # noqa: BLE001 - any failure becomes a progress error
        _emit({"event": "error", "message": str(exc)})
        raise SystemExit(1) from exc
    finally:
        click.echo = saved_echo
    _emit({"event": "done"})


def main(argv=None) -> None:
    parser = argparse.ArgumentParser(description="LSDJ per-deck inference sidecar")
    # Deck-sidecar arguments. Not required, so the same binary can run the
    # model-tooling modes below (issue #43) without a deck/port.
    parser.add_argument("--deck", help="deck id (e.g. a or b)")
    parser.add_argument("--model", help="model name (e.g. mrt2_small)")
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument(
        "--shared", action="store_true", help="run both decks in one worker"
    )
    modes.add_argument(
        "--render-worker",
        action="store_true",
        help="run the dedicated authenticated MRT2 clip renderer",
    )
    parser.add_argument("--model-a", help="shared-worker model for deck a")
    parser.add_argument("--model-b", help="shared-worker model for deck b")
    parser.add_argument(
        "--runtime",
        choices=RUNTIME_CHOICES,
        default=AUTO_RUNTIME,
        help="explicit MRT2 implementation selected by the native host",
    )
    parser.add_argument(
        "--port",
        type=int,
        help="loopback TCP port the shell is listening on",
    )
    parser.add_argument(
        "--init-resources",
        action="store_true",
        help="fetch the shared model resources, emit JSON progress, then exit",
    )
    parser.add_argument(
        "--download-model",
        metavar="NAME",
        help="download an exported Magenta model, emit JSON progress, then exit",
    )
    parser.add_argument(
        "--runtime-info",
        action="store_true",
        help="emit immutable PyTorch runtime/install metadata, then exit",
    )
    args = parser.parse_args(argv)

    if args.runtime_info:
        _emit(runtime_manifest())
        return

    if args.init_resources or args.download_model:
        run_model_tooling(
            init_resources=args.init_resources,
            download_model=args.download_model,
        )
        return

    if args.render_worker:
        missing = [name for name in ("model", "port") if getattr(args, name) is None]
        if missing:
            parser.error(
                "the following arguments are required in render-worker mode: "
                + ", ".join("--" + name for name in missing)
            )
        if args.runtime != PYTORCH_CUDA_RUNTIME:
            parser.error(
                "render-worker mode requires the explicit pytorch-cuda runtime"
            )
        sock = socket.create_connection(("127.0.0.1", args.port))
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        authenticate_to_host(sock)
        run_render_worker(sock, args.model, runtime=args.runtime)
        return

    if args.shared:
        missing = [
            name
            for name in ("model_a", "model_b", "port")
            if getattr(args, name) is None
        ]
        if missing:
            parser.error(
                "the following arguments are required in shared mode: "
                + ", ".join("--" + name.replace("_", "-") for name in missing)
            )
        sock = socket.create_connection(("127.0.0.1", args.port))
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        authenticate_to_host(sock)
        run_shared_sidecar(
            sock,
            (args.model_a, args.model_b),
            runtime=args.runtime,
        )
        return

    missing = [
        name for name in ("deck", "model", "port") if getattr(args, name) is None
    ]
    if missing:
        parser.error(
            "the following arguments are required: "
            + ", ".join("--" + name for name in missing)
        )

    sock = socket.create_connection(("127.0.0.1", args.port))
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    authenticate_to_host(sock)
    run_sidecar(sock, args.deck, args.model, runtime=args.runtime)


if __name__ == "__main__":
    main()
