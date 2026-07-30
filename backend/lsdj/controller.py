"""FastAPI generation server: the Stable Audio 3 + Magenta render RPC.

The native shell (src-tauri/) hosts the realtime decks as Rust-spawned Python
sidecars and serves the UI from the Tauri asset host, so this server runs ONLY
the generation RPC: /api/render (the third Magenta engine), /api/generate
(Stable Audio 3), and /api/models. It never touches magenta_rt directly
(ADR-0002) — the render worker is a separate spawned process.
"""

import argparse
import asyncio
import contextlib
import io
import json
import logging
import math
import multiprocessing as mp
import os
import queue
import time
import wave

import uvicorn
from fastapi import FastAPI, HTTPException, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import Response
from starlette.datastructures import UploadFile
from starlette.exceptions import HTTPException as StarletteHTTPException

from . import engine, loras, sa3
from .worker import run_deck_worker

logger = logging.getLogger(__name__)

DEFAULT_MODEL = "mrt2_small"

# Rough whole-process footprints (model + MusicCoCa + MLX runtime), used only
# for the UI's "this combination looks tight" warning — not enforcement.
MODEL_RAM_ESTIMATE_GB = {"mrt2_small": 2.0, "mrt2_base": 6.0}


def _total_ram_gb() -> float:
    return os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES") / 1024**3


@contextlib.asynccontextmanager
async def _render_lifespan(_: FastAPI):
    """App lifespan: only the render worker has a lifecycle here.

    The decks moved to the Rust sidecars in the native cutover, so the
    controller spawns none — this just tears down the lazily-spawned
    render worker on shutdown.
    """
    yield
    if render_state["worker"] is not None:
        render_state["worker"].shutdown()
        render_state["worker"] = None


app = FastAPI(lifespan=_render_lifespan)


# Worst case: a 32 s clip at a pessimistic ~1× real time, plus a cold
# prompt embed; well past it the worker is wedged, not slow.
RENDER_TIMEOUT_SECONDS = 90
# First use pays the model load; this bounds it.
RENDER_READY_TIMEOUT_SECONDS = 180
# Magenta track ceiling (M19, ADR-0013): at the measured 1.86× real time
# (docs/spike-mrt2.md) a 3-minute render holds the single worker ~97 s —
# the boundary between a visible pending state and an outage.
RENDER_MAX_SECONDS = 180.0

# Multipart framing/headers are small but not zero. Content-Length is only an
# early rejection; `_read_init_audio` remains the authoritative file-size gate.
MAX_MULTIPART_BODY_BYTES = (
    sa3.MAX_INIT_AUDIO_BYTES + sa3.MAX_GENERATE_METADATA_BYTES + 128 * 1024
)


def render_timeout_for(seconds: float) -> float:
    """Deadline for one render, scaled to the requested length.

    Wall is ~0.54× the requested seconds at the measured 1.86× real time,
    so 2× seconds keeps ~3.7× slack; the flat pad deadline stays as the
    floor for short clips' cold-embed margin (ADR-0013)."""
    return max(RENDER_TIMEOUT_SECONDS, seconds * 2)


class RenderProcess:
    """The third Magenta engine (M18): a worker that only renders clips.

    Reuses the deck worker loop — a render worker is a deck worker that
    never receives `play` — but lives apart from the decks, so pads can
    fill while both streams run. Spawned lazily on the first request:
    a resident third model (~2 GB for mrt2_small) is only paid for by
    sessions that use it.
    """

    def __init__(self, model: str = DEFAULT_MODEL):
        self.model = model
        self.render_lock = asyncio.Lock()
        self._spawn()

    def _spawn(self) -> None:
        ctx = mp.get_context("spawn")
        self.cmd_queue = ctx.Queue()
        # Only the "ready" status ever lands here; renders answer on
        # clip_queue like a deck's.
        self.out_queue = ctx.Queue(maxsize=4)
        self.clip_queue = ctx.Queue()
        self.ready = False
        self.process = ctx.Process(
            target=run_deck_worker,
            args=("render", self.model, self.cmd_queue, self.out_queue),
            kwargs={"clip_queue": self.clip_queue},
            name="render-worker",
            daemon=True,
        )
        self.process.start()

    def await_ready(self) -> None:
        """Block until the worker reports the model loaded (first use)."""
        if self.ready:
            return
        kind, payload = self.out_queue.get(timeout=RENDER_READY_TIMEOUT_SECONDS)
        if kind != "status" or payload.get("event") != "ready":
            raise RuntimeError(f"render worker spoke out of turn: {payload!r}")
        self.ready = True

    def send(self, command: dict) -> None:
        self.cmd_queue.put(command)

    def shutdown(self) -> None:
        if self.process.is_alive():
            self.send({"type": "shutdown"})
            self.process.join(timeout=5)
            if self.process.is_alive():
                self.process.terminate()


