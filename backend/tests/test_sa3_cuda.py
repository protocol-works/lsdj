import pytest

from lsdj import sa3_cuda


def evidence(**updates):
    values = {
        "platform": "win32",
        "machine": "AMD64",
        "runtime_ready": True,
        "provenance_complete": True,
        "packages": dict(sa3_cuda.EXPECTED_PACKAGES),
        "cuda_available": True,
        "cuda_runtime": "12.6",
        "driver": "560.76",
        "device": "NVIDIA test device",
        "compute_capability": (8, 9),
        "total_vram_bytes": 24 * 1024**3,
        "free_vram_bytes": 16 * 1024**3,
        "estimated_vram_bytes": {"music": 6 * 1024**3, "sfx": 6 * 1024**3},
        "source_revision": sa3_cuda.SOURCE_REVISION,
        "model_revision": sa3_cuda.MODEL_PINS["music"]["revision"],
    }
    values.update(updates)
    return sa3_cuda.CudaEvidence(**values)


def test_auto_keeps_tflite_until_hardware_is_release_qualified():
    decision = sa3_cuda.choose_backend(
        "auto", kind="music", cuda=evidence(), tflite_ready=True, env={}
    )
    assert decision.backend == "tflite"
    assert decision.fallback is True


def test_explicit_gpu_never_silently_falls_back():
    with pytest.raises(sa3_cuda.CudaUnavailable) as caught:
        sa3_cuda.choose_backend(
            "gpu", kind="music", cuda=evidence(), tflite_ready=True, env={}
        )
    assert caught.value.fallback_available is True
    assert caught.value.reason == "cuda_not_eligible"


def test_qualification_opt_in_allows_small_models_but_not_auto():
    explicit = sa3_cuda.choose_backend(
        "gpu",
        kind="music",
        cuda=evidence(estimated_vram_bytes={"music": None}),
        tflite_ready=True,
        env={sa3_cuda.UNVERIFIED_OPT_IN: "1"},
    )
    automatic = sa3_cuda.choose_backend(
        "auto",
        kind="music",
        cuda=evidence(estimated_vram_bytes={"music": None}),
        tflite_ready=True,
        env={sa3_cuda.UNVERIFIED_OPT_IN: "1"},
    )
    assert explicit.backend == "pytorch_cuda"
    assert automatic.backend == "tflite"


@pytest.mark.parametrize(
    "updates, expected",
    [
        ({"provenance_complete": False}, "provenance is incomplete"),
        ({"source_revision": "0" * 40}, "source revision does not match"),
        ({"model_revision": "0" * 40}, "model revision does not match"),
        ({"cuda_available": False}, "no CUDA device"),
        ({"cuda_runtime": "13.0"}, "not the pinned 12.6"),
        ({"driver": None}, "driver version could not be verified"),
        ({"driver": "528.33"}, "older than the provisional"),
        (
            {"packages": {**sa3_cuda.EXPECTED_PACKAGES, "torch": "2.12.1+cu130"}},
            "dependency versions do not match",
        ),
    ],
)
def test_explicit_gpu_fails_closed_on_runtime_mismatch(updates, expected):
    with pytest.raises(sa3_cuda.CudaUnavailable, match=expected):
        sa3_cuda.choose_backend(
            "gpu",
            kind="music",
            cuda=evidence(**updates),
            tflite_ready=True,
            env={sa3_cuda.UNVERIFIED_OPT_IN: "1"},
        )


def test_medium_stays_on_tflite_without_an_official_windows_flashattention_build():
    with pytest.raises(
        sa3_cuda.CudaUnavailable, match="Medium requires FlashAttention"
    ):
        sa3_cuda.choose_backend(
            "gpu",
            kind="track",
            cuda=evidence(estimated_vram_bytes={"track": 12 * 1024**3}),
            tflite_ready=True,
            env={sa3_cuda.UNVERIFIED_OPT_IN: "1"},
        )


def test_free_vram_is_advisory_but_still_a_conservative_admission_gate():
    errors = sa3_cuda.runtime_errors(
        evidence(free_vram_bytes=6 * 1024**3), kind="music"
    )
    assert any("headroom" in error for error in errors)


def test_cpu_choice_requires_the_portable_baseline():
    with pytest.raises(
        sa3_cuda.CudaUnavailable, match="TFLite backend is not installed"
    ):
        sa3_cuda.choose_backend(
            "cpu_tflite", kind="music", cuda=evidence(), tflite_ready=False
        )


def test_diagnostics_are_honest_about_hardware_and_gated_hashes():
    status = sa3_cuda.diagnostic_manifest(evidence(), tflite_ready=True)
    assert status["release_ready"] is False
    assert status["cpu_fallback"] is False
    assert any("gated" in item for item in status["qualification_blockers"])
