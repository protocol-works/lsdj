"""Model-free contract tests for the MLX and TFLite Stable Audio adapters."""

import asyncio
import io
import json
import os
import pathlib
import shutil
import sys
import wave

import pytest

from lsdj import runtime_paths, sa3
from lsdj.sa3_contract import BackendName, GenerationMode, GenerationRequest


SUCCESS_STUB = r"""import os
import pathlib
import shutil
import sys
import wave

runtime_dir = pathlib.Path(__file__).resolve().parent.parent
(runtime_dir / "argv.txt").write_text("\n".join(sys.argv) + "\n")
(runtime_dir / "env.txt").write_text(
    f"offline={os.environ.get('HF_HUB_OFFLINE')}\n"
    f"token={os.environ.get('HF_TOKEN')}\n"
    f"threads={os.environ.get('OMP_NUM_THREADS')}\n"
)
if "--init-audio" in sys.argv:
    source = pathlib.Path(sys.argv[sys.argv.index("--init-audio") + 1])
    shutil.copyfile(source, runtime_dir / "init.wav")
seconds = float(sys.argv[sys.argv.index("--seconds") + 1])
frames = round(seconds * 44100)
out = pathlib.Path(sys.argv[sys.argv.index("--out") + 1])
with wave.open(str(out), "wb") as target:
    target.setnchannels(2)
    target.setsampwidth(2)
    target.setframerate(44100)
    target.writeframes(b"\0" * frames * 4)
"""

PROGRESS_STUB = SUCCESS_STUB.replace(
    "seconds = float", 'print("sampling step 1/2", flush=True)\nseconds = float'
).replace("frames = round", 'print("sampling step 2/2", flush=True)\nframes = round')

FAILURE_STUB = """import sys
print("prompt  super secret user prompt")
print("error: no DiT weights found")
sys.exit(3)
"""
SILENT_STUB = "pass\n"
CORRUPT_STUB = """import pathlib, sys
pathlib.Path(sys.argv[sys.argv.index("--out") + 1]).write_bytes(b"RIFFbad")
"""
WRONG_DURATION_STUB = """import pathlib, sys, wave
out = pathlib.Path(sys.argv[sys.argv.index("--out") + 1])
with wave.open(str(out), "wb") as target:
    target.setnchannels(2); target.setsampwidth(2); target.setframerate(44100)
    target.writeframes(b"\\0" * round(0.25 * 44100) * 4)
"""
TIMEOUT_STUB = "import time\ntime.sleep(30)\n"


def pcm16_wav(seconds: float = 0.25) -> bytes:
    output = io.BytesIO()
    with wave.open(output, "wb") as target:
        target.setnchannels(2)
        target.setsampwidth(2)
        target.setframerate(44_100)
        target.writeframes(b"\0" * round(seconds * 44_100) * 4)
    return output.getvalue()


def _install_interpreter(runtime_dir: pathlib.Path, platform_name: str) -> pathlib.Path:
    executable = runtime_paths.venv_python(
        runtime_dir / ".venv", platform=platform_name
    )
    executable.parent.mkdir(parents=True)
    (runtime_dir / ".venv" / "pyvenv.cfg").write_text(
        f"home = {sys.base_prefix}\n"
        "include-system-site-packages = false\n"
        f"version = {sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}\n"
    )
    if platform_name == sys.platform:
        if os.name == "nt":
            shutil.copyfile(sys.executable, executable)
        else:
            executable.symlink_to(sys.executable)
    else:
        executable.write_bytes(b"fake")
    return executable


def make_runtime(
    root: pathlib.Path,
    backend: BackendName,
    stub: str = SUCCESS_STUB,
    *,
    platform_name: str | None = None,
    assets: bool = True,
) -> sa3.RuntimeSelection:
    platform_name = sys.platform if platform_name is None else platform_name
    subdir = "mlx" if backend is BackendName.MLX else "tflite"
    script_name = "sa3_mlx.py" if backend is BackendName.MLX else "sa3_tflite.py"
    runtime_dir = root / "optimized" / subdir
    script = runtime_dir / "scripts" / script_name
    script.parent.mkdir(parents=True)
    script.write_text(stub)
    executable = _install_interpreter(runtime_dir, platform_name)
    (runtime_dir / sa3.WARMED_STAMP).write_text("ready\n")
    if backend is BackendName.TFLITE:
        (runtime_dir / sa3.TFLITE_PROVENANCE_STAMP).write_text(
            json.dumps(
                {
                    "runtime": {
                        "repo": sa3.TFLITE_RUNTIME_REPO,
                        "revision": sa3.TFLITE_RUNTIME_REVISION,
                    },
                    "models": {
                        "repo": sa3.TFLITE_MODELS_REPO,
                        "revision": sa3.TFLITE_MODELS_REVISION,
                    },
                }
            )
        )
    if backend is BackendName.TFLITE and assets:
        request = GenerationRequest("probe", 0.5, "sfx", init_audio=pcm16_wav())
        for relative in sa3._required_tflite_assets(request):
            path = runtime_dir / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"fixture")
        for kind in ("music", "track"):
            request = GenerationRequest("probe", 0.5, kind, init_audio=pcm16_wav())
            for relative in sa3._required_tflite_assets(request):
                path = runtime_dir / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"fixture")
    return sa3.RuntimeSelection(backend, root, runtime_dir, executable, script)