# Created on the first /api/render call, never at startup.
render_state: dict = {"worker": None}


def ensure_render_worker() -> RenderProcess:
    worker = render_state["worker"]
    if worker is None or not worker.process.is_alive():
        worker = RenderProcess()
        render_state["worker"] = worker
    return worker


def discard_render_worker(worker: RenderProcess) -> None:
    """Kill a worker that missed its deadline. Past the timeout it is wedged,
    not slow (see RENDER_TIMEOUT_SECONDS) — and even a merely-slow one must
    die, or its late answer would land in the next request's queue. The next
    call respawns clean via ensure_render_worker."""
    if worker.process.is_alive():
        worker.process.terminate()
        worker.process.join(timeout=5)
    if render_state["worker"] is worker:
        render_state["worker"] = None


def float32_wav(pcm: bytes, sample_rate: int, channels: int) -> bytes:
    """Wrap wire-format PCM in a WAVE_FORMAT_IEEE_FLOAT header — what
    decodeAudioData expects, with no quantisation on the way."""
    byte_rate = sample_rate * channels * 4
    header = b"RIFF" + (36 + len(pcm)).to_bytes(4, "little") + b"WAVEfmt "
    header += (16).to_bytes(4, "little")
    header += (3).to_bytes(2, "little")  # IEEE float
    header += channels.to_bytes(2, "little")
    header += sample_rate.to_bytes(4, "little")
    header += byte_rate.to_bytes(4, "little")
    header += (channels * 4).to_bytes(2, "little")  # block align
    header += (32).to_bytes(2, "little")  # bits per sample
    header += b"data" + len(pcm).to_bytes(4, "little")
    return header + pcm


def _generation_number(
    parsed: dict, name: str, minimum: float, maximum: float
) -> float | None:
    if name not in parsed:
        return None
    value = parsed[name]
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
        or not minimum <= value <= maximum
    ):
        raise HTTPException(
            status_code=422,
            detail=f"'{name}' must be between {minimum:g} and {maximum:g}",
        )
    return float(value)


def _validate_init_wav(data: bytes) -> None:
    try:
        with wave.open(io.BytesIO(data), "rb") as source:
            channels = source.getnchannels()
            sample_width = source.getsampwidth()
            sample_rate = source.getframerate()
            frames = source.getnframes()
            compression = source.getcomptype()
            pcm_bytes = source.readframes(frames)
    except (EOFError, wave.Error):
        raise HTTPException(
            status_code=422, detail="'init_audio' must be a valid WAV file"
        ) from None
    if (
        compression != "NONE"
        or channels not in (1, 2)
        or sample_width != 2
        or sample_rate != 44_100
        or frames == 0
        or len(pcm_bytes) != frames * channels * sample_width
    ):
        raise HTTPException(
            status_code=422,
            detail=(
                "'init_audio' must be non-empty 44.1 kHz 16-bit PCM WAV "
                "with one or two channels"
            ),
        )


async def _read_init_audio(upload: UploadFile) -> bytes:
    chunks = []
    size = 0
    while chunk := await upload.read(64 * 1024):
        size += len(chunk)
        if size > sa3.MAX_INIT_AUDIO_BYTES:
            raise HTTPException(
                status_code=413,
                detail=(
                    f"'init_audio' must be at most {sa3.MAX_INIT_AUDIO_BYTES} bytes"
                ),
            )
        chunks.append(chunk)
    data = b"".join(chunks)
    _validate_init_wav(data)
    return data


