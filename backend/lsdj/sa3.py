"""Stable Audio 3 generation via a spawned sa3_mlx subprocess (ADR-0012).

Nothing here imports sa3_mlx code: the checkout's own venv python runs its
CLI once per generation and the WAV comes back as bytes. The interpreter is
invoked directly — `uv run` would resolve the checkout's repo-root torch
project (measured), and the `./sa3` wrapper exists for humans and may
prompt. Generations are serialised so the transient ~1.5 GB peak never
stacks next to the two deck workers.
"""

import asyncio
import os
import pathlib
import tempfile
from collections.abc import Sequence

# CLI vocabulary of scripts/sa3_mlx.py at the pinned commit (sa3-pin.json).
# Pads use the small DiTs with the SAME-S decoder; tracks (M19, ADR-0013)
# the medium DiT, which pairs with SAME-L.
KINDS = {"sfx": "sm-sfx", "music": "sm-music", "track": "medium"}
DECODERS = {"sfx": "same-s", "music": "same-s", "track": "same-l"}
SAMPLER_STEPS = 8

MIN_SECONDS = 0.5
MAX_SECONDS = 32.0
# Stability's published ceiling for the medium DiT (6:20).
TRACK_MAX_SECONDS = 380.0
MAX_SECONDS_FOR = {"sfx": MAX_SECONDS, "music": MAX_SECONDS, "track": TRACK_MAX_SECONDS}
# A safety ceiling, not a UX limit: the prompt is passed to the sa3_mlx CLI as a
# single argv (see `generate`), so an unbounded prompt would blow the OS arg-length
# limit, and it guards the loopback endpoint against a pathological body. Set generous
# enough to hold a large structured/JSON prompt (a pasted song spec runs ~8 KB) with
# headroom, while staying far below the OS argv limit. The model's text encoder
# truncates beyond its own window anyway.
MAX_PROMPT_LENGTH = 32000

# Issue #54 generation controls. These are trust-boundary limits, mirrored by
# `controller.generate_audio`; the CLI itself is wider in places, but unbounded
# loopback input must not become unbounded argv, model guidance, or memory use.
MIN_INIT_NOISE_LEVEL = 0.01
MAX_INIT_NOISE_LEVEL = 5.0
MIN_CFG = -20.0
MAX_CFG = 20.0
MIN_APG = 0.0
MAX_APG = 1.0
MAX_SEED = 2**31 - 1
MAX_INIT_AUDIO_BYTES = 16 * 1024 * 1024
MAX_GENERATE_METADATA_BYTES = 64 * 1024

# Measured small-DiT generation is ~1.5 s; the margin covers a cold
# filesystem cache and slower machines, not a first-ever weight download
# (see SETUP_HINT).
TIMEOUT_SECONDS = 120

SETUP_HINT = (
    "sa3_mlx checkout not found - install Stable Audio 3 from the app's settings "
    "drawer (the model manager), or point SA3_MLX_HOME at an existing checkout"
)


def timeout_for(seconds: float) -> float:
    """Deadline for one generation, scaled to the requested length.

    The published medium benchmark is ~15 s wall for a 2-minute track on
    M4-Pro-class hardware, so a second of deadline per second of audio is
    ~8x slack on top of the flat base — a wedge kill-switch, not a UX
    promise (ADR-0013)."""
    return TIMEOUT_SECONDS + seconds


_generation_lock = asyncio.Semaphore(1)


class GenerationUnavailable(Exception):
    """No usable sa3_mlx checkout on this machine."""


class GenerationFailed(Exception):
    """The CLI ran and did not produce a WAV."""


# Canonical SA3 install states, shared verbatim with the Rust `model_status`
# and the model-manager UI (issue #43): the readiness contract is one of these.
STATE_MISSING = "missing"
STATE_VENV_MISSING = "venv_missing"
STATE_NOT_WARMED = "not_warmed"
STATE_READY = "ready"

WARMED_STAMP = ".lsdj-warmed"


def _checkout_candidates(env: dict, home: pathlib.Path) -> list[pathlib.Path]:
    """Checkout roots to probe, in order. $SA3_MLX_HOME wins (pointing at the
    checkout root); otherwise the app-owned data dir, where the in-app installer
    puts the checkout. Mirrors the Rust `models::sa3_candidates`."""
    candidates = []
    override = env.get("SA3_MLX_HOME", "")
    if override:
        candidates.append(pathlib.Path(override).expanduser())
    candidates.append(
        home / "Library" / "Application Support" / "LSDJai" / "stable-audio-3"
    )
    return candidates


def resolve_mlx_dir(
    env: dict | None = None, home: pathlib.Path | None = None
) -> pathlib.Path | None:
    """First checkout whose optimized/mlx has a venv and the CLI script."""
    env = os.environ if env is None else env
    home = pathlib.Path.home() if home is None else home
    for checkout in _checkout_candidates(env, home):
        mlx_dir = checkout / "optimized" / "mlx"
        python = mlx_dir / ".venv" / "bin" / "python"
        script = mlx_dir / "scripts" / "sa3_mlx.py"
        if python.is_file() and script.is_file():
            return mlx_dir
    return None


