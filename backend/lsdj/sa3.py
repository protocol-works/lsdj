"""Runtime-neutral Stable Audio 3 service.

LSDJ spawns an official, pinned upstream CLI for each generation.  Apple
Silicon uses MLX; Linux and Windows use LiteRT/TFLite.  Both adapters receive
the same request object, execute strictly offline against app-installed assets,
and return a validated canonical WAV.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import pathlib
import platform as host_platform
import re
import signal
import subprocess
import sys
import tempfile
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass

from . import runtime_paths, sa3_cuda
from .sa3_audio import AudioFormatError, inspect_canonical_wav, normalize_wav
from .sa3_audio import validate_output_wav as _validate_output_wav
from .sa3_contract import (
    BackendName,
    GenerationRequest,
    ProgressEvent,
    capabilities_for,
)

# Both pinned official CLIs deliberately share these names.
KINDS = {"sfx": "sm-sfx", "music": "sm-music", "track": "medium"}
DECODERS = {"sfx": "same-s", "music": "same-s", "track": "same-l"}
SAMPLER_STEPS = 8
MIN_STEPS = 1
MAX_STEPS = 100

MIN_SECONDS = 0.5
MAX_SECONDS = 32.0
TRACK_MAX_SECONDS = 380.0
MAX_SECONDS_FOR = {"sfx": MAX_SECONDS, "music": MAX_SECONDS, "track": TRACK_MAX_SECONDS}
MAX_PROMPT_LENGTH = 32_000

MIN_INIT_NOISE_LEVEL = 0.01
MAX_INIT_NOISE_LEVEL = 5.0
MIN_CFG = -20.0
MAX_CFG = 20.0
MIN_APG = 0.0
MAX_APG = 1.0
MAX_SEED = 2**31 - 1
MAX_INIT_AUDIO_BYTES = 16 * 1024 * 1024
MAX_GENERATE_METADATA_BYTES = 64 * 1024

TIMEOUT_SECONDS = 120
TFLITE_THREADS_DEFAULT = 4
TFLITE_THREADS_MAX = 8
SA3_PREFERENCE_ENV = "LSDJ_SA3_PREFERENCE"

STATE_MISSING = "missing"
STATE_VENV_MISSING = "venv_missing"
STATE_NOT_WARMED = "not_warmed"
STATE_READY = "ready"
STATE_UNSUPPORTED = "unsupported"
STATE_FAILED = "failed"

WARMED_STAMP = ".lsdj-warmed"
TFLITE_PROVENANCE_STAMP = ".lsdj-provenance.json"
TFLITE_RUNTIME_REPO = "https://github.com/Stability-AI/stable-audio-3"
TFLITE_RUNTIME_REVISION = "a0b57f5483c4588f827f3552b7d5c6ca2a9687be"
TFLITE_MODELS_REPO = "stabilityai/stable-audio-3-optimized"
TFLITE_MODELS_REVISION = "6736003cb57d06b7b1fdc36fad31b2a3709e4774"

_MLX_SUBDIR = pathlib.Path("optimized/mlx")
_TFLITE_SUBDIR = pathlib.Path("optimized/tflite")
_MLX_SCRIPT = pathlib.Path("scripts/sa3_mlx.py")
_TFLITE_SCRIPT = pathlib.Path("scripts/sa3_tflite.py")

# fp32 is the official TFLite default and the only precision for which upstream
# supports LoRA.  The secure installer consumes sa3-tflite-pin.json and places
# these files before the worker is ever started.
_TFLITE_SHARED_ASSETS = (
    pathlib.Path("models/tokenizer.model"),
    pathlib.Path("models/tflite/t5gemma/encoder_fp16.tflite"),
)
_TFLITE_MODEL_ASSETS = {
    "sfx": (
        pathlib.Path("models/tflite/sa3-sm-sfx/dit_fp32.tflite"),
        pathlib.Path("models/tflite/same-s/dec_fp32.tflite"),
    ),
    "music": (
        pathlib.Path("models/tflite/sa3-sm-music/dit_fp32.tflite"),
        pathlib.Path("models/tflite/same-s/dec_fp32.tflite"),
    ),
    "track": (
        pathlib.Path("models/tflite/sa3-m/dit_fp32.tflite"),
        pathlib.Path("models/tflite/same-l/dec_fp32.tflite"),
    ),
}
_TFLITE_ENCODER_ASSET = {
    "sfx": pathlib.Path("models/tflite/same-s/enc_fp32.tflite"),
    "music": pathlib.Path("models/tflite/same-s/enc_fp32.tflite"),
    "track": pathlib.Path("models/tflite/same-l/enc_fp32.tflite"),
}


class GenerationUnavailable(Exception):
    """No supported, ready Stable Audio runtime exists on this machine."""


class GenerationFailed(Exception):
    """The selected runtime failed or produced invalid audio."""


class GenerationCancelled(Exception):
    """The caller cancelled a generation and the worker was stopped."""


class UnsupportedCapability(GenerationUnavailable):
    """A request names a capability the selected backend cannot honour."""


@dataclass(frozen=True)
class RuntimeSelection:
    backend: BackendName
    checkout: pathlib.Path
    runtime_dir: pathlib.Path
    executable: pathlib.Path
    script: pathlib.Path


def timeout_for(seconds: float) -> float:
    """Wedge deadline, not a performance promise."""
    return TIMEOUT_SECONDS + seconds


def _normalise_arch(machine: str) -> str:
    value = machine.strip().lower()
    if value in {"arm64", "aarch64"}:
        return "arm64"
    if value in {"amd64", "x86_64"}:
        return "x86_64"
    return value


def select_backend(
    env: Mapping[str, str] | None = None,
    *,
    platform_name: str | None = None,
    machine: str | None = None,
) -> BackendName:
    """Select the backend deterministically; never fall back silently."""
    env = os.environ if env is None else env
    platform_name = sys.platform if platform_name is None else platform_name
    machine = host_platform.machine() if machine is None else machine
    arch = _normalise_arch(machine)
    override = env.get("LSDJ_SA3_BACKEND", "").strip().lower()
    if override:
        try:
            chosen = BackendName(override)
        except ValueError:
            raise GenerationUnavailable(
                "LSDJ_SA3_BACKEND must be 'mlx' or 'tflite'"
            ) from None
        if chosen is BackendName.MLX and not (
            platform_name == "darwin" and arch == "arm64"
        ):
            raise GenerationUnavailable(
                "the MLX Stable Audio backend requires Apple Silicon macOS"
            )
        if chosen is BackendName.TFLITE and not (
            platform_name in {"linux", "win32"} and arch == "x86_64"
        ):
            raise GenerationUnavailable(
                f"the TFLite Stable Audio backend does not support {platform_name}/{arch}"
            )
        return chosen
    if platform_name == "darwin" and arch == "arm64":
        return BackendName.MLX
    if platform_name in {"linux", "win32"} and arch == "x86_64":
        return BackendName.TFLITE
    raise GenerationUnavailable(
        f"no Stable Audio backend supports {platform_name}/{arch}"
    )


def _checkout_candidates(env: Mapping[str, str]) -> list[pathlib.Path]:
    checkout = runtime_paths.sa3_home(env)
    return [] if checkout is None else [checkout]


def _layout(backend: BackendName) -> tuple[pathlib.Path, pathlib.Path]:
    if backend is BackendName.MLX:
        return _MLX_SUBDIR, _MLX_SCRIPT
    return _TFLITE_SUBDIR, _TFLITE_SCRIPT


def _tflite_provenance_error(runtime_dir: pathlib.Path) -> str | None:
    stamp = runtime_dir / TFLITE_PROVENANCE_STAMP
    try:
        parsed = json.loads(stamp.read_text())
    except (OSError, json.JSONDecodeError):
        return "the verified TFLite provenance stamp is missing or unreadable"
    expected = {
        "runtime": {
            "repo": TFLITE_RUNTIME_REPO,
            "revision": TFLITE_RUNTIME_REVISION,
        },
        "models": {
            "repo": TFLITE_MODELS_REPO,
            "revision": TFLITE_MODELS_REVISION,
        },
    }
    if parsed != expected:
        return "the installed TFLite runtime/model revisions do not match LSDJ's pin"
    return None


def resolve_runtime(
    env: Mapping[str, str] | None = None,
    *,
    platform_name: str | None = None,
    machine: str | None = None,
) -> RuntimeSelection | None:
    env = os.environ if env is None else env
    backend = select_backend(env, platform_name=platform_name, machine=machine)
    subdir, script_rel = _layout(backend)
    for checkout in _checkout_candidates(env):
        runtime_dir = checkout / subdir
        executable = runtime_paths.venv_python(
            runtime_dir / ".venv", platform=platform_name
        )
        script = runtime_dir / script_rel
        if executable.is_file() and script.is_file():
            return RuntimeSelection(
                backend=backend,
                checkout=checkout,
                runtime_dir=runtime_dir,
                executable=executable,
                script=script,
            )
    return None


def resolve_mlx_dir(
    env: Mapping[str, str] | None = None, home: pathlib.Path | None = None
) -> pathlib.Path | None:
    """Compatibility probe used by existing model-manager tests."""
    del home
    env = os.environ if env is None else env
    for checkout in _checkout_candidates(env):
        runtime_dir = checkout / _MLX_SUBDIR
        executable = runtime_paths.venv_python(runtime_dir / ".venv", platform="darwin")
        if executable.is_file() and (runtime_dir / _MLX_SCRIPT).is_file():
            return runtime_dir
    return None


def resolve_tflite_dir(
    env: Mapping[str, str] | None = None,
    *,
    platform_name: str | None = None,
) -> pathlib.Path | None:
    env = os.environ if env is None else env
    for checkout in _checkout_candidates(env):
        runtime_dir = checkout / _TFLITE_SUBDIR
        executable = runtime_paths.venv_python(
            runtime_dir / ".venv", platform=platform_name
        )
        if executable.is_file() and (runtime_dir / _TFLITE_SCRIPT).is_file():
            return runtime_dir
    return None


def readiness(
    env: Mapping[str, str] | None = None,
    home: pathlib.Path | None = None,
    *,
    platform_name: str | None = None,
    machine: str | None = None,
) -> dict:
    del home
    env = os.environ if env is None else env
    try:
        backend = select_backend(env, platform_name=platform_name, machine=machine)
    except GenerationUnavailable as error:
        return {
            "state": STATE_UNSUPPORTED,
            "backend": None,
            "checkout": None,
            "runtime_dir": None,
            "mlx_dir": None,
            "detail": str(error),
        }
    subdir, script_rel = _layout(backend)
    first_runtime: tuple[pathlib.Path, pathlib.Path] | None = None
    for checkout in _checkout_candidates(env):
        runtime_dir = checkout / subdir
        if not runtime_dir.is_dir():
            continue
        if first_runtime is None:
            first_runtime = (checkout, runtime_dir)
        executable = runtime_paths.venv_python(
            runtime_dir / ".venv", platform=platform_name
        )
        if not (executable.is_file() and (runtime_dir / script_rel).is_file()):
            continue
        warmed = (runtime_dir / WARMED_STAMP).is_file()
        provenance_error = (
            _tflite_provenance_error(runtime_dir)
            if backend is BackendName.TFLITE
            else None
        )
        state = (
            STATE_FAILED
            if warmed and provenance_error is not None
            else STATE_READY
            if warmed
            else STATE_NOT_WARMED
        )
        return {
            "state": state,
            "backend": backend.value,
            "checkout": str(checkout),
            "runtime_dir": str(runtime_dir),
            "mlx_dir": str(runtime_dir) if backend is BackendName.MLX else None,
            "detail": provenance_error if state == STATE_FAILED else None,
        }
    if first_runtime is not None:
        checkout, runtime_dir = first_runtime
        return {
            "state": STATE_VENV_MISSING,
            "backend": backend.value,
            "checkout": str(checkout),
            "runtime_dir": str(runtime_dir),
            "mlx_dir": str(runtime_dir) if backend is BackendName.MLX else None,
            "detail": None,
        }
    return {
        "state": STATE_MISSING,
        "backend": backend.value,
        "checkout": None,
        "runtime_dir": None,
        "mlx_dir": None,
        "detail": None,
    }


_generation_state: dict = {
    "state": "idle",
    "backend": None,
    "mode": None,
    "progress": None,
}


def status(
    env: Mapping[str, str] | None = None,
    *,
    platform_name: str | None = None,
    machine: str | None = None,
) -> dict:
    env = os.environ if env is None else env
    ready = readiness(env, platform_name=platform_name, machine=machine)
    backend_value = ready["backend"]
    capabilities = (
        None
        if backend_value is None
        else capabilities_for(BackendName(backend_value)).as_dict()
    )
    platform_value = sys.platform if platform_name is None else platform_name
    machine_value = host_platform.machine() if machine is None else machine
    preference = env.get(SA3_PREFERENCE_ENV, sa3_cuda.BackendPreference.AUTO.value)
    cuda = None
    if platform_value == "win32" and _normalise_arch(machine_value) == "x86_64":
        evidence = sa3_cuda.CudaEvidence(
            platform=platform_value,
            machine=machine_value,
            runtime_ready=False,
            provenance_complete=False,
            packages={},
            cuda_available=False,
            cuda_runtime=None,
            driver=None,
            device=None,
            compute_capability=None,
            total_vram_bytes=None,
            free_vram_bytes=None,
            estimated_vram_bytes={"music": None, "sfx": None, "track": None},
        )
        cuda = sa3_cuda.diagnostic_manifest(
            evidence, tflite_ready=ready["state"] == STATE_READY, env=env
        )
    return {
        **ready,
        "activeBackend": backend_value,
        "preference": preference,
        "preferenceChoices": [item.value for item in sa3_cuda.BackendPreference],
        "cuda": cuda,
        "capabilities": capabilities,
        "generation": dict(_generation_state),
        "maxSeconds": dict(MAX_SECONDS_FOR),
    }


def _tflite_threads(env: Mapping[str, str]) -> int:
    raw = env.get("LSDJ_SA3_TFLITE_THREADS", str(TFLITE_THREADS_DEFAULT))
    try:
        threads = int(raw)
    except ValueError:
        raise GenerationUnavailable(
            "LSDJ_SA3_TFLITE_THREADS must be an integer"
        ) from None
    if not 1 <= threads <= TFLITE_THREADS_MAX:
        raise GenerationUnavailable(
            f"LSDJ_SA3_TFLITE_THREADS must be 1-{TFLITE_THREADS_MAX}"
        )
    return threads


def _required_tflite_assets(request: GenerationRequest) -> tuple[pathlib.Path, ...]:
    paths = [*_TFLITE_SHARED_ASSETS, *_TFLITE_MODEL_ASSETS[request.kind]]
    if request.init_audio is not None:
        paths.append(_TFLITE_ENCODER_ASSET[request.kind])
    return tuple(paths)


def _preflight(selection: RuntimeSelection, request: GenerationRequest) -> None:
    if request.inpaint_range is not None and request.init_audio is None:
        raise UnsupportedCapability("inpainting requires init audio")
    if request.negative_prompt is not None and (
        request.cfg is None or request.cfg == 1
    ):
        raise UnsupportedCapability("negative prompt requires CFG other than 1")
    if request.apg is not None and (request.cfg is None or request.cfg == 1):
        raise UnsupportedCapability("APG requires CFG other than 1")
    if request.lora_strengths is not None and len(request.lora_strengths) != len(
        request.lora_dirs or ()
    ):
        raise UnsupportedCapability("every LoRA must have exactly one aligned strength")
    if not MIN_STEPS <= request.steps <= MAX_STEPS:
        raise UnsupportedCapability(f"steps must be {MIN_STEPS}-{MAX_STEPS}")
    if selection.backend is not BackendName.TFLITE:
        return
    if not (selection.runtime_dir / WARMED_STAMP).is_file():
        raise GenerationUnavailable(
            "the TFLite runtime has not completed its verified warm-up"
        )
    if provenance_error := _tflite_provenance_error(selection.runtime_dir):
        raise GenerationUnavailable(provenance_error)
    missing = [
        str(path)
        for path in _required_tflite_assets(request)
        if not (selection.runtime_dir / path).is_file()
    ]
    if missing:
        names = ", ".join(missing)
        raise GenerationUnavailable(
            "the pinned TFLite model bundle is incomplete; install it from the "
            f"model manager before generating (missing: {names})"
        )


def build_argv(
    selection: RuntimeSelection,
    request: GenerationRequest,
    *,
    out_path: pathlib.Path,
    init_path: pathlib.Path | None,
    env: Mapping[str, str] | None = None,
) -> list[str]:
    """Translate the neutral request to an official CLI argument vector."""
    env = os.environ if env is None else env
    _preflight(selection, request)
    argv = [
        str(selection.executable),
        str(selection.script),
        "--prompt",
        request.prompt,
        "--dit",
        KINDS[request.kind],
        "--decoder",
        DECODERS[request.kind],
        "--seconds",
        f"{request.seconds:g}",
        "--steps",
        str(request.steps),
        "--out",
        str(out_path),
    ]
    if selection.backend is BackendName.TFLITE:
        argv.extend(("--precision", "fp32", "--threads", str(_tflite_threads(env))))
    if request.init_audio is not None:
        if init_path is None:
            raise UnsupportedCapability("init audio requires a normalized input path")
        argv.extend(("--init-audio", str(init_path)))
    if request.init_noise_level is not None:
        argv.extend(("--init-noise-level", f"{request.init_noise_level:g}"))
    if request.inpaint_range is not None:
        start, end = request.inpaint_range
        argv.extend(("--inpaint-range", f"{start:g},{end:g}"))
    if request.negative_prompt is not None:
        argv.extend(("--negative-prompt", request.negative_prompt))
    if request.cfg is not None:
        argv.extend(("--cfg", f"{request.cfg:g}"))
    if request.apg is not None:
        argv.extend(("--apg", f"{request.apg:g}"))
    if request.seed is not None:
        argv.extend(("--seed", str(request.seed)))
    for index, lora_dir in enumerate(request.lora_dirs or ()):
        argv.extend(("--lora", lora_dir))
        if request.lora_strengths is not None:
            argv.append(f"strength={request.lora_strengths[index]:g}")
    return argv


_PROGRESS_PATTERNS = (
    ("sampling", re.compile(r"sampling step (\d+)/(\d+)")),
    ("decode", re.compile(r"decode chunk (\d+)/(\d+)")),
)
_SENSITIVE_OUTPUT = re.compile(
    r"(?i)(prompt|init audio|--lora|hf[_-]?token|hugging_face_hub_token|authorization)"
)


def _progress_from_line(line: str) -> ProgressEvent | None:
    for stage, pattern in _PROGRESS_PATTERNS:
        if match := pattern.search(line):
            return ProgressEvent(
                stage=stage,
                current=int(match.group(1)),
                total=int(match.group(2)),
                message=f"{stage} {match.group(1)}/{match.group(2)}",
            )
    return None


async def _drain_output(
    stream: asyncio.StreamReader,
    on_progress: Callable[[ProgressEvent], None] | None,
) -> str:
    tail = bytearray()
    pending = bytearray()
    while chunk := await stream.read(4096):
        tail.extend(chunk)
        if len(tail) > 8192:
            del tail[:-8192]
        pending.extend(chunk)
        while True:
            newline = pending.find(b"\n")
            if newline < 0:
                break
            line = bytes(pending[:newline]).decode(errors="replace")
            del pending[: newline + 1]
            if len(pending) > 8192:
                del pending[:-8192]
            event = _progress_from_line(line)
            if event is not None and on_progress is not None:
                on_progress(event)
        if len(pending) > 8192:
            del pending[:-8192]
    return tail.decode(errors="replace")


def _safe_failure_tail(output: str, backend: BackendName) -> str:
    lines = [
        line.strip()
        for line in output.splitlines()
        if line.strip() and not _SENSITIVE_OUTPUT.search(line)
    ]
    tail = "\n".join(lines[-8:])[-1000:]
    return tail or f"the {backend.value} Stable Audio process failed"


async def _stop_process(process: asyncio.subprocess.Process) -> None:
    if process.returncode is not None:
        return
    if os.name == "posix":
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGTERM)
    else:
        with contextlib.suppress(ProcessLookupError):
            process.terminate()
    try:
        await asyncio.wait_for(process.wait(), timeout=1.0)
        return
    except TimeoutError:
        pass
    if os.name == "posix":
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
    else:
        with contextlib.suppress(ProcessLookupError):
            process.kill()
    await process.wait()


def _child_environment(selection: RuntimeSelection) -> dict[str, str]:
    env = dict(os.environ)
    # Models are installed and verified by the app.  Missing files must fail
    # rather than trigger upstream's mutable first-run downloader.
    env["HF_HUB_OFFLINE"] = "1"
    env["HF_HUB_DISABLE_TELEMETRY"] = "1"
    env["DO_NOT_TRACK"] = "1"
    # Windows otherwise inherits a legacy console/filesystem encoding (often
    # cp1252), which can make valid Unicode asset paths fail before inference.
    env["PYTHONUTF8"] = "1"
    env["PYTHONIOENCODING"] = "utf-8"
    env.pop("HF_TOKEN", None)
    env.pop("HUGGING_FACE_HUB_TOKEN", None)
    if selection.backend is BackendName.TFLITE:
        threads = str(_tflite_threads(os.environ))
        env["OMP_NUM_THREADS"] = threads
        env["OPENBLAS_NUM_THREADS"] = threads
        env["TF_NUM_INTRAOP_THREADS"] = threads
    return env


async def _run_cli(
    selection: RuntimeSelection,
    argv: list[str],
    *,
    seconds: float,
    cancel_event: asyncio.Event | None,
    on_progress: Callable[[ProgressEvent], None] | None,
) -> tuple[int, str]:
    spawn_options: dict = {
        "cwd": selection.runtime_dir,
        "env": _child_environment(selection),
        "stdout": asyncio.subprocess.PIPE,
        "stderr": asyncio.subprocess.STDOUT,
    }
    if os.name == "posix":
        spawn_options["start_new_session"] = True
    elif os.name == "nt":
        flags = subprocess.CREATE_NEW_PROCESS_GROUP
        if selection.backend is BackendName.TFLITE:
            flags |= subprocess.BELOW_NORMAL_PRIORITY_CLASS
        spawn_options["creationflags"] = flags
    process = await asyncio.create_subprocess_exec(*argv, **spawn_options)
    if selection.backend is BackendName.TFLITE and hasattr(os, "setpriority"):
        with contextlib.suppress(OSError):
            os.setpriority(os.PRIO_PROCESS, process.pid, 10)
    assert process.stdout is not None
    drain = asyncio.create_task(_drain_output(process.stdout, on_progress))
    wait = asyncio.create_task(process.wait())
    cancel = (
        asyncio.create_task(cancel_event.wait()) if cancel_event is not None else None
    )
    watched = {wait}
    if cancel is not None:
        watched.add(cancel)
    try:
        done, _ = await asyncio.wait(
            watched, timeout=timeout_for(seconds), return_when=asyncio.FIRST_COMPLETED
        )
        if not done:
            await _stop_process(process)
            raise GenerationFailed(
                f"generation timed out after {timeout_for(seconds):g}s"
            )
        if cancel is not None and cancel in done and cancel.result():
            await _stop_process(process)
            raise GenerationCancelled("generation cancelled")
        return_code = await wait
        return return_code, await drain
    except asyncio.CancelledError:
        await _stop_process(process)
        raise
    finally:
        if cancel is not None:
            cancel.cancel()
        if not drain.done():
            drain.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await drain


_generation_lock = asyncio.Semaphore(1)


async def generate(
    prompt: str,
    seconds: float,
    kind: str,
    *,
    init_audio: bytes | None = None,
    init_noise_level: float | None = None,
    inpaint_range: tuple[float, float] | None = None,
    negative_prompt: str | None = None,
    cfg: float | None = None,
    apg: float | None = None,
    seed: int | None = None,
    steps: int = SAMPLER_STEPS,
    lora_dirs: Sequence[str] | None = None,
    lora_strengths: Sequence[float] | None = None,
    cancel_event: asyncio.Event | None = None,
    on_progress: Callable[[ProgressEvent], None] | None = None,
) -> bytes:
    """Generate one validated WAV through the platform-selected backend."""
    request = GenerationRequest(
        prompt=prompt,
        seconds=seconds,
        kind=kind,
        init_audio=init_audio,
        init_noise_level=init_noise_level,
        inpaint_range=inpaint_range,
        negative_prompt=negative_prompt,
        cfg=cfg,
        apg=apg,
        seed=seed,
        steps=steps,
        lora_dirs=lora_dirs,
        lora_strengths=lora_strengths,
    )
    preference_value = os.environ.get(
        SA3_PREFERENCE_ENV, sa3_cuda.BackendPreference.AUTO.value
    )
    try:
        preference = sa3_cuda.BackendPreference(preference_value)
    except ValueError:
        raise GenerationUnavailable(
            f"{SA3_PREFERENCE_ENV} must be auto, gpu, or cpu_tflite"
        ) from None
    if preference is sa3_cuda.BackendPreference.GPU:
        platform_value, machine_value = sa3_cuda.host_identity()
        evidence = sa3_cuda.CudaEvidence(
            platform=platform_value,
            machine=machine_value,
            runtime_ready=False,
            provenance_complete=False,
            packages={},
            cuda_available=False,
            cuda_runtime=None,
            driver=None,
            device=None,
            compute_capability=None,
            total_vram_bytes=None,
            free_vram_bytes=None,
            estimated_vram_bytes={kind: None},
        )
        try:
            sa3_cuda.choose_backend(
                preference,
                kind=kind,
                cuda=evidence,
                tflite_ready=resolve_runtime() is not None,
            )
        except sa3_cuda.CudaUnavailable as error:
            fallback = (
                " Choose CPU/TFLite to confirm the fallback."
                if error.fallback_available
                else ""
            )
            raise GenerationUnavailable(f"{error}{fallback}") from None
        # The release gate currently makes this unreachable.  Do not start the
        # experimental worker through the public endpoint until the installer
        # can produce a complete provenance stamp and hardware evidence flips
        # HARDWARE_QUALIFIED in the same reviewed change.
        raise GenerationUnavailable(
            "the CUDA worker is not exposed until its release gates are complete"
        )
    try:
        selection = resolve_runtime()
    except GenerationUnavailable:
        raise
    if selection is None:
        backend = select_backend()
        raise GenerationUnavailable(
            f"the {backend.value} Stable Audio runtime is unavailable; "
            "install the pinned runtime and model bundle from the model manager"
        )

    normalized = None
    if init_audio is not None:
        try:
            normalized = inspect_canonical_wav(init_audio)
        except AudioFormatError:
            try:
                normalized = normalize_wav(init_audio)
            except AudioFormatError as error:
                raise GenerationFailed(str(error)) from None

    mode = request.mode(
        input_seconds=None if normalized is None else normalized.seconds
    )
    _generation_state.update(
        state="queued",
        backend=selection.backend.value,
        mode=mode.value,
        progress=None,
    )

    def report(event: ProgressEvent) -> None:
        _generation_state["progress"] = event.as_dict()
        if on_progress is not None:
            on_progress(event)

    async with _generation_lock:
        _generation_state["state"] = "running"
        staging = runtime_paths.staging_home()
        if staging is not None:
            staging.mkdir(parents=True, exist_ok=True)
        try:
            with tempfile.TemporaryDirectory(prefix="sa3-", dir=staging) as tmp:
                tmp_path = pathlib.Path(tmp)
                out_path = tmp_path / "out.wav"
                init_path = None
                if normalized is not None:
                    init_path = tmp_path / "init.wav"
                    init_path.write_bytes(normalized.wav)
                argv = build_argv(
                    selection,
                    request,
                    out_path=out_path,
                    init_path=init_path,
                )
                return_code, output = await _run_cli(
                    selection,
                    argv,
                    seconds=seconds,
                    cancel_event=cancel_event,
                    on_progress=report,
                )
                if return_code != 0 or not out_path.is_file():
                    raise GenerationFailed(
                        _safe_failure_tail(output, selection.backend)
                    )
                max_output_bytes = round(seconds * 44_100) * 4 + 1024 * 1024
                if out_path.stat().st_size > max_output_bytes:
                    raise GenerationFailed(
                        "backend WAV is larger than the requested duration"
                    )
                try:
                    return _validate_output_wav(out_path.read_bytes(), seconds)
                except AudioFormatError as error:
                    raise GenerationFailed(str(error)) from None
        finally:
            _generation_state.update(
                state="idle", backend=None, mode=None, progress=None
            )