async def _read_capped_body(request: Request, limit: int, detail: str) -> bytes:
    """Stream the request body, rejecting with 413 once it passes `limit`.

    A chunked request can omit or lie about Content-Length, so the ceiling is
    enforced while reading rather than trusting the header.
    """
    chunks = []
    size = 0
    async for chunk in request.stream():
        size += len(chunk)
        if size > limit:
            raise HTTPException(status_code=413, detail=detail)
        chunks.append(chunk)
    return b"".join(chunks)


async def _bounded_multipart_request(request: Request) -> Request:
    body = await _read_capped_body(
        request, MAX_MULTIPART_BODY_BYTES, "multipart body is too large"
    )
    sent = False

    async def receive() -> dict:
        nonlocal sent
        if sent:
            return {"type": "http.request", "body": b"", "more_body": False}
        sent = True
        return {"type": "http.request", "body": body, "more_body": False}

    return Request(request.scope, receive)


async def _parse_generate_body(request: Request) -> tuple[dict, bytes | None]:
    content_type = request.headers.get("content-type", "")
    media_type = content_type.partition(";")[0].strip().lower()
    if media_type == "application/json":
        body = await _read_capped_body(
            request, sa3.MAX_GENERATE_METADATA_BYTES, "request body is too large"
        )
        try:
            parsed = json.loads(body)
        except (json.JSONDecodeError, UnicodeDecodeError):
            raise HTTPException(status_code=422, detail="body must be JSON") from None
        return parsed, None
    if media_type != "multipart/form-data":
        raise HTTPException(
            status_code=422,
            detail="content type must be application/json or multipart/form-data",
        )

    content_length = request.headers.get("content-length")
    if content_length is not None:
        try:
            declared_size = int(content_length)
        except ValueError:
            raise HTTPException(
                status_code=422, detail="invalid Content-Length"
            ) from None
        if declared_size < 0:
            raise HTTPException(status_code=422, detail="invalid Content-Length")
        if declared_size > MAX_MULTIPART_BODY_BYTES:
            raise HTTPException(status_code=413, detail="multipart body is too large")

    multipart_request = await _bounded_multipart_request(request)
    try:
        # `max_part_size` guards only non-file parts and Starlette maps a breach to
        # a generic 422. Leave it at the whole-body bound (already the memory
        # ceiling) so the explicit, charset-correct metadata check below is the
        # authoritative gate and returns a consistent 413 for oversized metadata.
        async with multipart_request.form(
            max_files=1,
            max_fields=1,
            max_part_size=MAX_MULTIPART_BODY_BYTES,
        ) as form:
            items = form.multi_items()
            request_fields = [value for name, value in items if name == "request"]
            audio_fields = [value for name, value in items if name == "init_audio"]
            if (
                len(items) != 2
                or len(request_fields) != 1
                or len(audio_fields) != 1
                or not isinstance(request_fields[0], str)
                or not isinstance(audio_fields[0], UploadFile)
            ):
                raise HTTPException(
                    status_code=422,
                    detail=(
                        "multipart body must contain one 'request' JSON field "
                        "and one 'init_audio' WAV file"
                    ),
                )
            metadata_text = request_fields[0]
            if len(metadata_text.encode("utf-8")) > sa3.MAX_GENERATE_METADATA_BYTES:
                raise HTTPException(
                    status_code=413, detail="'request' metadata is too large"
                )
            try:
                parsed = json.loads(metadata_text)
            except json.JSONDecodeError:
                raise HTTPException(
                    status_code=422, detail="'request' must be JSON"
                ) from None
            init_audio = await _read_init_audio(audio_fields[0])
    except StarletteHTTPException as error:
        if error.status_code != 400:
            raise
        raise HTTPException(status_code=422, detail=str(error.detail)) from None
    return parsed, init_audio


