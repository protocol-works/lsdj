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

Shared-worker frames prefix payloads with a single deck byte (0 or 1). The Rust
and Python transport tests cover both forms without loading either model stack.

The transport (framing + the queue adapters) is testable against a socketpair
with a fake engine — no model, no Rust; see `tests/test_sidecar.py`. The
model-loaded round-trip is a native-checklist item.
"""

import argparse
import json
import os
import queue
import re
import socket
import struct
import sys
import threading

from .mrt2 import (
    AUTO_RUNTIME,
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

MAX_FRAME_BYTES = 16 * 1024 * 1024
MAX_EMBED_ID_BYTES = 4 * 1024
WORKER_TOKEN_ENV = "LSDJ_WORKER_LAUNCH_TOKEN"

# u8 frame type, u32 little-endian payload length.
_HEADER = struct.Struct("<BI")
_FRAME_TYPES = frozenset(
    {FRAME_PCM, FRAME_STATUS, FRAME_CONTROL, FRAME_EMBED, FRAME_AUTH}
)


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
    sock: socket.socket, env: dict[str, str] | None = None
) -> None:
    """Send the in-memory launch capability before any worker traffic."""

    env = os.environ if env is None else env
    token = env.get(WORKER_TOKEN_ENV, "")
    if not 32 <= len(token) <= 256 or not token.isascii():
        raise RuntimeError("the authenticated sidecar launch token is missing")
    write_frame(sock, FRAME_AUTH, token.encode("ascii"))


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
    parser.add_argument(
        "--shared", action="store_true", help="run both decks in one worker"
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
        os.environ.pop(WORKER_TOKEN_ENV, None)
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
    os.environ.pop(WORKER_TOKEN_ENV, None)
    run_sidecar(sock, args.deck, args.model, runtime=args.runtime)


if __name__ == "__main__":
    main()