def readiness(env: dict | None = None, home: pathlib.Path | None = None) -> dict:
    """The SA3 install state for the model manager (issue #43).

    Walks the same candidates as `resolve_mlx_dir` and classifies the first
    checkout that has an `optimized/mlx` dir:

      - ``missing``       no checkout with an ``optimized/mlx`` dir
      - ``venv_missing``  checkout present, but no ``.venv``/CLI script
      - ``not_warmed``    venv present, but the ``.lsdj-warmed`` stamp is absent
      - ``ready``         venv present and warmed

    Returns ``{"state", "checkout", "mlx_dir"}`` (paths are str or None). The
    Rust `model_status` mirrors this exact logic and these exact identifiers.
    """
    env = os.environ if env is None else env
    home = pathlib.Path.home() if home is None else home

    first_with_mlx: tuple[pathlib.Path, pathlib.Path] | None = None
    for checkout in _checkout_candidates(env, home):
        mlx_dir = checkout / "optimized" / "mlx"
        if not mlx_dir.is_dir():
            continue
        if first_with_mlx is None:
            first_with_mlx = (checkout, mlx_dir)
        python = mlx_dir / ".venv" / "bin" / "python"
        script = mlx_dir / "scripts" / "sa3_mlx.py"
        if not (python.is_file() and script.is_file()):
            continue
        warmed = (mlx_dir / WARMED_STAMP).is_file()
        return {
            "state": STATE_READY if warmed else STATE_NOT_WARMED,
            "checkout": str(checkout),
            "mlx_dir": str(mlx_dir),
        }

    if first_with_mlx is not None:
        checkout, mlx_dir = first_with_mlx
        return {
            "state": STATE_VENV_MISSING,
            "checkout": str(checkout),
            "mlx_dir": str(mlx_dir),
        }
    return {"state": STATE_MISSING, "checkout": None, "mlx_dir": None}


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
    lora_dirs: Sequence[str] | None = None,
    lora_strengths: Sequence[float] | None = None,
) -> bytes:
    """Run one generation and return the WAV bytes.

    Raises GenerationUnavailable when no checkout resolves and
    GenerationFailed when the CLI errors, times out, or writes nothing.
    Inputs are assumed validated at the trust boundary (controller).
    """
    mlx_dir = resolve_mlx_dir()
    if mlx_dir is None:
        raise GenerationUnavailable(SETUP_HINT)
    async with _generation_lock:
        with tempfile.TemporaryDirectory(prefix="sa3-") as tmp:
            out_path = pathlib.Path(tmp) / "out.wav"
            argv = [
                str(mlx_dir / ".venv" / "bin" / "python"),
                str(mlx_dir / "scripts" / "sa3_mlx.py"),
                "--prompt",
                prompt,
                "--dit",
                KINDS[kind],
                "--decoder",
                DECODERS[kind],
                "--seconds",
                f"{seconds:g}",
                "--steps",
                str(SAMPLER_STEPS),
                "--out",
                str(out_path),
            ]
            if init_audio is not None:
                init_path = pathlib.Path(tmp) / "init.wav"
                init_path.write_bytes(init_audio)
                argv.extend(("--init-audio", str(init_path)))
            if init_noise_level is not None:
                argv.extend(("--init-noise-level", f"{init_noise_level:g}"))
            if inpaint_range is not None:
                start, end = inpaint_range
                argv.extend(("--inpaint-range", f"{start:g},{end:g}"))
            if negative_prompt is not None:
                argv.extend(("--negative-prompt", negative_prompt))
            if cfg is not None:
                argv.extend(("--cfg", f"{cfg:g}"))
            if apg is not None:
                argv.extend(("--apg", f"{apg:g}"))
            if seed is not None:
                argv.extend(("--seed", str(seed)))
            if lora_dirs:
                # One --lora group per adapter (upstream's PR #57/#65 syntax):
                # the directory plus its strength=S option. The CLI resolves
                # the .safetensors inside and merges all deltas at DiT load.
                for index, lora_dir in enumerate(lora_dirs):
                    argv.extend(("--lora", lora_dir))
                    if lora_strengths is not None:
                        argv.append(f"strength={lora_strengths[index]:g}")
            process = await asyncio.create_subprocess_exec(
                *argv,
                cwd=mlx_dir,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.STDOUT,
            )
            timeout = timeout_for(seconds)
            try:
                output, _ = await asyncio.wait_for(
                    process.communicate(), timeout=timeout
                )
            except TimeoutError:
                process.kill()
                await process.wait()
                raise GenerationFailed(
                    f"generation timed out after {timeout:g}s"
                ) from None
            if process.returncode != 0 or not out_path.is_file():
                # The CLI's last lines name the problem; progress bars and
                # ANSI noise live further up.
                tail = output.decode(errors="replace").strip()[-500:]
                raise GenerationFailed(tail or "sa3_mlx produced no output")
            return out_path.read_bytes()