def _validate_generate_request(
    parsed: object, init_audio: bytes | None
) -> tuple[str, float, str, dict]:
    if not isinstance(parsed, dict):
        raise HTTPException(status_code=422, detail="body must be a JSON object")
    prompt = parsed.get("prompt")
    if not (isinstance(prompt, str) and prompt.strip()):
        raise HTTPException(
            status_code=422, detail="'prompt' must be a non-empty string"
        )
    prompt = prompt.strip()
    if len(prompt) > sa3.MAX_PROMPT_LENGTH:
        raise HTTPException(
            status_code=422,
            detail=f"'prompt' must be at most {sa3.MAX_PROMPT_LENGTH} characters",
        )
    kind = parsed.get("kind")
    if not isinstance(kind, str) or kind not in sa3.KINDS:
        raise HTTPException(
            status_code=422, detail=f"'kind' must be one of {sorted(sa3.KINDS)}"
        )
    max_seconds = sa3.MAX_SECONDS_FOR[kind]
    seconds = parsed.get("seconds")
    if (
        isinstance(seconds, bool)
        or not isinstance(seconds, (int, float))
        or not math.isfinite(seconds)
        or not sa3.MIN_SECONDS <= seconds <= max_seconds
    ):
        raise HTTPException(
            status_code=422,
            detail=f"'seconds' must be {sa3.MIN_SECONDS}-{max_seconds:g}",
        )

    options = {}
    init_noise_level = _generation_number(
        parsed,
        "init_noise_level",
        sa3.MIN_INIT_NOISE_LEVEL,
        sa3.MAX_INIT_NOISE_LEVEL,
    )
    cfg = _generation_number(parsed, "cfg", sa3.MIN_CFG, sa3.MAX_CFG)
    apg = _generation_number(parsed, "apg", sa3.MIN_APG, sa3.MAX_APG)
    if init_noise_level is not None:
        options["init_noise_level"] = init_noise_level
    if cfg is not None:
        options["cfg"] = cfg
    if apg is not None:
        if cfg is None or cfg == 1.0:
            raise HTTPException(
                status_code=422, detail="'apg' requires 'cfg' other than 1"
            )
        options["apg"] = apg

    if "negative_prompt" in parsed:
        negative_prompt = parsed["negative_prompt"]
        if not isinstance(negative_prompt, str):
            raise HTTPException(
                status_code=422, detail="'negative_prompt' must be a string"
            )
        negative_prompt = negative_prompt.strip()
        if negative_prompt:
            if len(negative_prompt) > sa3.MAX_PROMPT_LENGTH:
                raise HTTPException(
                    status_code=422,
                    detail=(
                        "'negative_prompt' must be at most "
                        f"{sa3.MAX_PROMPT_LENGTH} characters"
                    ),
                )
            if cfg is None or cfg == 1.0:
                raise HTTPException(
                    status_code=422,
                    detail="'negative_prompt' requires 'cfg' other than 1",
                )
            options["negative_prompt"] = negative_prompt

    if "seed" in parsed:
        seed = parsed["seed"]
        if (
            isinstance(seed, bool)
            or not isinstance(seed, int)
            or not 0 <= seed <= sa3.MAX_SEED
        ):
            raise HTTPException(
                status_code=422,
                detail=f"'seed' must be an integer from 0-{sa3.MAX_SEED}",
            )
        options["seed"] = seed

    if "loras" in parsed:
        stack = parsed["loras"]
        if not isinstance(stack, list):
            raise HTTPException(status_code=422, detail="'loras' must be an array")
        if len(stack) > loras.MAX_LORA_STACK:
            raise HTTPException(
                status_code=422,
                detail=f"'loras' holds at most {loras.MAX_LORA_STACK} adapters",
            )
        lora_dirs: list[str] = []
        lora_strengths: list[float] = []
        seen: set[str] = set()
        for entry in stack:
            if not isinstance(entry, dict):
                raise HTTPException(
                    status_code=422, detail="'loras' entries must be objects"
                )
            name = entry.get("name")
            if not isinstance(name, str):
                raise HTTPException(
                    status_code=422, detail="'loras[].name' must be a string"
                )
            if name in seen:
                # A repeated adapter would double its delta silently; refuse.
                raise HTTPException(
                    status_code=422, detail=f"duplicate adapter {name!r}"
                )
            seen.add(name)
            try:
                adapter_dir, base = loras.resolve(name)
            except loras.UnknownAdapter as error:
                raise HTTPException(status_code=422, detail=str(error)) from None
            if loras.KIND_BASES[kind] != base:
                raise HTTPException(
                    status_code=422,
                    detail=(
                        f"adapter '{name}' rides the {base} DiT and cannot apply "
                        f"to kind '{kind}'"
                    ),
                )
            strength = entry.get("strength", 1.0)
            if (
                isinstance(strength, bool)
                or not isinstance(strength, (int, float))
                or not math.isfinite(strength)
                or not loras.MIN_LORA_STRENGTH <= strength <= loras.MAX_LORA_STRENGTH
            ):
                raise HTTPException(
                    status_code=422,
                    detail=(
                        "'loras[].strength' must be "
                        f"{loras.MIN_LORA_STRENGTH:g}-{loras.MAX_LORA_STRENGTH:g}"
                    ),
                )
            lora_dirs.append(str(adapter_dir))
            lora_strengths.append(float(strength))
        if lora_dirs:
            options["lora_dirs"] = lora_dirs
            options["lora_strengths"] = lora_strengths

    if "inpaint_range" in parsed:
        inpaint_range = parsed["inpaint_range"]
        if not isinstance(inpaint_range, list) or len(inpaint_range) != 2:
            raise HTTPException(
                status_code=422,
                detail="'inpaint_range' must be a two-number array",
            )
        start, end = inpaint_range
        if (
            isinstance(start, bool)
            or isinstance(end, bool)
            or not isinstance(start, (int, float))
            or not isinstance(end, (int, float))
            or not math.isfinite(start)
            or not math.isfinite(end)
            or not 0 <= start < end <= seconds
        ):
            raise HTTPException(
                status_code=422,
                detail=("'inpaint_range' must satisfy 0 <= start < end <= seconds"),
            )
        if init_audio is None:
            raise HTTPException(
                status_code=422, detail="'inpaint_range' requires 'init_audio'"
            )
        options["inpaint_range"] = (float(start), float(end))

    if init_audio is not None:
        options["init_audio"] = init_audio
    return prompt, float(seconds), kind, options


