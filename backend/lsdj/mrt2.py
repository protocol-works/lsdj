"""Runtime-neutral Magenta RealTime 2 selection and engine contract.

The Rust host always names a runtime explicitly.  This module keeps the
platform policy and release qualification gate independent from either model
stack, so selecting PyTorch can never degrade silently to CPU inference.
"""

from __future__ import annotations

import os
import sys
from dataclasses import asdict, dataclass
from typing import Mapping, Protocol, runtime_checkable


MLX_RUNTIME = "mlx"
PYTORCH_CUDA_RUNTIME = "pytorch-cuda"
AUTO_RUNTIME = "auto"
RUNTIME_CHOICES = (AUTO_RUNTIME, MLX_RUNTIME, PYTORCH_CUDA_RUNTIME)

# The #109 spike established API and packaging feasibility, but neither target
# OS has completed the required two-deck NVIDIA run.  Keep that release fact in
# executable metadata instead of allowing an unqualified backend to look ready.
PYTORCH_HARDWARE_QUALIFIED = False
UNVERIFIED_OPT_IN = "LSDJ_ALLOW_UNVERIFIED_MRT2_CUDA"

ADAPTER_REFERENCE = {
    "repository": "https://github.com/multimodalart/magenta-realtime-torch.git",
    "revision": "6d076baa3df3b10448876c400521a015a5137c59",
    "license": "Apache-2.0",
    "credit": "Apolinario",
    "role": "implementation reference; not executed by LSDJ",
}
MODEL_SNAPSHOTS = {
    "mrt2_base": {
        "repository": "magenta-community/magenta-realtime-2",
        "revision": "92087988d05d0fe38b11f021f0b0d00a75afb86b",
    },
    "mrt2_small": {
        "repository": "magenta-community/magenta-realtime-2-small",
        "revision": "7037d99551c84ac5c6afb7f1a5e58c65e7233dbb",
    },
}
PROCESSOR_SNAPSHOT = {
    "repository": "magenta-community/magenta-rt-musiccoca-torch",
    "revision": "236c488e38aa98643805514996934d705668298b",
}
RUNTIME_CANDIDATE = {
    "python": "3.12",
    "torch": "2.12.1",
    "transformers": "5.8.0",
    "huggingface_hub": "1.5.0",
    "numpy": "2.3.5",
    "safetensors": "0.7.0",
    "sentencepiece": "0.2.1",
    "resampy": "0.4.3",
    "cuda_wheel": "cu130",
    "lock_status": "hash_locked_uninstalled",
    "locks": {
        "linux-x86_64": "runtime-locks/mrt2-pytorch-linux-x86_64.txt",
        "windows-x86_64": "runtime-locks/mrt2-pytorch-windows-x86_64.txt",
    },
}


class RuntimeUnavailable(RuntimeError):
    """The requested MRT2 runtime cannot be used safely on this host."""


def public_startup_error(error: Exception) -> str:
    """Return a bounded, non-sensitive startup diagnostic for the UI."""

    if isinstance(error, RuntimeUnavailable):
        return str(error)[:512]
    return (
        f"{type(error).__name__}: MRT2 worker startup failed; "
        "inspect the local application log for details"
    )


@dataclass(frozen=True)
class RuntimeSelection:
    name: str
    platform: str
    accelerator: str
    hardware_qualified: bool
    experimental: bool


@runtime_checkable
class Mrt2Engine(Protocol):
    """The model-independent contract consumed by ``run_deck_worker``."""

    @property
    def chunk_seconds(self) -> float: ...

    def set_style(
        self,
        prompts: list[tuple[str, float]],
        sample_keys: frozenset[str] = frozenset(),
    ) -> None: ...

    def embed_sample(self, sample_id: str, pcm: bytes) -> None: ...

    def set_notes(self, notes: list[int] | None) -> None: ...

    def set_drums(self, flag: int | None, cfg: float | None = None) -> None: ...

    def set_generation(
        self,
        temperature: float,
        top_k: int,
        cfg_musiccoca: float,
        cfg_notes: float,
    ) -> None: ...

    def set_chunk_frames(self, frames: int) -> None: ...

    def generate_chunk(self) -> bytes: ...

    def render_clip(self, prompt: str, seconds: float) -> bytes: ...

    def diagnostics(self) -> dict[str, object]: ...


