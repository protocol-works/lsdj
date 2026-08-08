"""sa3 generation tests: checkout resolution and the subprocess contract.

A copied or linked Python interpreter runs a fake sa3_mlx CLI so the real spawn
path — argument passing, --out handling, failure, and timeout mapping — is
exercised without MLX or weights.
"""

import asyncio
import os
import pathlib
import shutil
import sys

import pytest

from lsdj import runtime_paths, sa3

FAKE_WAV = b"RIFFfakewavdata"

# Writes the fake WAV to whatever follows --out and records one argv element per
# line beside the copied venv interpreter so tests can assert the exact CLI
# contract. This is Python rather than a shell stub so the subprocess contract
# runs unchanged on macOS, Linux, and Windows without a model runtime.
SUCCESS_STUB = """import pathlib
import shutil
import sys

args = [sys.argv[0], *sys.argv[1:]]
runtime_dir = pathlib.Path(sys.executable).parent
(runtime_dir / "argv.txt").write_text("\\n".join(args) + "\\n")
out = pathlib.Path(sys.argv[sys.argv.index("--out") + 1])
if "--init-audio" in sys.argv:
    init_audio = pathlib.Path(sys.argv[sys.argv.index("--init-audio") + 1])
    shutil.copyfile(init_audio, runtime_dir / "init.wav")
out.write_bytes(b"RIFFfakewavdata")
"""

FAILURE_STUB = """import sys
print("error: no DiT weights found")
sys.exit(3)
"""

# Exits cleanly without writing the WAV.
SILENT_STUB = """pass
"""

TIMEOUT_STUB = """import time
time.sleep(30)
"""


def make_checkout(root: pathlib.Path, stub_body: str) -> pathlib.Path:
    """Lay out <root>/optimized/mlx with a portable fake CLI runtime."""
    mlx_dir = root / "optimized" / "mlx"
    venv = mlx_dir / ".venv"
    python = runtime_paths.venv_python(venv)
    python.parent.mkdir(parents=True)
    (mlx_dir / "scripts").mkdir()
    (mlx_dir / "scripts" / "sa3_mlx.py").write_text(stub_body)
    (venv / "pyvenv.cfg").write_text(
        f"home = {sys.base_prefix}\n"
        "include-system-site-packages = false\n"
        f"version = {sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}\n"
    )
    if os.name == "nt":
        # Creating symlinks normally requires elevated Windows privileges.
        # A copied interpreter plus pyvenv.cfg exercises the real Scripts layout.
        shutil.copyfile(sys.executable, python)
    else:
        # Preserve relocatable interpreter/library relationships on Unix.
        python.symlink_to(sys.executable)
    return mlx_dir


def runtime_file(mlx_dir: pathlib.Path, name: str) -> pathlib.Path:
    return runtime_paths.venv_python(mlx_dir / ".venv").parent / name


class TestResolveMlxDir:
    def test_fixture_uses_the_platform_native_venv_interpreter(self, tmp_path):
        mlx_dir = make_checkout(tmp_path / "checkout", SUCCESS_STUB)
        assert runtime_paths.venv_python(mlx_dir / ".venv").is_file()

    def test_env_override_wins(self, tmp_path):
        mlx_dir = make_checkout(tmp_path / "elsewhere", SUCCESS_STUB)
        resolved = sa3.resolve_mlx_dir(
            env={"SA3_MLX_HOME": str(tmp_path / "elsewhere")}, home=tmp_path / "home"
        )
        assert resolved == mlx_dir

    def test_resolves_the_host_supplied_assets_home(self, tmp_path):
        assets = tmp_path / "DJ Name" / "模型 assets"
        mlx_dir = make_checkout(assets / "stable-audio-3", SUCCESS_STUB)
        assert sa3.resolve_mlx_dir(env={"LSDJ_ASSETS_HOME": str(assets)}) == mlx_dir

    def test_checkout_without_venv_is_skipped(self, tmp_path):
        assets = tmp_path / "assets"
        checkout = assets / "stable-audio-3"
        (checkout / "optimized" / "mlx" / "scripts").mkdir(parents=True)
        (checkout / "optimized" / "mlx" / "scripts" / "sa3_mlx.py").write_text("#")
        assert sa3.resolve_mlx_dir(env={"LSDJ_ASSETS_HOME": str(assets)}) is None

    def test_nothing_resolves_to_none(self, tmp_path):
        assert sa3.resolve_mlx_dir(env={}) is None


@pytest.fixture
def checkout(tmp_path, monkeypatch):
    """Install a stub checkout, point SA3_MLX_HOME at it, return mlx dir."""

    def install(stub_body):
        mlx_dir = make_checkout(tmp_path / "sa3", stub_body)
        monkeypatch.setenv("SA3_MLX_HOME", str(tmp_path / "sa3"))
        return mlx_dir

    return install