@app.post("/api/render")
async def render_clip(request: Request) -> Response:
    """Render a pad clip with the third Magenta engine (M18).

    Body: JSON {prompt, seconds}. The render worker spawns on the
    first call (the model load happens inside that request's pending
    state) and stays warm after; both decks keep streaming untouched.
    Returns the clip as a float32 WAV.
    """
    try:
        parsed = await request.json()
    except json.JSONDecodeError:
        raise HTTPException(status_code=422, detail="body must be JSON") from None
    if not isinstance(parsed, dict):
        raise HTTPException(status_code=422, detail="body must be a JSON object")
    prompt = parsed.get("prompt")
    if not (isinstance(prompt, str) and prompt.strip()):
        raise HTTPException(
            status_code=422, detail="'prompt' must be a non-empty string"
        )
    prompt = prompt.strip()
    if len(prompt) > sa3.MAX_PROMPT_LENGTH:
        raise HTTPException(
            status_code=422,
            detail=f"'prompt' must be at most {sa3.MAX_PROMPT_LENGTH} characters",
        )
    seconds = parsed.get("seconds")
    if (
        isinstance(seconds, bool)
        or not isinstance(seconds, (int, float))
        or not math.isfinite(seconds)
        or not sa3.MIN_SECONDS <= seconds <= RENDER_MAX_SECONDS
    ):
        raise HTTPException(
            status_code=422,
            detail=f"'seconds' must be {sa3.MIN_SECONDS}-{RENDER_MAX_SECONDS:g}",
        )
    worker = ensure_render_worker()
    async with worker.render_lock:
        # A request that queued on the lock may hold a worker another
        # request just killed; fail fast rather than burn the timeout
        # against the corpse.
        if not worker.process.is_alive():
            discard_render_worker(worker)
            raise HTTPException(status_code=502, detail="render engine died")
        try:
            await asyncio.to_thread(worker.await_ready)
        except (queue.Empty, RuntimeError):
            discard_render_worker(worker)
            raise HTTPException(
                status_code=502, detail="render engine failed to start"
            ) from None
        # A previous timed-out render may have answered late; whatever
        # sits in the queue belongs to nobody now.
        with contextlib.suppress(queue.Empty):
            while True:
                worker.clip_queue.get_nowait()
        request_id = f"clip-{time.monotonic_ns()}"
        worker.send(
            {
                "type": "render_clip",
                "id": request_id,
                "prompt": prompt,
                "seconds": float(seconds),
            }
        )
        try:
            result_id, result = await asyncio.to_thread(
                worker.clip_queue.get, True, render_timeout_for(float(seconds))
            )
        except queue.Empty:
            discard_render_worker(worker)
            raise HTTPException(status_code=502, detail="render timed out") from None
    if result_id != request_id:
        raise HTTPException(status_code=502, detail="render answered out of turn")
    if "error" in result:
        raise HTTPException(status_code=502, detail=result["error"])
    return Response(
        content=float32_wav(result["pcm"], engine.SAMPLE_RATE, engine.CHANNELS),
        media_type="audio/wav",
    )


