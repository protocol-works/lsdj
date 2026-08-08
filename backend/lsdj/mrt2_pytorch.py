"""Thin LSDJ adapter for Apolinario's pinned PyTorch MRT2 snapshots.

All heavyweight imports and snapshot lookups happen in the supervised worker.
Acquisition is deliberately out of band: this adapter opens only immutable,
already-installed snapshots and never performs a network download at startup.
"""

from __future__ import annotations

import contextlib
import importlib.metadata
import math
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

from . import runtime_paths
from .gpu_broker import GpuBroker, Priority
from .engine import (
    CFG_MUSICCOCA,
    CFG_NOTES,
    CHANNELS,
    EMBED_CACHE_SIZE,
    FRAME_SECONDS,
    FRAMES_PER_CHUNK,
    MAX_CFG,
    MAX_DRUM_CFG,
    MAX_SAMPLE_SECONDS,
    MAX_CHUNK_FRAMES,
    MIN_CFG,
    MIN_DRUM_CFG,
    MIN_SAMPLE_SECONDS,
    MIN_CHUNK_FRAMES,
    MIN_TEMPERATURE,
    MIN_TOP_K,
    NOTE_ONSET,
    NOTE_SLOTS,
    NOTE_STATES,
    NOTE_SUSTAIN,
    SAMPLE_CACHE_SIZE,
    SAMPLE_RATE,
    TEMPERATURE,
    TOP_K,
)
from .mrt2 import (
    MODEL_SNAPSHOTS,
    PROCESSOR_SNAPSHOT,
    PYTORCH_CUDA_RUNTIME,
    UPSTREAM_SOURCE,
    RuntimeSelection,
    RuntimeUnavailable,
)

MAX_SEED = (1 << 63) - 1


@dataclass(frozen=True)
class PytorchBindings:
    torch: Any
    auto_model: Any
    snapshot_download: Any
    versions: dict[str, str]


def _package_version(name: str) -> str:
    try:
        return importlib.metadata.version(name)
    except importlib.metadata.PackageNotFoundError:
        return "unknown"


def load_bindings() -> PytorchBindings:
    """Load the separately-installed CUDA runtime only inside its worker."""

    try:
        import torch
        from huggingface_hub import snapshot_download
        from transformers import AutoModel
    except ImportError as error:
        raise RuntimeUnavailable(
            "the pinned PyTorch MRT2 runtime is not installed; use LSDJ's model "
            "manager instead of installing system Python packages"
        ) from error
    return PytorchBindings(
        torch=torch,
        auto_model=AutoModel,
        snapshot_download=snapshot_download,
        versions={
            "torch": _package_version("torch"),
            "transformers": _package_version("transformers"),
            "huggingface_hub": _package_version("huggingface-hub"),
        },
    )


def _driver_version(torch: Any) -> str | None:
    getter = getattr(getattr(torch, "_C", None), "_cuda_getDriverVersion", None)
    if not callable(getter):
        return None
    try:
        value = int(getter())
    except (RuntimeError, TypeError, ValueError):
        return None
    # CUDA returns e.g. 13020 for 13.2.  Preserve the raw value too imprecisely
    # only when its layout is unexpected rather than inventing a version.
    if value < 1000:
        return str(value)
    major = value // 1000
    minor = (value % 1000) // 10
    return f"{major}.{minor}"


