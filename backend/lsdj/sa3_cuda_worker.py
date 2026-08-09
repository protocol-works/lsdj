"""Disposable Stable Audio 3 PyTorch/CUDA worker.

The controller writes one bounded JSON request and starts this module with the
app-owned shared PyTorch interpreter.  Heavyweight imports and model allocation
occur only in this child.  Cancellation, an MRT2 priority waiter, CUDA OOM, a
driver reset, or any other failure ends the process, releasing its CUDA context
without affecting the deck workers or native audio callback.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import hmac
import importlib.metadata
import json
import os
import pathlib
import platform as host_platform
import re
import sys
import threading
import wave
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from typing import Any, Protocol

import numpy as np

from . import sa3_cuda
from .gpu_broker import GpuBroker, Lease, Priority


SCHEMA_VERSION = 1
SAMPLE_RATE = 44_100
CHANNELS = 2
MAX_JSON_BYTES = 64 * 1024
MAX_LAUNCH_TOKEN_BYTES = 512
LAUNCH_TOKEN_ENV = "LSDJ_WORKER_LAUNCH_TOKEN"
MODEL_FOR_KIND = {"music": "small-music", "sfx": "small-sfx"}
_JOB_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,63}")
_SHA256 = re.compile(r"[0-9a-f]{64}")


class WorkerError(RuntimeError):
    pass


class WorkerCancelled(WorkerError):
    pass


class ModelProtocol(Protocol):
    def load_lora(self, paths: Sequence[str]) -> None: ...

    def set_lora_strength(
        self, strength: float, lora_index: int | None = None
    ) -> None: ...

    def generate(self, **kwargs: Any) -> Any: ...


@dataclass(frozen=True)
class WorkerRequest:
    job_id: str
    launch_token_sha256: str
    prompt: str
    seconds: float
    kind: str
    steps: int
    cfg: float | None
    apg: float | None
    seed: int | None
    negative_prompt: str | None
    init_noise_level: float | None
    inpaint_range: tuple[float, float] | None
    init_audio: pathlib.Path | None
    lora_files: tuple[pathlib.Path, ...]
    lora_strengths: tuple[float, ...]
    model_dir: pathlib.Path
    output: pathlib.Path

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> "WorkerRequest":
        if value.get("schema_version") != SCHEMA_VERSION:
            raise WorkerError("unsupported CUDA worker request schema")
        prompt = value.get("prompt")
        job_id = value.get("job_id")
        launch_token_sha256 = value.get("launch_token_sha256")
        kind = value.get("kind")
        seconds = value.get("seconds")
        steps = value.get("steps")
        if not isinstance(prompt, str) or not prompt or len(prompt) > 32_000:
            raise WorkerError("prompt is invalid")
        if not isinstance(job_id, str) or _JOB_ID.fullmatch(job_id) is None:
            raise WorkerError("job_id is invalid")
        if (
            not isinstance(launch_token_sha256, str)
            or _SHA256.fullmatch(launch_token_sha256) is None
        ):
            raise WorkerError("launch_token_sha256 is invalid")
        if kind not in MODEL_FOR_KIND:
            raise WorkerError("CUDA supports only Small Music and Small SFX")
        if (
            isinstance(seconds, bool)
            or not isinstance(seconds, (int, float))
            or not 0.5 <= float(seconds) <= 32.0
        ):
            raise WorkerError("seconds is invalid")
        if (
            isinstance(steps, bool)
            or not isinstance(steps, int)
            or not 1 <= steps <= 100
        ):
            raise WorkerError("steps is invalid")
        inpaint = value.get("inpaint_range")
        if inpaint is not None:
            if (
                not isinstance(inpaint, list)
                or len(inpaint) != 2
                or any(
                    isinstance(item, bool) or not isinstance(item, (int, float))
                    for item in inpaint
                )
                or not 0 <= float(inpaint[0]) < float(inpaint[1]) <= float(seconds)
            ):
                raise WorkerError("inpaint_range is invalid")
            inpaint = (float(inpaint[0]), float(inpaint[1]))
        lora_files = value.get("lora_files", [])
        lora_strengths = value.get("lora_strengths", [])
        if (
            not isinstance(lora_files, list)
            or not isinstance(lora_strengths, list)
            or len(lora_files) != len(lora_strengths)
            or len(lora_files) > 4
            or any(not isinstance(item, str) for item in lora_files)
            or any(
                isinstance(item, bool)
                or not isinstance(item, (int, float))
                or not 0 <= float(item) <= 4
                for item in lora_strengths
            )
        ):
            raise WorkerError("LoRA stack is invalid")
        init_audio = value.get("init_audio")
        if init_audio is not None and not isinstance(init_audio, str):
            raise WorkerError("init_audio is invalid")
        if inpaint is not None and init_audio is None:
            raise WorkerError("inpainting requires init_audio")
        return cls(
            job_id=job_id,
            launch_token_sha256=launch_token_sha256,
            prompt=prompt,
            seconds=float(seconds),
            kind=kind,
            steps=steps,
            cfg=_optional_float(value, "cfg"),
            apg=_optional_float(value, "apg"),
            seed=_optional_int(value, "seed"),
            negative_prompt=_optional_string(value, "negative_prompt"),
            init_noise_level=_optional_float(value, "init_noise_level"),
            inpaint_range=inpaint,
            init_audio=None if init_audio is None else pathlib.Path(init_audio),
            lora_files=tuple(pathlib.Path(item) for item in lora_files),
            lora_strengths=tuple(float(item) for item in lora_strengths),
            model_dir=pathlib.Path(_required_string(value, "model_dir")),
            output=pathlib.Path(_required_string(value, "output")),
        )


def _required_string(value: Mapping[str, Any], field: str) -> str:
    item = value.get(field)
    if not isinstance(item, str) or not item:
        raise WorkerError(f"{field} is invalid")
    return item


def _optional_string(value: Mapping[str, Any], field: str) -> str | None:
    item = value.get(field)
    if item is None:
        return None
    if not isinstance(item, str) or not item or len(item) > 32_000:
        raise WorkerError(f"{field} is invalid")
    return item


def _optional_float(value: Mapping[str, Any], field: str) -> float | None:
    item = value.get(field)
    if item is None:
        return None
    if isinstance(item, bool) or not isinstance(item, (int, float)):
        raise WorkerError(f"{field} is invalid")
    result = float(item)
    if not np.isfinite(result):
        raise WorkerError(f"{field} is invalid")
    return result


def _optional_int(value: Mapping[str, Any], field: str) -> int | None:
    item = value.get(field)
    if item is None:
        return None
    if isinstance(item, bool) or not isinstance(item, int):
        raise WorkerError(f"{field} is invalid")
    return item


def read_request(path: pathlib.Path) -> WorkerRequest:
    if path.is_symlink() or not path.is_file():
        raise WorkerError("CUDA worker request must be a regular file")
    if path.stat().st_size > MAX_JSON_BYTES:
        raise WorkerError("CUDA worker request is too large")
    try:
        parsed = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise WorkerError("CUDA worker request is unreadable") from error
    if not isinstance(parsed, dict):
        raise WorkerError("CUDA worker request must be an object")
    return WorkerRequest.from_dict(parsed)


def verify_launch_token(
    request: WorkerRequest, env: Mapping[str, str] | None = None
) -> None:
    environment = os.environ if env is None else env
    token = environment.get(LAUNCH_TOKEN_ENV)
    if token is None or not 32 <= len(token.encode("utf-8")) <= MAX_LAUNCH_TOKEN_BYTES:
        raise WorkerError("CUDA worker launch authorization is missing or invalid")
    actual = hashlib.sha256(token.encode("utf-8")).hexdigest()
    if not hmac.compare_digest(actual, request.launch_token_sha256):
        raise WorkerError("CUDA worker launch authorization does not match the request")


def verify_provenance(path: pathlib.Path, request: WorkerRequest) -> dict[str, Any]:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_JSON_BYTES:
        raise WorkerError("CUDA provenance must be a bounded regular file")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise WorkerError("CUDA provenance is unreadable") from error
    expected = {
        "schema_version": 1,
        "backend": sa3_cuda.BACKEND_NAME,
        "gated_artifacts_complete": True,
        "source_revision": sa3_cuda.SOURCE_REVISION,
        "runtime_lock_sha256": sa3_cuda.RUNTIME_LOCK_SHA256,
        "packages": sa3_cuda.EXPECTED_PACKAGES,
        "model": sa3_cuda.MODEL_PINS[request.kind],
    }
    if value != expected:
        raise WorkerError(
            "CUDA provenance does not match the immutable source/runtime/model pin"
        )
    root = path.parent.resolve(strict=True)
    model_dir = request.model_dir.resolve(strict=True)
    expected_model_dir = (root / "models" / MODEL_FOR_KIND[request.kind]).resolve()
    if model_dir != expected_model_dir:
        raise WorkerError("CUDA model path is outside its verified runtime bundle")
    return value


def _single_safetensors(path: pathlib.Path) -> pathlib.Path:
    if path.suffix == ".safetensors" and path.is_file() and not path.is_symlink():
        return path
    if path.is_dir() and not path.is_symlink():
        hits = [
            item
            for item in path.iterdir()
            if item.is_file()
            and not item.is_symlink()
            and item.suffix == ".safetensors"
        ]
        if len(hits) == 1:
            return hits[0]
    raise WorkerError("each LoRA must resolve to exactly one regular safetensors file")


def _load_pcm16(path: pathlib.Path) -> Any:
    try:
        with wave.open(str(path), "rb") as source:
            if (
                source.getnchannels() != CHANNELS
                or source.getsampwidth() != 2
                or source.getframerate() != SAMPLE_RATE
                or source.getcomptype() != "NONE"
            ):
                raise WorkerError("init audio is not canonical PCM16")
            frames = source.getnframes()
            raw = source.readframes(frames)
    except (EOFError, OSError, wave.Error) as error:
        raise WorkerError("init audio is unreadable") from error
    if frames < 1 or len(raw) != frames * CHANNELS * 2:
        raise WorkerError("init audio is empty or truncated")
    return (
        np.frombuffer(raw, dtype="<i2").reshape(frames, CHANNELS).T.astype(np.float32)
        / 32768.0
    )


def _to_numpy(audio: Any) -> np.ndarray:
    if isinstance(audio, np.ndarray):
        return audio
    value = audio.detach().to("cpu").float().numpy()
    return np.asarray(value)


def write_pcm16(path: pathlib.Path, audio: Any, seconds: float) -> None:
    samples = _to_numpy(audio)
    if samples.ndim == 3 and samples.shape[0] == 1:
        samples = samples[0]
    if samples.ndim != 2 or samples.shape[0] != CHANNELS:
        raise WorkerError(f"upstream returned invalid audio shape {samples.shape!r}")
    frames = round(seconds * SAMPLE_RATE)
    if samples.shape[1] < frames or not np.isfinite(samples[:, :frames]).all():
        raise WorkerError("upstream returned short or non-finite audio")
    clipped = np.clip(samples[:, :frames], -1.0, 1.0)
    pcm = np.where(clipped <= -1, -32768, np.rint(clipped * 32767)).astype("<i2")
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_symlink():
        raise WorkerError("output path must not be a symlink")
    with wave.open(str(path), "wb") as output:
        output.setnchannels(CHANNELS)
        output.setsampwidth(2)
        output.setframerate(SAMPLE_RATE)
        output.writeframes(pcm.T.tobytes())


def generation_kwargs(
    request: WorkerRequest,
    *,
    torch_module: Any,
    progress: Callable[[int, int], None],
) -> dict[str, Any]:
    init = None
    if request.init_audio is not None:
        waveform = torch_module.from_numpy(_load_pcm16(request.init_audio))
        init = (SAMPLE_RATE, waveform)
    kwargs: dict[str, Any] = {
        "prompt": request.prompt,
        "negative_prompt": request.negative_prompt,
        "duration": request.seconds,
        "steps": request.steps,
        "cfg_scale": 1.0 if request.cfg is None else request.cfg,
        "apg_scale": 1.0 if request.apg is None else request.apg,
        "seed": -1 if request.seed is None else request.seed,
        "batch_size": 1,
        "chunked_decode": True,
        "callback": lambda info: progress(int(info["i"]) + 1, request.steps),
        "disable_tqdm": True,
    }
    if request.inpaint_range is not None:
        kwargs.update(
            {
                "inpaint_audio": init,
                "inpaint_mask_start_seconds": request.inpaint_range[0],
                "inpaint_mask_end_seconds": request.inpaint_range[1],
                "init_audio": None,
            }
        )
    else:
        kwargs.update(
            {
                "init_audio": init,
                "init_noise_level": (
                    0.9
                    if request.init_noise_level is None
                    else request.init_noise_level
                ),
                "inpaint_audio": None,
            }
        )
    return kwargs


def run_generation(
    request: WorkerRequest,
    *,
    model: ModelProtocol,
    torch_module: Any,
    cancelled: Callable[[], bool],
    broker: GpuBroker | None = None,
    lease: Lease | None = None,
    emit: Callable[[dict[str, object]], None] = lambda event: None,
) -> None:
    lora_files = [_single_safetensors(path) for path in request.lora_files]
    if lora_files:
        model.load_lora([str(path) for path in lora_files])
        for index, strength in enumerate(request.lora_strengths):
            model.set_lora_strength(strength, lora_index=index)

    def progress(current: int, total: int) -> None:
        if cancelled():
            raise WorkerCancelled("Stable Audio generation was cancelled")
        if broker is not None and lease is not None and broker.should_yield(lease):
            raise WorkerCancelled("Stable Audio yielded to realtime MRT2 generation")
        emit(
            {
                "event": "progress",
                "stage": "sampling",
                "current": current,
                "total": total,
            }
        )

    kwargs = generation_kwargs(request, torch_module=torch_module, progress=progress)
    audio = model.generate(**kwargs)
    emit({"event": "progress", "stage": "decoding", "current": None, "total": None})
    write_pcm16(request.output, audio, request.seconds)
    emit({"event": "done"})


def _package_versions() -> dict[str, str]:
    versions = {}
    for name in sa3_cuda.EXPECTED_PACKAGES:
        try:
            versions[name] = importlib.metadata.version(name)
        except importlib.metadata.PackageNotFoundError:
            versions[name] = "missing"
    return versions


def _nvml_driver_version() -> str | None:
    if os.name != "nt":
        return None
    import ctypes

    library = None
    try:
        library = ctypes.WinDLL("nvml.dll")
        if library.nvmlInit_v2() != 0:
            return None
        buffer = ctypes.create_string_buffer(96)
        if library.nvmlSystemGetDriverVersion(buffer, len(buffer)) != 0:
            return None
        return buffer.value.decode("ascii", "strict")
    except (AttributeError, OSError, UnicodeDecodeError):
        return None
    finally:
        if library is not None:
            with contextlib.suppress(Exception):
                library.nvmlShutdown()


def _load_production_runtime() -> tuple[Any, Callable[[WorkerRequest], ModelProtocol]]:
    try:
        import torch
        from stable_audio_3.loading_utils import load_diffusion_cond
        from stable_audio_3.model import StableAudioModel
    except ImportError as error:
        raise WorkerError(
            "the pinned Stable Audio PyTorch dependency is missing"
        ) from error

    def load(request: WorkerRequest) -> ModelProtocol:
        config = request.model_dir / "model_config.json"
        checkpoint = request.model_dir / "model.safetensors"
        if any(
            path.is_symlink() or not path.is_file() for path in (config, checkpoint)
        ):
            raise WorkerError("the verified Stable Audio model bundle is incomplete")
        try:
            model_config = json.loads(config.read_text(encoding="utf-8"))
            upstream = load_diffusion_cond(
                model_config, str(checkpoint), device="cuda", model_half=True
            )
            upstream.use_lora = False
            upstream.lora_names = []
            return StableAudioModel(upstream, model_config, "cuda", True)
        except Exception as error:
            raise WorkerError(
                "the pinned Stable Audio model could not initialize"
            ) from error

    return torch, load


def _emit(event: dict[str, object]) -> None:
    sys.stdout.write(json.dumps(event, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def _job_emitter(job_id: str) -> Callable[[dict[str, object]], None]:
    def emit(event: dict[str, object]) -> None:
        _emit({"jobId": job_id, **event})

    return emit


def start_broker_watchdog(
    broker: GpuBroker,
    lease: Lease,
    emit: Callable[[dict[str, object]], None],
    *,
    poll_seconds: float = 0.05,
    exit_process: Callable[[int], None] = os._exit,
) -> tuple[threading.Event, threading.Thread]:
    """Hard-stop model loading/decoding when realtime MRT2 needs the GPU.

    The sampler callback provides graceful yield during diffusion. Loading and
    decoding are upstream calls with no cancellation callback, so a daemon
    watchdog terminates only this disposable worker. Process exit is the
    reliable CUDA-context/VRAM release boundary.
    """

    stop = threading.Event()

    def watch() -> None:
        while not stop.wait(poll_seconds):
            try:
                should_yield = broker.should_yield(lease)
            except Exception:
                should_yield = True
            if should_yield:
                emit(
                    {
                        "event": "cancelled",
                        "message": "Stable Audio yielded to realtime MRT2 generation",
                    }
                )
                exit_process(2)
                return

    thread = threading.Thread(target=watch, name="sa3-gpu-yield", daemon=True)
    thread.start()
    return stop, thread


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="LSDJ disposable SA3 CUDA worker")
    parser.add_argument("--request", required=True)
    parser.add_argument("--cancel-file", required=True)
    parser.add_argument("--broker-root", required=True)
    parser.add_argument("--provenance", required=True)
    parser.add_argument("--reservation-bytes", required=True, type=int)
    args = parser.parse_args(argv)
    request: WorkerRequest | None = None
    emit = _emit
    model: ModelProtocol | None = None
    torch: Any | None = None
    try:
        request = read_request(pathlib.Path(args.request))
        emit = _job_emitter(request.job_id)
        verify_launch_token(request)
        # The token authenticates this one request/child pairing.  Upstream
        # imports and any grandchildren must never inherit it.
        os.environ.pop(LAUNCH_TOKEN_ENV, None)
        provenance = verify_provenance(pathlib.Path(args.provenance), request)
        cancel_file = pathlib.Path(args.cancel_file)
        torch, load_model = _load_production_runtime()
        versions = _package_versions()
        if not torch.cuda.is_available():
            raise WorkerError(
                "PyTorch reports no CUDA device; there is no PyTorch CPU fallback"
            )
        free_bytes, total_bytes = torch.cuda.mem_get_info()
        properties = torch.cuda.get_device_properties(torch.cuda.current_device())
        evidence = sa3_cuda.CudaEvidence(
            platform=sys.platform,
            machine=os.environ.get("PROCESSOR_ARCHITECTURE") or host_platform.machine(),
            runtime_ready=True,
            provenance_complete=True,
            packages=versions,
            cuda_available=torch.cuda.is_available(),
            cuda_runtime=torch.version.cuda,
            driver=_nvml_driver_version(),
            device=properties.name,
            compute_capability=tuple(torch.cuda.get_device_capability()),
            total_vram_bytes=int(total_bytes),
            free_vram_bytes=int(free_bytes),
            estimated_vram_bytes={request.kind: args.reservation_bytes},
            source_revision=provenance["source_revision"],
            model_revision=provenance["model"]["revision"],
        )
        errors = sa3_cuda.runtime_errors(evidence, kind=request.kind)
        if errors:
            raise WorkerError("; ".join(errors))
        broker = GpuBroker(pathlib.Path(args.broker_root))
        capacity = max(0, int(free_bytes) - sa3_cuda.VRAM_HEADROOM_BYTES)
        with broker.hold(
            "sa3",
            priority=Priority.SA3_BACKGROUND,
            reservation_bytes=args.reservation_bytes,
            capacity_bytes=capacity,
            timeout_seconds=120,
            cancelled=cancel_file.exists,
        ) as lease:
            emit(
                {
                    "event": "progress",
                    "stage": "loading",
                    "current": None,
                    "total": None,
                }
            )
            watchdog_stop, watchdog = start_broker_watchdog(broker, lease, emit)
            try:
                model = load_model(request)
                run_generation(
                    request,
                    model=model,
                    torch_module=torch,
                    cancelled=cancel_file.exists,
                    broker=broker,
                    lease=lease,
                    emit=emit,
                )
            finally:
                watchdog_stop.set()
                watchdog.join(timeout=1)
        return 0
    except WorkerCancelled as error:
        emit({"event": "cancelled", "message": str(error)})
        return 2
    except Exception as error:
        # Only our bounded, path-free errors cross the worker boundary.  Unknown
        # upstream/OS errors are intentionally reduced to their class name so a
        # prompt, token, or app-owned filesystem path cannot leak into logs.
        message = (
            str(error)[:512]
            if isinstance(error, WorkerError)
            else f"CUDA worker failed ({type(error).__name__})"
        )
        emit({"event": "error", "message": message})
        return 1
    finally:
        model = None
        if torch is not None:
            with contextlib.suppress(Exception):
                torch.cuda.empty_cache()


if __name__ == "__main__":
    raise SystemExit(main())