@app.post("/api/generate")
async def generate_audio(request: Request) -> Response:
    """Generate a pad clip with Stable Audio 3 (M18, ADR-0012).

    Body: JSON generation metadata, or multipart with that JSON in `request`
    plus an `init_audio` WAV. Returns the WAV. Generation runs in a spawned
    subprocess and is serialised, so a busy moment queues rather than stacking
    memory.
    """
    parsed, init_audio = await _parse_generate_body(request)
    prompt, seconds, kind, options = _validate_generate_request(parsed, init_audio)
    try:
        wav = await sa3.generate(prompt, seconds, kind, **options)
    except sa3.GenerationUnavailable as error:
        raise HTTPException(status_code=503, detail=str(error)) from None
    except sa3.GenerationFailed as error:
        logger.warning("generation failed: %s", error)
        raise HTTPException(status_code=502, detail=str(error)) from None
    return Response(content=wav, media_type="audio/wav")


@app.get("/api/models")
def list_models() -> dict:
    """The downloaded models + RAM info for the deck UI's model picker and the
    "this combination looks tight" warning. In the native shell the realtime decks
    live in the Rust sidecars, so the webview fetches this from the generation
    server instead."""
    return {
        "models": engine.available_models(),
        "sample_rate": engine.SAMPLE_RATE,
        "channels": engine.CHANNELS,
        "chunk_seconds": engine.CHUNK_SECONDS,
        "total_ram_gb": round(_total_ram_gb(), 1),
        "model_ram_estimate_gb": MODEL_RAM_ESTIMATE_GB,
    }


# The browser cue sink (ADR-0007, `/ws/cue` + `/api/cue/outputs`) was retired at
# the native cutover (Phase 2 part 7): the native shell routes the cue to the FLX4
# phones (channels 3/4) inside the Rust engine (Slice 5), so no backend
# `sounddevice` sink is needed. ADR-0007 is superseded by ADR-0019.


def main() -> None:
    logging.basicConfig(level=logging.INFO)
    parser = argparse.ArgumentParser(description="LSDJai generation server")
    parser.add_argument(
        "--port", type=int, default=8000, help="loopback port to bind (default 8000)"
    )
    args = parser.parse_args()

    # The webview loads from the Tauri asset host and fetches this server
    # cross-origin over loopback, so allow it. Loopback-bound; not exposed.
    app.add_middleware(
        CORSMiddleware,
        allow_origins=["*"],
        allow_methods=["*"],
        allow_headers=["*"],
    )
    uvicorn.run(app, host="127.0.0.1", port=args.port)


if __name__ == "__main__":
    main()
