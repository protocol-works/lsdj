"""Verify issue #54's SA3 controls through the real FastAPI route.

Run from backend/ with a native SA3 output (44.1 kHz PCM16 mono/stereo):

    uv run python -u scripts/verify_sa3_surface.py \
      "$HOME/Documents/LSDJ/generated_samples/Cat.wav"

The verifier writes five clips to /tmp/lsdj-issue54 by default. It exercises
JSON and multipart parsing, validation, the generation lock, the pinned CLI,
and the WAV response rather than calling `sa3.generate` directly.
"""

import argparse
import hashlib
import io
import json
import math
import wave
from pathlib import Path

import numpy as np
from fastapi.testclient import TestClient

from lsdj import controller, sa3

DEFAULT_OUT_DIR = Path("/tmp/lsdj-issue54")
DEFAULT_PROMPT = "a close variation of the source audio"
DEFAULT_NEGATIVE_PROMPT = "harsh distortion, clipping, vocals"
DEFAULT_SEED = 54_054
DIFFERENCE_FLOOR = 1e-5
INPAINT_CONCENTRATION_MIN = 1.1
CODEC_GUARD_SECONDS = 0.25


def read_pcm16(data: bytes) -> tuple[int, np.ndarray]:
    with wave.open(io.BytesIO(data), "rb") as source:
        if source.getcomptype() != "NONE" or source.getsampwidth() != 2:
            raise ValueError("expected uncompressed PCM16 WAV")
        sample_rate = source.getframerate()
        channels = source.getnchannels()
        raw = source.readframes(source.getnframes())
    samples = np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
    return sample_rate, samples.reshape(-1, channels)


def stereo(samples: np.ndarray) -> np.ndarray:
    if samples.shape[1] == 1:
        return np.repeat(samples, 2, axis=1)
    return samples[:, :2]


def aligned(left: bytes, right: bytes) -> tuple[int, np.ndarray, np.ndarray]:
    left_rate, left_pcm = read_pcm16(left)
    right_rate, right_pcm = read_pcm16(right)
    if left_rate != right_rate:
        raise ValueError(f"sample-rate mismatch: {left_rate} != {right_rate}")
    frames = min(len(left_pcm), len(right_pcm))
    return left_rate, stereo(left_pcm[:frames]), stereo(right_pcm[:frames])


def rms(samples: np.ndarray) -> float:
    return float(np.sqrt(np.mean(np.square(samples, dtype=np.float64))))


def difference_rms(left: bytes, right: bytes) -> float:
    _, left_pcm, right_pcm = aligned(left, right)
    return rms(left_pcm - right_pcm)


def inpaint_differences(
    source: bytes, result: bytes, start: float, end: float
) -> tuple[float, float]:
    sample_rate, source_pcm, result_pcm = aligned(source, result)
    guard = round(CODEC_GUARD_SECONDS * sample_rate)
    start_frame = round(start * sample_rate)
    end_frame = round(end * sample_rate)
    inside_start = min(end_frame, start_frame + guard)
    inside_end = max(inside_start, end_frame - guard)
    outside_before = (
        source_pcm[: max(0, start_frame - guard)]
        - result_pcm[: max(0, start_frame - guard)]
    )
    outside_after = (
        source_pcm[min(len(source_pcm), end_frame + guard) :]
        - result_pcm[min(len(result_pcm), end_frame + guard) :]
    )
    inside = source_pcm[inside_start:inside_end] - result_pcm[inside_start:inside_end]
    outside_parts = [part for part in (outside_before, outside_after) if part.size]
    if not inside.size or not outside_parts:
        raise ValueError("inpaint range leaves no measurable inside/outside regions")
    outside = np.concatenate(outside_parts)
    return rms(inside), rms(outside)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def generate(
    client: TestClient, metadata: dict, init_audio: bytes | None = None
) -> bytes:
    if init_audio is None:
        response = client.post("/api/generate", json=metadata)
    else:
        response = client.post(
            "/api/generate",
            files=[
                ("request", (None, json.dumps(metadata))),
                ("init_audio", ("source.wav", init_audio, "audio/wav")),
            ],
        )
    if response.status_code != 200:
        try:
            detail = response.json().get("detail", response.text)
        except (json.JSONDecodeError, AttributeError):
            detail = response.text
        raise RuntimeError(f"generation failed ({response.status_code}): {detail}")
    if response.headers.get("content-type") != "audio/wav":
        raise RuntimeError("generation response was not audio/wav")
    return response.content


