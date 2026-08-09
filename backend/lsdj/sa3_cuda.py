"""Fail-closed policy for the optional Windows Stable Audio CUDA worker.

The official TFLite backend remains the portable baseline.  This module only
selects PyTorch when its immutable shared runtime, gated model provenance,
Windows NVIDIA driver, measured VRAM reservation, and release qualification are
all present.  An explicit qualification opt-in may exercise unmeasured hardware,
but it never enables automatic selection or a PyTorch CPU fallback.
"""

from __future__ import annotations

import enum
import os
import platform as host_platform
import re
import sys
from dataclasses import asdict, dataclass
from typing import Mapping


BACKEND_NAME = "pytorch_cuda"
UNVERIFIED_OPT_IN = "LSDJ_ALLOW_UNVERIFIED_SA3_CUDA"
CUDA_RUNTIME = "12.6"
MIN_WINDOWS_DRIVER = (560, 76)
VRAM_HEADROOM_BYTES = 1024**3

# Flipped only in a PR that carries the completed physical-hardware evidence.
HARDWARE_QUALIFIED = False

EXPECTED_PACKAGES = {
    "torch": "2.7.1+cu126",
    "torchaudio": "2.7.1+cu126",
    "transformers": "5.8.0",
    "huggingface-hub": "1.7.1",
    "numpy": "2.3.5",
    "safetensors": "0.7.0",
    "sentencepiece": "0.2.1",
    "resampy": "0.4.3",
}
SOURCE_REVISION = "a0b57f5483c4588f827f3552b7d5c6ca2a9687be"
RUNTIME_LOCK_SHA256 = "3c9bf7d79c3848ebe1da40fd14b26708b55d8157f008cb3a1944ddfb1cd597c4"
MODEL_PINS = {
    "music": {
        "repository": "stabilityai/stable-audio-3-small-music",
        "revision": "0fef1392cd842149a2b6d445e181c97608faac06",
    },
    "sfx": {
        "repository": "stabilityai/stable-audio-3-small-sfx",
        "revision": "ae12755283df9d62ca39a9b050a39a0b607b8c20",
    },
}


class BackendPreference(enum.StrEnum):
    AUTO = "auto"
    GPU = "gpu"
    CPU_TFLITE = "cpu_tflite"


class CudaUnavailable(RuntimeError):
    def __init__(
        self,
        message: str,
        *,
        reason: str,
        fallback_available: bool,
    ) -> None:
        super().__init__(message)
        self.reason = reason
        self.fallback_available = fallback_available


@dataclass(frozen=True)
class CudaEvidence:
    platform: str
    machine: str
    runtime_ready: bool
    provenance_complete: bool
    packages: Mapping[str, str]
    cuda_available: bool
    cuda_runtime: str | None
    driver: str | None
    device: str | None
    compute_capability: tuple[int, int] | None
    total_vram_bytes: int | None
    free_vram_bytes: int | None
    estimated_vram_bytes: Mapping[str, int | None]
    source_revision: str | None = None
    model_revision: str | None = None

    def as_dict(self) -> dict[str, object]:
        value = asdict(self)
        if self.compute_capability is not None:
            value["compute_capability"] = list(self.compute_capability)
        return value


@dataclass(frozen=True)
class BackendDecision:
    backend: str
    preference: BackendPreference
    reason: str
    fallback: bool

    def as_dict(self) -> dict[str, object]:
        value = asdict(self)
        value["preference"] = self.preference.value
        return value


def _normalise_platform(platform_name: str) -> str:
    value = platform_name.lower()
    if value.startswith(("win32", "cygwin", "msys")):
        return "windows"
    return value


def _normalise_machine(machine: str) -> str:
    value = machine.lower()
    return "x86_64" if value in {"amd64", "x86_64"} else value


def _truthy(value: str | None) -> bool:
    return value is not None and value.strip().lower() in {"1", "true", "yes", "on"}


def parse_driver_version(version: str | None) -> tuple[int, int] | None:
    if version is None:
        return None
    match = re.fullmatch(r"\s*(\d{3,4})\.(\d{1,3})(?:\.\d+)?\s*", version)
    if match is None:
        return None
    return int(match.group(1)), int(match.group(2))