def _platform_family(platform: str) -> str:
    value = platform.lower()
    if value.startswith("darwin"):
        return "macos"
    if value.startswith("linux"):
        return "linux"
    if value.startswith(("win32", "cygwin", "msys")):
        return "windows"
    return value


def _truthy(value: str | None) -> bool:
    return value is not None and value.strip().lower() in {"1", "true", "yes", "on"}


def select_runtime(
    requested: str = AUTO_RUNTIME,
    *,
    platform: str | None = None,
    env: Mapping[str, str] | None = None,
) -> RuntimeSelection:
    """Resolve a platform backend and enforce the #109 qualification gate."""

    platform_name = _platform_family(sys.platform if platform is None else platform)
    environment = os.environ if env is None else env
    if requested not in RUNTIME_CHOICES:
        raise RuntimeUnavailable(
            f"unknown MRT2 runtime {requested!r}; expected one of {RUNTIME_CHOICES}"
        )
    runtime = requested
    if runtime == AUTO_RUNTIME:
        if platform_name == "macos":
            runtime = MLX_RUNTIME
        elif platform_name in {"linux", "windows"}:
            runtime = PYTORCH_CUDA_RUNTIME
        else:
            raise RuntimeUnavailable(
                f"MRT2 has no runtime for unsupported platform {platform_name!r}"
            )

    if runtime == MLX_RUNTIME:
        if platform_name != "macos":
            raise RuntimeUnavailable(
                f"the MLX MRT2 runtime is macOS-only, not {platform_name}"
            )
        return RuntimeSelection(runtime, platform_name, "metal", True, False)

    if platform_name not in {"linux", "windows"}:
        raise RuntimeUnavailable(
            "the PyTorch CUDA MRT2 runtime is supported only on Linux and Windows"
        )
    experimental = _truthy(environment.get(UNVERIFIED_OPT_IN))
    if not PYTORCH_HARDWARE_QUALIFIED and not experimental:
        raise RuntimeUnavailable(
            "the PyTorch CUDA MRT2 runtime is implemented but not release-qualified: "
            "issue #109 still requires Linux and Windows NVIDIA two-deck hardware "
            f"results; {UNVERIFIED_OPT_IN}=1 is reserved for that qualification run"
        )
    return RuntimeSelection(
        runtime,
        platform_name,
        "cuda",
        PYTORCH_HARDWARE_QUALIFIED,
        experimental,
    )


def runtime_manifest() -> dict[str, object]:
    """Immutable dependency/install metadata exposed without importing a model."""

    return {
        "schema_version": 1,
        "runtime": PYTORCH_CUDA_RUNTIME,
        "release_ready": PYTORCH_HARDWARE_QUALIFIED,
        "supported_platforms": ["linux", "windows"],
        "accelerator": "nvidia-cuda",
        "cpu_fallback": False,
        "topology": "shared-worker-two-state",
        "topology_implemented": True,
        "adapter_reference": dict(ADAPTER_REFERENCE),
        "executable_remote_code": {
            name: dict(pin) for name, pin in MODEL_SNAPSHOTS.items()
        },
        "models": {name: dict(pin) for name, pin in MODEL_SNAPSHOTS.items()},
        "processor": dict(PROCESSOR_SNAPSHOT),
        "runtime_candidate": dict(RUNTIME_CANDIDATE),
        "qualification_blockers": [
            "Linux NVIDIA two-deck 25-frame and 5-frame ten-minute results",
            "Windows NVIDIA two-deck 25-frame and 5-frame ten-minute results",
            "clean-host lock installation and minimum driver selection",
            "issue #108 notices and download acknowledgement",
        ],
    }


def create_engine(
    *,
    model: str,
    runtime: str,
    platform: str | None = None,
    env: Mapping[str, str] | None = None,
) -> Mrt2Engine:
    """Construct exactly the selected backend; never fall back to another one."""

    selection = select_runtime(runtime, platform=platform, env=env)
    if selection.name == MLX_RUNTIME:
        from .engine import DeckEngine

        return DeckEngine(model=model)

    from .mrt2_pytorch import PytorchMrt2Engine

    return PytorchMrt2Engine(model=model, selection=selection)


def selection_dict(selection: RuntimeSelection) -> dict[str, object]:
    """Stable JSON-ready representation for diagnostics and tests."""

    return asdict(selection)