def parse_range(value: str) -> tuple[float, float]:
    try:
        start_text, end_text = value.split(",", 1)
        start, end = float(start_text), float(end_text)
    except ValueError:
        raise argparse.ArgumentTypeError("range must be START,END") from None
    if not math.isfinite(start) or not math.isfinite(end) or not 0 <= start < end:
        raise argparse.ArgumentTypeError("range must satisfy 0 <= START < END")
    return start, end


def source_duration(data: bytes) -> float:
    with wave.open(io.BytesIO(data), "rb") as source:
        return source.getnframes() / source.getframerate()


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Verify SA3 audio-to-audio, inpaint, CFG/APG, and seed via HTTP"
    )
    parser.add_argument("init_audio", type=Path, help="44.1 kHz PCM16 source WAV")
    parser.add_argument("--prompt", default=DEFAULT_PROMPT)
    parser.add_argument("--negative-prompt", default=DEFAULT_NEGATIVE_PROMPT)
    parser.add_argument("--kind", choices=sorted(sa3.KINDS), default="sfx")
    parser.add_argument("--seconds", type=float)
    parser.add_argument("--inpaint-range", type=parse_range)
    parser.add_argument("--noise", type=float, default=0.6)
    parser.add_argument("--cfg", type=float, default=4.0)
    parser.add_argument("--apg", type=float, default=1.0)
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    args = parser.parse_args()

    source = args.init_audio.expanduser().read_bytes()
    duration = args.seconds if args.seconds is not None else source_duration(source)
    if args.inpaint_range is None:
        start, end = duration * 0.25, duration * 0.75
    else:
        start, end = args.inpaint_range
    if end > duration:
        parser.error("inpaint range must end at or before --seconds/source duration")

    args.out_dir.mkdir(parents=True, exist_ok=True)
    common = {
        "prompt": args.prompt,
        "seconds": duration,
        "kind": args.kind,
        "seed": args.seed,
    }
    with TestClient(controller.app) as client:
        baseline = generate(client, common)
        variation = generate(
            client,
            {**common, "init_noise_level": args.noise},
            source,
        )
        inpaint = generate(
            client,
            {
                **common,
                "init_noise_level": args.noise,
                "inpaint_range": [start, end],
            },
            source,
        )
        negative = generate(
            client,
            {
                **common,
                "negative_prompt": args.negative_prompt,
                "cfg": args.cfg,
                "apg": args.apg,
            },
        )
        seed_repeat = generate(client, common)

    artifacts = {
        "text-baseline.wav": baseline,
        "audio-variation.wav": variation,
        "inpaint.wav": inpaint,
        "negative-cfg.wav": negative,
        "seed-repeat.wav": seed_repeat,
    }
    for name, data in artifacts.items():
        (args.out_dir / name).write_bytes(data)
        print(f"{name:<22} sha256 {sha256(data)[:16]}")

    variation_source = difference_rms(source, variation)
    variation_baseline = difference_rms(baseline, variation)
    negative_baseline = difference_rms(baseline, negative)
    inside, outside = inpaint_differences(source, inpaint, start, end)
    concentration = inside / outside if outside else math.inf
    print()
    print(f"variation vs source RMS:   {variation_source:.8f}")
    print(f"variation vs baseline RMS: {variation_baseline:.8f}")
    print(f"negative vs baseline RMS:  {negative_baseline:.8f}")
    print(f"inpaint inside RMS:        {inside:.8f}")
    print(f"inpaint outside RMS:       {outside:.8f}")
    print(f"inpaint concentration:     {concentration:.3f}x")

    failures = []
    if baseline != seed_repeat:
        failures.append("fixed-seed repeats are not byte-identical")
    if variation_source <= DIFFERENCE_FLOOR:
        failures.append("audio-to-audio output does not differ from the source")
    if variation_baseline <= DIFFERENCE_FLOOR:
        failures.append("audio-to-audio output does not differ from text-only")
    if negative_baseline <= DIFFERENCE_FLOOR:
        failures.append("negative CFG output does not differ from text-only")
    if inside <= DIFFERENCE_FLOOR:
        failures.append("inpaint did not alter the requested range")
    if concentration < INPAINT_CONCENTRATION_MIN:
        failures.append(
            "inpaint changes are not concentrated inside the requested range "
            f"({concentration:.3f}x < {INPAINT_CONCENTRATION_MIN:.3f}x)"
        )
    if failures:
        raise SystemExit("FAIL\n- " + "\n- ".join(failures))
    print(f"\nPASS — artifacts written to {args.out_dir}")


if __name__ == "__main__":
    main()