def runtime_errors(
    evidence: CudaEvidence,
    *,
    kind: str,
    allow_unmeasured_vram: bool = False,
) -> list[str]:
    errors = []
    if (
        _normalise_platform(evidence.platform) != "windows"
        or _normalise_machine(evidence.machine) != "x86_64"
    ):
        errors.append("the CUDA Stable Audio backend supports Windows x64 only")
    if not evidence.runtime_ready:
        errors.append("the shared app-owned PyTorch runtime is not ready")
    if not evidence.provenance_complete:
        errors.append("the gated Stable Audio model provenance is incomplete")
    if evidence.source_revision != SOURCE_REVISION:
        errors.append(
            "the installed Stable Audio source revision does not match the pin"
        )
    expected_model = MODEL_PINS.get(kind)
    if (
        expected_model is not None
        and evidence.model_revision != expected_model["revision"]
    ):
        errors.append(
            "the installed Stable Audio model revision does not match the pin"
        )
    mismatched = {
        name: (evidence.packages.get(name), expected)
        for name, expected in EXPECTED_PACKAGES.items()
        if evidence.packages.get(name) != expected
    }
    if mismatched:
        errors.append(
            "the installed shared PyTorch dependency versions do not match the pin"
        )
    if not evidence.cuda_available:
        errors.append(
            "PyTorch reports no CUDA device; there is no PyTorch CPU fallback"
        )
    if evidence.cuda_runtime != CUDA_RUNTIME:
        errors.append(
            f"the installed PyTorch CUDA runtime is {evidence.cuda_runtime or 'unknown'}, "
            f"not the pinned {CUDA_RUNTIME} runtime"
        )
    driver = parse_driver_version(evidence.driver)
    if driver is None:
        errors.append("the NVIDIA display driver version could not be verified")
    elif driver < MIN_WINDOWS_DRIVER:
        errors.append(
            "the NVIDIA driver is older than the provisional CUDA 12.6 floor "
            f"{MIN_WINDOWS_DRIVER[0]}.{MIN_WINDOWS_DRIVER[1]}"
        )
    if kind == "track":
        errors.append(
            "Stable Audio Medium requires FlashAttention 2; no official Windows "
            "wheel has been qualified, so Medium remains on TFLite"
        )
    estimate = evidence.estimated_vram_bytes.get(kind)
    if estimate is None:
        if not allow_unmeasured_vram:
            errors.append(f"{kind} has no qualified VRAM reservation yet")
    elif evidence.free_vram_bytes is None:
        errors.append("free CUDA memory could not be measured")
    elif evidence.free_vram_bytes < estimate + VRAM_HEADROOM_BYTES:
        errors.append(
            f"{kind} needs an estimated {estimate} bytes plus "
            f"{VRAM_HEADROOM_BYTES} bytes headroom, but only "
            f"{evidence.free_vram_bytes} bytes are free"
        )
    return errors


def choose_backend(
    preference: BackendPreference | str,
    *,
    kind: str,
    cuda: CudaEvidence,
    tflite_ready: bool,
    env: Mapping[str, str] | None = None,
) -> BackendDecision:
    try:
        preference = BackendPreference(preference)
    except ValueError:
        raise CudaUnavailable(
            "Stable Audio preference must be auto, gpu, or cpu_tflite",
            reason="invalid_preference",
            fallback_available=tflite_ready,
        ) from None
    environment = os.environ if env is None else env

    if preference is BackendPreference.CPU_TFLITE:
        if not tflite_ready:
            raise CudaUnavailable(
                "the requested TFLite backend is not installed and ready",
                reason="tflite_not_ready",
                fallback_available=False,
            )
        return BackendDecision("tflite", preference, "CPU/TFLite was selected", False)

    experimental = _truthy(environment.get(UNVERIFIED_OPT_IN))
    errors = runtime_errors(
        cuda,
        kind=kind,
        allow_unmeasured_vram=(preference is BackendPreference.GPU and experimental),
    )
    release_ready = HARDWARE_QUALIFIED and not errors
    qualification_ready = experimental and not errors

    if preference is BackendPreference.GPU:
        if not (release_ready or qualification_ready):
            if not HARDWARE_QUALIFIED and not experimental:
                errors.insert(
                    0,
                    "the Windows CUDA backend is implemented but not release-qualified; "
                    f"{UNVERIFIED_OPT_IN}=1 is reserved for hardware qualification",
                )
            raise CudaUnavailable(
                "; ".join(errors) if errors else "the CUDA backend is unavailable",
                reason="cuda_not_eligible",
                fallback_available=tflite_ready,
            )
        return BackendDecision(
            BACKEND_NAME,
            preference,
            "explicit experimental GPU qualification"
            if not HARDWARE_QUALIFIED
            else "explicit GPU selection",
            False,
        )

    if release_ready:
        return BackendDecision(
            BACKEND_NAME, preference, "qualified CUDA backend", False
        )
    if tflite_ready:
        return BackendDecision(
            "tflite",
            preference,
            "; ".join(errors)
            if errors
            else "CUDA hardware qualification is incomplete",
            True,
        )
    raise CudaUnavailable(
        "; ".join(errors + ["the TFLite fallback is not ready"]),
        reason="no_ready_backend",
        fallback_available=False,
    )


def diagnostic_manifest(
    cuda: CudaEvidence,
    *,
    tflite_ready: bool,
    env: Mapping[str, str] | None = None,
) -> dict[str, object]:
    environment = os.environ if env is None else env
    return {
        "backend": BACKEND_NAME,
        "release_ready": HARDWARE_QUALIFIED,
        "qualification_opt_in": _truthy(environment.get(UNVERIFIED_OPT_IN)),
        "cpu_fallback": False,
        "tflite_fallback_ready": tflite_ready,
        "cuda_runtime_pin": CUDA_RUNTIME,
        "minimum_windows_driver_provisional": (
            f"{MIN_WINDOWS_DRIVER[0]}.{MIN_WINDOWS_DRIVER[1]}"
        ),
        "vram_headroom_bytes": VRAM_HEADROOM_BYTES,
        "evidence": cuda.as_dict(),
        "qualification_blockers": [
            "authenticated hashes for gated Stable Audio and T5Gemma artifacts",
            "MRT2 parity on the shared torch 2.7.1/CUDA 12.6 runtime",
            "measured Small Music and Small SFX VRAM reservations",
            "Windows NVIDIA cancellation/OOM/crash/VRAM-release evidence",
            "two active MRT2 decks for ten minutes at 25- and 5-frame scheduling",
        ],
    }


def host_identity() -> tuple[str, str]:
    return sys.platform, host_platform.machine()