@pytest.fixture
def tflite_runtime(tmp_path, monkeypatch):
    def install(stub=SUCCESS_STUB, *, assets=True):
        selection = make_runtime(
            tmp_path / "Stable Audio 模型", BackendName.TFLITE, stub, assets=assets
        )
        monkeypatch.setenv("SA3_HOME", str(selection.checkout))
        monkeypatch.setenv("LSDJ_SA3_BACKEND", "tflite")
        # The worker tests inject a fully resolved fake runtime so they remain
        # model-free on every CI host without weakening production's target gate.
        monkeypatch.setattr(sa3, "resolve_runtime", lambda: selection)
        return selection

    return install


@pytest.mark.parametrize(
    ("platform_name", "machine", "expected"),
    [
        ("darwin", "arm64", BackendName.MLX),
        ("darwin", "aarch64", BackendName.MLX),
        ("linux", "x86_64", BackendName.TFLITE),
        ("win32", "AMD64", BackendName.TFLITE),
    ],
)
def test_backend_selection_is_explicit(platform_name, machine, expected):
    assert (
        sa3.select_backend({}, platform_name=platform_name, machine=machine) is expected
    )


def test_backend_override_is_validated():
    assert (
        sa3.select_backend(
            {"LSDJ_SA3_BACKEND": "tflite"},
            platform_name="linux",
            machine="x86_64",
        )
        is BackendName.TFLITE
    )
    with pytest.raises(sa3.GenerationUnavailable, match="does not support"):
        sa3.select_backend(
            {"LSDJ_SA3_BACKEND": "tflite"},
            platform_name="darwin",
            machine="x86_64",
        )
    with pytest.raises(sa3.GenerationUnavailable, match="requires Apple Silicon"):
        sa3.select_backend(
            {"LSDJ_SA3_BACKEND": "mlx"},
            platform_name="win32",
            machine="AMD64",
        )
    with pytest.raises(sa3.GenerationUnavailable, match="must be"):
        sa3.select_backend(
            {"LSDJ_SA3_BACKEND": "cuda"},
            platform_name="linux",
            machine="x86_64",
        )


def test_unsupported_platform_fails_instead_of_guessing():
    for platform_name, machine in (("freebsd", "x86_64"), ("linux", "aarch64")):
        with pytest.raises(sa3.GenerationUnavailable, match="no Stable Audio backend"):
            sa3.select_backend({}, platform_name=platform_name, machine=machine)


def test_runtime_resolution_uses_windows_venv_layout(tmp_path):
    selection = make_runtime(
        tmp_path / "Audio Runtime",
        BackendName.TFLITE,
        platform_name="win32",
    )
    resolved = sa3.resolve_runtime(
        {"SA3_HOME": str(selection.checkout)},
        platform_name="win32",
        machine="AMD64",
    )
    assert resolved is not None
    assert resolved.executable.name == "python.exe"
    assert resolved.executable.parent.name == "Scripts"


def test_status_exposes_backend_capabilities_and_real_limitations(tmp_path):
    selection = make_runtime(tmp_path / "sa3", BackendName.TFLITE)
    result = sa3.status(
        {"SA3_HOME": str(selection.checkout)},
        platform_name="linux",
        machine="x86_64",
    )
    assert result["state"] == sa3.STATE_READY
    assert result["backend"] == "tflite"
    assert result["capabilities"]["preview"] is False
    assert result["capabilities"]["cancellation"] is True
    assert any(
        "Per-step LoRA" in item for item in result["capabilities"]["limitations"]
    )