class TestGenerate:
    def test_returns_wav_bytes(self, checkout):
        checkout(SUCCESS_STUB)
        wav = asyncio.run(sa3.generate("vinyl spinback", 3.0, "sfx"))
        assert wav == FAKE_WAV

    def test_default_cli_argv_is_unchanged(self, checkout):
        mlx_dir = checkout(SUCCESS_STUB)
        asyncio.run(sa3.generate("deep house loop", 7.74, "music"))
        argv = runtime_file(mlx_dir, "argv.txt").read_text().splitlines()
        assert argv[:-1] == [
            str(mlx_dir / "scripts" / "sa3_mlx.py"),
            "--prompt",
            "deep house loop",
            "--dit",
            "sm-music",
            "--decoder",
            "same-s",
            "--seconds",
            "7.74",
            "--steps",
            "8",
            "--out",
        ]
        assert pathlib.Path(argv[-1]).name == "out.wav"

    def test_passes_the_full_generation_surface_and_init_bytes(self, checkout):
        mlx_dir = checkout(SUCCESS_STUB)
        init_audio = b"RIFFsource-WAVE"
        asyncio.run(
            sa3.generate(
                "warm dub loop",
                8.0,
                "music",
                init_audio=init_audio,
                init_noise_level=0.6,
                inpaint_range=(1.25, 2.5),
                negative_prompt="vocals",
                cfg=4.5,
                apg=0.75,
                seed=12345,
            )
        )
        argv = runtime_file(mlx_dir, "argv.txt").read_text().splitlines()
        init_index = argv.index("--init-audio")
        assert pathlib.Path(argv[init_index + 1]).name == "init.wav"
        assert argv[init_index + 2 :] == [
            "--init-noise-level",
            "0.6",
            "--inpaint-range",
            "1.25,2.5",
            "--negative-prompt",
            "vocals",
            "--cfg",
            "4.5",
            "--apg",
            "0.75",
            "--seed",
            "12345",
        ]
        assert runtime_file(mlx_dir, "init.wav").read_bytes() == init_audio

    def test_passes_one_lora_group_per_adapter_with_its_strength(self, checkout):
        # Issue #66 (ADR-0028): each adapter rides the argv as its own
        # --lora group — the directory plus a strength=S option (the
        # upstream PR #57/#65 CLI syntax).
        mlx_dir = checkout(SUCCESS_STUB)
        asyncio.run(
            sa3.generate(
                "maqam phrasing",
                120.0,
                "track",
                lora_dirs=["/adapters/medium/maqam", "/adapters/medium/breaks"],
                lora_strengths=[0.75, 1.5],
            )
        )
        argv = runtime_file(mlx_dir, "argv.txt").read_text().splitlines()
        first = argv.index("--lora")
        assert argv[first : first + 6] == [
            "--lora",
            "/adapters/medium/maqam",
            "strength=0.75",
            "--lora",
            "/adapters/medium/breaks",
            "strength=1.5",
        ]

    def test_lora_without_strengths_omits_the_option(self, checkout):
        # No strengths → bare --lora groups; the CLI's default (1.0) applies.
        mlx_dir = checkout(SUCCESS_STUB)
        asyncio.run(
            sa3.generate(
                "vinyl spinback", 3.0, "sfx", lora_dirs=["/adapters/small/crackle"]
            )
        )
        argv = runtime_file(mlx_dir, "argv.txt").read_text().splitlines()
        lora_index = argv.index("--lora")
        assert argv[lora_index + 1] == "/adapters/small/crackle"
        assert not any(arg.startswith("strength=") for arg in argv)
        assert "--lora-strength" not in argv

    def test_tracks_run_the_medium_dit_with_its_decoder(self, checkout):
        # M19 (ADR-0013): tracks pair the medium DiT with SAME-L; the
        # pad kinds keep the small DiTs with SAME-S.
        mlx_dir = checkout(SUCCESS_STUB)
        asyncio.run(sa3.generate("late night dub techno", 120.0, "track"))
        argv = runtime_file(mlx_dir, "argv.txt").read_text().splitlines()
        assert argv[argv.index("--dit") + 1] == "medium"
        assert argv[argv.index("--decoder") + 1] == "same-l"
        assert argv[argv.index("--seconds") + 1] == "120"

    def test_timeout_scales_with_the_requested_length(self):
        assert sa3.timeout_for(3.0) == sa3.TIMEOUT_SECONDS + 3.0
        assert sa3.timeout_for(380.0) == sa3.TIMEOUT_SECONDS + 380.0

    def test_no_checkout_raises_unavailable(self, monkeypatch, tmp_path):
        monkeypatch.delenv("SA3_MLX_HOME", raising=False)
        monkeypatch.setenv("LSDJ_ASSETS_HOME", str(tmp_path / "assets"))
        with pytest.raises(sa3.GenerationUnavailable):
            asyncio.run(sa3.generate("anything", 3.0, "sfx"))

    def test_cli_failure_raises_with_output_tail(self, checkout):
        checkout(FAILURE_STUB)
        with pytest.raises(sa3.GenerationFailed, match="no DiT weights"):
            asyncio.run(sa3.generate("anything", 3.0, "sfx"))

    def test_clean_exit_without_wav_is_a_failure(self, checkout):
        checkout(SILENT_STUB)
        with pytest.raises(sa3.GenerationFailed):
            asyncio.run(sa3.generate("anything", 3.0, "sfx"))

    def test_timeout_kills_and_raises(self, checkout, monkeypatch):
        # The deadline is base + seconds (timeout_for), so a short clip
        # keeps the test fast while exercising the real kill path.
        checkout(TIMEOUT_STUB)
        monkeypatch.setattr(sa3, "TIMEOUT_SECONDS", 0.2)
        with pytest.raises(sa3.GenerationFailed, match="timed out"):
            asyncio.run(sa3.generate("anything", 0.5, "sfx"))
