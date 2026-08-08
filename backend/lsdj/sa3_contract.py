"""Runtime-neutral Stable Audio 3 service contract.

The desktop app owns this contract.  MLX and TFLite are implementation details:
they receive the same validated request and must either honour every populated
control or reject it explicitly.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from enum import StrEnum


class BackendName(StrEnum):
    MLX = "mlx"
    TFLITE = "tflite"


class GenerationMode(StrEnum):
    TEXT_TO_AUDIO = "text_to_audio"
    AUDIO_TO_AUDIO = "audio_to_audio"
    INPAINT = "inpaint"
    CONTINUATION = "continuation"


@dataclass(frozen=True)
class GenerationRequest:
    prompt: str
    seconds: float
    kind: str
    init_audio: bytes | None = None
    init_noise_level: float | None = None
    inpaint_range: tuple[float, float] | None = None
    negative_prompt: str | None = None
    cfg: float | None = None
    apg: float | None = None
    seed: int | None = None
    steps: int = 8
    lora_dirs: Sequence[str] | None = None
    lora_strengths: Sequence[float] | None = None

    def mode(self, *, input_seconds: float | None = None) -> GenerationMode:
        if self.init_audio is None:
            return GenerationMode.TEXT_TO_AUDIO
        if self.inpaint_range is None:
            return GenerationMode.AUDIO_TO_AUDIO
        start, end = self.inpaint_range
        one_sample = 1 / 44_100
        if (
            input_seconds is not None
            and self.seconds > input_seconds
            and abs(start - input_seconds) <= one_sample
            and abs(end - self.seconds) <= one_sample
        ):
            return GenerationMode.CONTINUATION
        return GenerationMode.INPAINT


@dataclass(frozen=True)
class BackendCapabilities:
    backend: BackendName
    modes: tuple[GenerationMode, ...]
    controls: tuple[str, ...]
    models: tuple[str, ...]
    progress: bool
    cancellation: bool
    preview: bool
    limitations: tuple[str, ...]

    def as_dict(self) -> dict:
        return {
            "backend": self.backend.value,
            "modes": [mode.value for mode in self.modes],
            "controls": list(self.controls),
            "models": list(self.models),
            "progress": self.progress,
            "cancellation": self.cancellation,
            "preview": self.preview,
            "limitations": list(self.limitations),
        }


@dataclass(frozen=True)
class ProgressEvent:
    stage: str
    current: int | None
    total: int | None
    message: str

    def as_dict(self) -> dict:
        return {
            "stage": self.stage,
            "current": self.current,
            "total": self.total,
            "message": self.message,
        }


COMMON_MODES = tuple(GenerationMode)
COMMON_CONTROLS = (
    "positive_prompt",
    "negative_prompt",
    "duration",
    "steps",
    "seed",
    "init_noise_level",
    "cfg",
    "apg",
    "inpaint_range",
    "lora",
)
COMMON_MODELS = ("small_music", "small_sfx", "medium")

MLX_CAPABILITIES = BackendCapabilities(
    backend=BackendName.MLX,
    modes=COMMON_MODES,
    controls=COMMON_CONTROLS,
    models=COMMON_MODELS,
    progress=True,
    cancellation=True,
    preview=False,
    limitations=("The pinned MLX CLI does not expose partial audio previews.",),
)

TFLITE_CAPABILITIES = BackendCapabilities(
    backend=BackendName.TFLITE,
    modes=COMMON_MODES,
    controls=COMMON_CONTROLS,
    models=COMMON_MODELS,
    progress=True,
    cancellation=True,
    preview=False,
    limitations=(
        "The official TFLite CLI does not expose partial audio previews.",
        "Per-step LoRA gating is MLX-only; LSDJ supports TFLite LoRA strength but not step ranges.",
        "The portable backend is CPU-only and does not use an NVIDIA GPU.",
    ),
)


def capabilities_for(backend: BackendName) -> BackendCapabilities:
    return MLX_CAPABILITIES if backend is BackendName.MLX else TFLITE_CAPABILITIES