def test_windows_status_exposes_conservative_backend_choices_and_cuda_blockers(
    tmp_path,
):
    selection = make_runtime(
        tmp_path / "sa3", BackendName.TFLITE, platform_name="win32"
    )
    result = sa3.status(
        {"SA3_HOME": str(selection.checkout)},
        platform_name="win32",
        machine="AMD64",
    )

    assert result["preference"] == "auto"
    assert result["preferenceChoices"] == ["auto", "gpu", "cpu_tflite"]
    assert result["activeBackend"] == "tflite"
    assert result["cuda"]["release_ready"] is False
    assert result["cuda"]["tflite_fallback_ready"] is True
    assert any("gated" in item for item in result["cuda"]["qualification_blockers"])


def test_explicit_gpu_fails_before_start_and_requires_confirmed_tflite_fallback(
    tflite_runtime, monkeypatch
):
    tflite_runtime()
    monkeypatch.setenv(sa3.SA3_PREFERENCE_ENV, "gpu")

    with pytest.raises(sa3.GenerationUnavailable, match="Choose CPU/TFLite"):
        asyncio.run(sa3.generate("kick", 0.5, "sfx"))


def test_status_fails_closed_for_unverified_tflite_provenance(tmp_path):
    selection = make_runtime(tmp_path / "sa3", BackendName.TFLITE)
    (selection.runtime_dir / sa3.TFLITE_PROVENANCE_STAMP).write_text("{}")
    result = sa3.status(
        {"SA3_HOME": str(selection.checkout)},
        platform_name="linux",
        machine="x86_64",
    )
    assert result["state"] == sa3.STATE_FAILED
    assert "do not match" in result["detail"]


def test_generation_modes_include_continuation():
    request = GenerationRequest(
        "continue",
        2.0,
        "music",
        init_audio=b"wav",
        inpaint_range=(1.0, 2.0),
    )
    assert request.mode(input_seconds=1.0) is GenerationMode.CONTINUATION
    assert request.mode(input_seconds=0.5) is GenerationMode.INPAINT


def _option(argv: list[str], name: str) -> str:
    return argv[argv.index(name) + 1]


def test_mlx_and_tflite_translate_the_same_service_controls(tmp_path):
    mlx = make_runtime(tmp_path / "mlx-root", BackendName.MLX)
    tflite = make_runtime(tmp_path / "tflite-root", BackendName.TFLITE)
    request = GenerationRequest(
        "warm dub loop",
        0.5,
        "music",
        init_audio=pcm16_wav(),
        init_noise_level=0.6,
        inpaint_range=(0.1, 0.4),
        negative_prompt="vocals",
        cfg=4.5,
        apg=0.75,
        seed=12345,
        steps=12,
        lora_dirs=["/adapters/one", "/adapters/two"],
        lora_strengths=[0.75, 1.5],
    )
    commands = [
        sa3.build_argv(
            selection,
            request,
            out_path=tmp_path / f"{selection.backend}.wav",
            init_path=tmp_path / "init.wav",
            env={},
        )
        for selection in (mlx, tflite)
    ]
    for flag in (
        "--prompt",
        "--dit",
        "--decoder",
        "--seconds",
        "--steps",
        "--init-audio",
        "--init-noise-level",
        "--inpaint-range",
        "--negative-prompt",
        "--cfg",
        "--apg",
        "--seed",
    ):
        assert _option(commands[0], flag) == _option(commands[1], flag)
    assert commands[1][commands[1].index("--precision") + 1] == "fp32"
    assert commands[1][commands[1].index("--threads") + 1] == "4"
    first_lora = commands[1].index("--lora")
    assert commands[1][first_lora : first_lora + 6] == [
        "--lora",
        "/adapters/one",
        "strength=0.75",
        "--lora",
        "/adapters/two",
        "strength=1.5",
    ]


def test_long_medium_request_maps_to_the_official_model_without_allocating_audio(
    tmp_path,
):
    selection = make_runtime(tmp_path / "sa3", BackendName.TFLITE)
    request = GenerationRequest("long-form dub", 380.0, "track", steps=8)
    argv = sa3.build_argv(
        selection,
        request,
        out_path=tmp_path / "out.wav",
        init_path=None,
        env={},
    )
    assert _option(argv, "--dit") == "medium"
    assert _option(argv, "--decoder") == "same-l"
    assert _option(argv, "--seconds") == "380"
    assert sa3.timeout_for(380.0) == sa3.TIMEOUT_SECONDS + 380.0