class PytorchMrt2Engine:
    """LSDJ's model contract over an immutable Transformers snapshot."""

    def __init__(
        self,
        model: str = "mrt2_small",
        *,
        selection: RuntimeSelection,
        bindings: PytorchBindings | None = None,
        cache_root: Path | None = None,
        gpu_broker: GpuBroker | None = None,
    ) -> None:
        if selection.name != PYTORCH_CUDA_RUNTIME:
            raise RuntimeUnavailable(
                f"PyTorch adapter received the wrong runtime {selection.name!r}"
            )
        if model not in MODEL_SNAPSHOTS:
            raise ValueError(f"unknown pinned PyTorch MRT2 model {model!r}")
        self._selection = selection
        self._bindings = bindings or load_bindings()
        torch = self._bindings.torch
        if not torch.cuda.is_available():
            raise RuntimeUnavailable(
                "PyTorch reports no CUDA accelerator; MRT2 has no CPU fallback"
            )
        if not getattr(torch.version, "cuda", None):
            raise RuntimeUnavailable(
                "the installed PyTorch build has no CUDA runtime; MRT2 has no CPU fallback"
            )

        if cache_root is None:
            assets = runtime_paths.assets_home()
            if assets is None:
                raise RuntimeUnavailable(
                    "LSDJ_ASSETS_HOME is missing; the native host must supply the "
                    "app-owned model root"
                )
            cache = assets / "mrt2-pytorch" / "huggingface"
        else:
            cache = cache_root
        model_pin = MODEL_SNAPSHOTS[model]
        try:
            model_path = self._bindings.snapshot_download(
                repo_id=model_pin["repository"],
                revision=model_pin["revision"],
                cache_dir=str(cache),
                local_files_only=True,
            )
            processor_path = self._bindings.snapshot_download(
                repo_id=PROCESSOR_SNAPSHOT["repository"],
                revision=PROCESSOR_SNAPSHOT["revision"],
                cache_dir=str(cache),
                local_files_only=True,
            )
        except Exception as error:
            raise RuntimeUnavailable(
                "the pinned MRT2 model or MusicCoCa snapshot is missing or corrupt; "
                "install/repair it through LSDJ's model manager"
            ) from error

        # `trust_remote_code` is safe only because model_path resolves the exact
        # installer-verified revision above.  Never pass a mutable repository ID.
        try:
            upstream = self._bindings.auto_model.from_pretrained(
                model_path,
                trust_remote_code=True,
                dtype=torch.bfloat16,
                local_files_only=True,
            )
            self._system = upstream.to("cuda").eval()
            self._system.load_processor(processor_path, device="cuda")
        except Exception as error:
            raise RuntimeUnavailable(
                "the pinned PyTorch MRT2 snapshot could not initialize on CUDA"
            ) from error

        self._model = model
        self._model_pin = model_pin
        self._model_lock = threading.RLock()
        broker_root = runtime_paths.cache_home()
        self._gpu_broker = (
            gpu_broker
            if gpu_broker is not None
            else None
            if broker_root is None
            else GpuBroker(broker_root / "gpu-broker")
        )
        self._warmup_owner = True
        self._init_deck_state()

    def _init_deck_state(self) -> None:
        self._state: Any = None
        self._style: list[int] | None = None
        self._notes: list[int] | None = None
        self._drums: int | None = None
        self._drums_cfg: float | None = None
        self._temperature = TEMPERATURE
        self._top_k = TOP_K
        self._cfg_musiccoca = CFG_MUSICCOCA
        self._cfg_notes = CFG_NOTES
        self._chunk_frames = FRAMES_PER_CHUNK
        self._seed = 0
        self._embed_cache: dict[str, Any] = {}
        self._samples: dict[str, Any] = {}

    def shared_deck(self) -> "PytorchMrt2Engine":
        """A second deck state sharing this process's single loaded model."""

        deck = self.__class__.__new__(self.__class__)
        deck._selection = self._selection
        deck._bindings = self._bindings
        deck._system = self._system
        deck._model = self._model
        deck._model_pin = self._model_pin
        deck._model_lock = self._model_lock
        deck._gpu_broker = self._gpu_broker
        deck._warmup_owner = False
        deck._init_deck_state()
        return deck

    @property
    def chunk_seconds(self) -> float:
        return self._chunk_frames * FRAME_SECONDS

    def _embed_text(self, text: str) -> Any:
        if text in self._embed_cache:
            self._embed_cache[text] = self._embed_cache.pop(text)
        else:
            with self._model_lock:
                embedding = self._system.processor.embed(text)
            if len(self._embed_cache) >= EMBED_CACHE_SIZE:
                self._embed_cache.pop(next(iter(self._embed_cache)))
            self._embed_cache[text] = embedding
        return self._embed_cache[text]

    def embed_sample(self, sample_id: str, pcm: bytes) -> None:
        samples = np.frombuffer(pcm, dtype="<f4")
        if samples.size == 0 or samples.size % CHANNELS:
            raise ValueError("sample PCM must be whole interleaved stereo frames")
        seconds = samples.size / CHANNELS / SAMPLE_RATE
        if not MIN_SAMPLE_SECONDS <= seconds <= MAX_SAMPLE_SECONDS:
            raise ValueError(
                f"sample must be {MIN_SAMPLE_SECONDS}-{MAX_SAMPLE_SECONDS}s, "
                f"got {seconds:.1f}s"
            )
        audio = samples.reshape(-1, CHANNELS).astype(np.float32)
        with self._model_lock:
            embedding = self._system.processor.embed((audio, SAMPLE_RATE))
        if sample_id not in self._samples and len(self._samples) >= SAMPLE_CACHE_SIZE:
            self._samples.pop(next(iter(self._samples)))
        self._samples[sample_id] = embedding

    def set_style(
        self,
        prompts: list[tuple[str, float]],
        sample_keys: frozenset[str] = frozenset(),
    ) -> None:
        weighted = [(key, float(weight)) for key, weight in prompts if weight > 0]
        if not weighted:
            raise ValueError("set_style needs at least one prompt with weight > 0")
        total = sum(weight for _, weight in weighted)
        blend: Any = None
        for key, weight in weighted:
            if key in sample_keys:
                if key not in self._samples:
                    raise ValueError(f"unknown sample {key!r} — re-sample the deck")
                embedding = self._samples.pop(key)
                self._samples[key] = embedding
            else:
                embedding = self._embed_text(key)
            term = embedding * (weight / total)
            blend = term if blend is None else blend + term
        with self._model_lock:
            tokens = self._system.processor.tokenize(blend)
        self._style = [int(token) for token in tokens]

    def set_notes(self, notes: list[int] | None) -> None:
        if notes is not None:
            if len(notes) != NOTE_SLOTS:
                raise ValueError(
                    f"notes must hold {NOTE_SLOTS} slots, got {len(notes)}"
                )
            if any(state not in NOTE_STATES for state in notes):
                raise ValueError("note states must be -1, 0, 1, 2, or 3")
        self._notes = None if notes is None else list(notes)

    def set_drums(self, flag: int | None, cfg: float | None = None) -> None:
        if flag is not None and flag not in (0, 1):
            raise ValueError("drum flag must be 0, 1, or None")
        if cfg is not None and not MIN_DRUM_CFG <= cfg <= MAX_DRUM_CFG:
            raise ValueError(
                f"drum cfg must be in [{MIN_DRUM_CFG}, {MAX_DRUM_CFG}] or None"
            )
        self._drums = flag
        self._drums_cfg = cfg

    def set_generation(
        self,
        temperature: float,
        top_k: int,
        cfg_musiccoca: float,
        cfg_notes: float,
    ) -> None:
        if not isinstance(top_k, int) or isinstance(top_k, bool) or top_k < MIN_TOP_K:
            raise ValueError(f"top_k must be an int >= {MIN_TOP_K}")
        for name, value in (("cfg_musiccoca", cfg_musiccoca), ("cfg_notes", cfg_notes)):
            if not MIN_CFG <= value <= MAX_CFG:
                raise ValueError(f"{name} must be in [{MIN_CFG}, {MAX_CFG}]")
        self._temperature = max(MIN_TEMPERATURE, temperature)
        self._top_k = top_k
        self._cfg_musiccoca = cfg_musiccoca
        self._cfg_notes = cfg_notes

    def set_chunk_frames(self, frames: int) -> None:
        if (
            not isinstance(frames, int)
            or isinstance(frames, bool)
            or not MIN_CHUNK_FRAMES <= frames <= MAX_CHUNK_FRAMES
        ):
            raise ValueError(
                f"chunk frames must be an int in [{MIN_CHUNK_FRAMES}, {MAX_CHUNK_FRAMES}]"
            )
        self._chunk_frames = frames

    def set_seed(self, seed: int) -> None:
        if (
            not isinstance(seed, int)
            or isinstance(seed, bool)
            or not 0 <= seed <= MAX_SEED
        ):
            raise ValueError(f"seed must be an int in [0, {MAX_SEED}]")
        self._seed = seed
        # Upstream creates the RNG only for a fresh state.  Reset-to-reseed is
        # explicit, never a misleading live seed change.
        self._state = None

    def reset(self, *, seed: int | None = None) -> None:
        if seed is not None:
            self.set_seed(seed)
        else:
            self._state = None

    def _generate(
        self,
        *,
        frames: int,
        state: Any,
        style: Any,
        stream_conditioning: bool = True,
    ) -> tuple[np.ndarray, Any]:
        notes = self._notes if stream_conditioning else None
        drums = self._drums if stream_conditioning else None
        broker_hold = (
            contextlib.nullcontext()
            if self._gpu_broker is None
            else self._gpu_broker.hold(
                "mrt2",
                priority=Priority.MRT2_REALTIME,
                reservation_bytes=0,
                capacity_bytes=int(
                    self._bindings.torch.cuda.get_device_properties(
                        self._bindings.torch.cuda.current_device()
                    ).total_memory
                ),
                timeout_seconds=max(10.0, frames * FRAME_SECONDS),
            )
        )
        # Acquire the cross-process priority lease before the in-process model
        # lock. A waiting MRT2 lease makes a background SA3 callback cancel its
        # disposable process, while the two deck states remain serialized here.
        with broker_hold:
            with self._model_lock:
                audio, state = self._system.generate(
                    style=style,
                    notes=notes,
                    drums=None if drums is None else [drums],
                    cfg_drums=self._drums_cfg if stream_conditioning else None,
                    temperature=self._temperature,
                    top_k=self._top_k,
                    cfg_musiccoca=self._cfg_musiccoca,
                    cfg_notes=self._cfg_notes,
                    frames=frames,
                    seed=self._seed,
                    state=state,
                    guidance=True,
                )
        samples = np.asarray(audio)
        expected = frames * round(SAMPLE_RATE * FRAME_SECONDS)
        if samples.ndim != 2 or samples.shape != (expected, CHANNELS):
            raise RuntimeError(
                "upstream PyTorch MRT2 returned invalid audio shape "
                f"{samples.shape!r}; expected {(expected, CHANNELS)!r}"
            )
        if not np.isfinite(samples).all():
            raise RuntimeError("upstream PyTorch MRT2 returned non-finite audio")
        return samples.astype("<f4", copy=False), state

    def warm_up(self) -> None:
        # Exercise model, CUDA kernels, and decoder before readiness, then clear
        # every continuation/RNG state so the first audible stream is fresh.
        if not self._warmup_owner:
            return
        self._generate(
            frames=1,
            state=None,
            style=None,
            stream_conditioning=False,
        )
        self.reset()

    def generate_chunk(self) -> bytes:
        samples, self._state = self._generate(
            frames=self._chunk_frames,
            state=self._state,
            style=self._style,
        )
        if self._notes is not None:
            self._notes = [
                NOTE_SUSTAIN if state == NOTE_ONSET else state for state in self._notes
            ]
        return samples.tobytes()

    def render_clip(self, prompt: str, seconds: float) -> bytes:
        embedding = self._embed_text(prompt)
        with self._model_lock:
            style = [int(token) for token in self._system.processor.tokenize(embedding)]
        state: Any = None
        pieces = []
        for _ in range(math.ceil(seconds / (FRAMES_PER_CHUNK * FRAME_SECONDS))):
            samples, state = self._generate(
                frames=FRAMES_PER_CHUNK,
                state=state,
                style=style,
                stream_conditioning=False,
            )
            pieces.append(samples)
        return (
            np.concatenate(pieces)[: round(seconds * SAMPLE_RATE)]
            .astype("<f4", copy=False)
            .tobytes()
        )

    def diagnostics(self) -> dict[str, object]:
        torch = self._bindings.torch
        device_index = torch.cuda.current_device()
        props = torch.cuda.get_device_properties(device_index)
        return {
            "runtime": PYTORCH_CUDA_RUNTIME,
            "accelerator": "cuda",
            "acceleration_mode": "eager-guidance",
            "topology": "shared-worker-two-state",
            "gpu_broker": {
                "enabled": self._gpu_broker is not None,
                "priority": int(Priority.MRT2_REALTIME),
                "preempts": "sa3-background",
            },
            "hardware_qualified": self._selection.hardware_qualified,
            "experimental": self._selection.experimental,
            "model": self._model,
            "model_repository": self._model_pin["repository"],
            "model_revision": self._model_pin["revision"],
            "processor_repository": PROCESSOR_SNAPSHOT["repository"],
            "processor_revision": PROCESSOR_SNAPSHOT["revision"],
            "upstream_source_revision": UPSTREAM_SOURCE["revision"],
            "torch_version": self._bindings.versions["torch"],
            "transformers_version": self._bindings.versions["transformers"],
            "huggingface_hub_version": self._bindings.versions["huggingface_hub"],
            "torch_cuda_runtime": torch.version.cuda,
            "nvidia_driver": _driver_version(torch),
            "cuda_device": props.name,
            "cuda_capability": list(torch.cuda.get_device_capability(device_index)),
            "cuda_total_memory_bytes": int(props.total_memory),
            "capabilities": {
                "weighted_prompts": True,
                "audio_style": True,
                "notes": True,
                "drums": True,
                "negative_prompt": False,
                "explicit_seed": True,
                "reset_to_reseed": True,
            },
        }