def test_generate_returns_a_validated_wav_and_runs_offline(tflite_runtime):
    selection = tflite_runtime()
    wav = asyncio.run(sa3.generate("vinyl spinback", 0.5, "sfx", seed=7))
    assert sa3.inspect_canonical_wav(wav).frames == 22_050
    env = (selection.runtime_dir / "env.txt").read_text()
    assert "offline=1" in env
    assert "token=None" in env
    assert "threads=4" in env


def test_generate_passes_full_control_surface_and_normalized_input(tflite_runtime):
    selection = tflite_runtime()
    source = pcm16_wav()
    asyncio.run(
        sa3.generate(
            "warm dub loop",
            0.5,
            "music",
            init_audio=source,
            init_noise_level=0.6,
            inpaint_range=(0.1, 0.4),
            negative_prompt="vocals",
            cfg=4.5,
            apg=0.75,
            seed=12345,
            steps=12,
        )
    )
    argv = (selection.runtime_dir / "argv.txt").read_text().splitlines()
    assert _option(argv, "--steps") == "12"
    assert _option(argv, "--inpaint-range") == "0.1,0.4"
    assert _option(argv, "--negative-prompt") == "vocals"
    assert _option(argv, "--cfg") == "4.5"
    assert _option(argv, "--apg") == "0.75"
    assert (selection.runtime_dir / "init.wav").read_bytes() == source


def test_missing_pinned_asset_fails_before_spawn(tflite_runtime):
    selection = tflite_runtime(assets=False)
    with pytest.raises(sa3.GenerationUnavailable, match="bundle is incomplete"):
        asyncio.run(sa3.generate("anything", 0.5, "sfx"))
    assert not (selection.runtime_dir / "argv.txt").exists()


def test_cli_failure_is_bounded_and_redacts_the_prompt(tflite_runtime):
    tflite_runtime(FAILURE_STUB)
    with pytest.raises(sa3.GenerationFailed) as caught:
        asyncio.run(sa3.generate("super secret user prompt", 0.5, "sfx"))
    assert "no DiT weights" in str(caught.value)
    assert "super secret" not in str(caught.value)


@pytest.mark.parametrize("stub", [SILENT_STUB, CORRUPT_STUB, WRONG_DURATION_STUB])
def test_missing_corrupt_or_wrong_duration_output_fails(tflite_runtime, stub):
    tflite_runtime(stub)
    with pytest.raises(sa3.GenerationFailed):
        asyncio.run(sa3.generate("anything", 0.5, "sfx"))


def test_timeout_stops_the_worker(tflite_runtime, monkeypatch):
    tflite_runtime(TIMEOUT_STUB)
    monkeypatch.setattr(sa3, "TIMEOUT_SECONDS", 0.05)
    with pytest.raises(sa3.GenerationFailed, match="timed out"):
        asyncio.run(sa3.generate("anything", 0.5, "sfx"))
    assert sa3.status()["generation"]["state"] == "idle"


def test_explicit_cancellation_stops_the_worker(tflite_runtime):
    tflite_runtime(TIMEOUT_STUB)

    async def run():
        cancelled = asyncio.Event()
        task = asyncio.create_task(
            sa3.generate("anything", 0.5, "sfx", cancel_event=cancelled)
        )
        await asyncio.sleep(0.1)
        cancelled.set()
        await task

    with pytest.raises(sa3.GenerationCancelled, match="cancelled"):
        asyncio.run(run())
    assert sa3.status()["generation"]["state"] == "idle"


def test_progress_is_normalized_from_the_official_text_stream(tflite_runtime):
    tflite_runtime(PROGRESS_STUB)
    events = []
    asyncio.run(sa3.generate("anything", 0.5, "sfx", on_progress=events.append))
    assert [(event.stage, event.current, event.total) for event in events] == [
        ("sampling", 1, 2),
        ("sampling", 2, 2),
    ]


def test_no_runtime_raises_unavailable(monkeypatch, tmp_path):
    monkeypatch.setenv("LSDJ_SA3_BACKEND", "tflite")
    monkeypatch.setenv("SA3_HOME", str(tmp_path / "missing"))
    monkeypatch.setattr(sa3, "resolve_runtime", lambda: None)
    monkeypatch.setattr(sa3, "select_backend", lambda: BackendName.TFLITE)
    with pytest.raises(sa3.GenerationUnavailable, match="tflite"):
        asyncio.run(sa3.generate("anything", 0.5, "sfx"))
