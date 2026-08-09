"""Bounded WAV normalization and validation for Stable Audio 3.

Only uncompressed integer PCM WAV is accepted at the HTTP boundary.  LSDJ
converts sample width, channel layout, and sample rate itself, so neither SA3
backend can fall through to its optional system-``ffmpeg`` path.
"""

from __future__ import annotations

import io
import math
import struct
import threading
import wave
from collections.abc import Callable
from dataclasses import dataclass

SAMPLE_RATE = 44_100
CHANNELS = 2
SAMPLE_WIDTH = 2
MAX_INPUT_SECONDS = 380.0
MAX_DECODED_PCM_BYTES = 16 * 1024 * 1024
MAX_NORMALIZED_FRAMES = round(MAX_INPUT_SECONDS * SAMPLE_RATE)
NORMALIZATION_CHECK_FRAMES = 16_384


class AudioFormatError(ValueError):
    """The WAV is corrupt or uses an encoding LSDJ does not accept."""


class AudioNormalizationCancelled(Exception):
    """The caller cancelled bounded input normalization."""


@dataclass(frozen=True)
class NormalizedAudio:
    wav: bytes
    frames: int
    seconds: float


def _decode_sample(raw: bytes, width: int) -> float:
    if width == 1:
        return (raw[0] - 128) / 128.0
    if width == 2:
        return int.from_bytes(raw, "little", signed=True) / 32768.0
    if width == 3:
        value = int.from_bytes(raw, "little", signed=False)
        if value & 0x800000:
            value -= 1 << 24
        return value / 8388608.0
    if width == 4:
        return int.from_bytes(raw, "little", signed=True) / 2147483648.0
    raise AudioFormatError("PCM sample width must be 8, 16, 24, or 32 bits")


def _stereo_frame(
    raw: bytes, index: int, channels: int, width: int
) -> tuple[float, float]:
    start = index * channels * width
    left = _decode_sample(raw[start : start + width], width)
    if channels == 1:
        return left, left
    right_start = start + width
    return left, _decode_sample(raw[right_start : right_start + width], width)


def _pcm16(value: float) -> int:
    value = min(1.0, max(-1.0, value))
    if value <= -1.0:
        return -32768
    return min(32767, max(-32768, round(value * 32767.0)))


def normalize_wav(
    data: bytes,
    *,
    cancel_event: threading.Event | None = None,
    on_progress: Callable[[int, int], None] | None = None,
) -> NormalizedAudio:
    """Return canonical 44.1 kHz stereo PCM16 WAV bytes.

    The conversion is deterministic and bounded by ``MAX_INPUT_SECONDS``.
    Multichannel input follows the official TFLite runtime's semantics and
    retains the first two channels; mono is duplicated.
    """
    try:
        with wave.open(io.BytesIO(data), "rb") as source:
            channels = source.getnchannels()
            width = source.getsampwidth()
            rate = source.getframerate()
            frames = source.getnframes()
            compression = source.getcomptype()
            if compression != "NONE":
                raise AudioFormatError("WAV must use uncompressed integer PCM")
            if channels < 1 or channels > 32:
                raise AudioFormatError("WAV must have between 1 and 32 channels")
            if width not in (1, 2, 3, 4):
                raise AudioFormatError("PCM sample width must be 8, 16, 24, or 32 bits")
            if rate < 8_000 or rate > 384_000:
                raise AudioFormatError(
                    "WAV sample rate must be between 8 kHz and 384 kHz"
                )
            if frames < 1:
                raise AudioFormatError("WAV must contain audio frames")
            expected_bytes = frames * channels * width
            if expected_bytes > MAX_DECODED_PCM_BYTES:
                raise AudioFormatError(
                    f"decoded PCM must be at most {MAX_DECODED_PCM_BYTES} bytes"
                )
            seconds = frames / rate
            if not math.isfinite(seconds) or seconds > MAX_INPUT_SECONDS:
                raise AudioFormatError(
                    f"WAV must be at most {MAX_INPUT_SECONDS:g} seconds"
                )
            raw = source.readframes(frames)
    except AudioFormatError:
        raise
    except (EOFError, wave.Error, OverflowError, struct.error):
        raise AudioFormatError("init audio must be a valid PCM WAV file") from None

    if len(raw) != expected_bytes:
        raise AudioFormatError("WAV sample data is truncated")
    if cancel_event is not None and cancel_event.is_set():
        raise AudioNormalizationCancelled
    if channels == CHANNELS and width == SAMPLE_WIDTH and rate == SAMPLE_RATE:
        if on_progress is not None:
            on_progress(frames, frames)
        return NormalizedAudio(wav=data, frames=frames, seconds=seconds)

    target_frames = max(1, round(frames * SAMPLE_RATE / rate))
    if target_frames > MAX_NORMALIZED_FRAMES:
        raise AudioFormatError(
            f"normalized WAV must be at most {MAX_NORMALIZED_FRAMES} frames"
        )
    pcm = bytearray(target_frames * CHANNELS * SAMPLE_WIDTH)
    source_per_target = rate / SAMPLE_RATE
    last_source_frame = frames - 1
    for index in range(target_frames):
        if index % NORMALIZATION_CHECK_FRAMES == 0:
            if cancel_event is not None and cancel_event.is_set():
                raise AudioNormalizationCancelled
            if on_progress is not None:
                on_progress(index, target_frames)
        position = index * source_per_target
        lower = min(int(position), last_source_frame)
        upper = min(lower + 1, last_source_frame)
        fraction = position - lower
        left_lower, right_lower = _stereo_frame(raw, lower, channels, width)
        left_upper, right_upper = _stereo_frame(raw, upper, channels, width)
        left_sample = left_lower + (left_upper - left_lower) * fraction
        right_sample = right_lower + (right_upper - right_lower) * fraction
        offset = index * 4
        struct.pack_into("<hh", pcm, offset, _pcm16(left_sample), _pcm16(right_sample))

    if cancel_event is not None and cancel_event.is_set():
        raise AudioNormalizationCancelled
    if on_progress is not None:
        on_progress(target_frames, target_frames)

    output = io.BytesIO()
    with wave.open(output, "wb") as target:
        target.setnchannels(CHANNELS)
        target.setsampwidth(SAMPLE_WIDTH)
        target.setframerate(SAMPLE_RATE)
        target.writeframes(pcm)
    return NormalizedAudio(
        wav=output.getvalue(), frames=target_frames, seconds=target_frames / SAMPLE_RATE
    )


def inspect_canonical_wav(data: bytes) -> NormalizedAudio:
    """Validate a canonical backend WAV without copying its entire payload."""
    try:
        with wave.open(io.BytesIO(data), "rb") as source:
            channels = source.getnchannels()
            width = source.getsampwidth()
            rate = source.getframerate()
            frames = source.getnframes()
            compression = source.getcomptype()
            if (
                compression != "NONE"
                or channels != CHANNELS
                or width != SAMPLE_WIDTH
                or rate != SAMPLE_RATE
                or frames < 1
            ):
                raise AudioFormatError(
                    "backend output must be non-empty 44.1 kHz stereo PCM16 WAV"
                )
            read_frames = 0
            while read_frames < frames:
                chunk_frames = min(65_536, frames - read_frames)
                chunk = source.readframes(chunk_frames)
                if len(chunk) != chunk_frames * CHANNELS * SAMPLE_WIDTH:
                    raise AudioFormatError("backend WAV payload is truncated")
                read_frames += chunk_frames
    except AudioFormatError:
        raise
    except (EOFError, wave.Error, OverflowError):
        raise AudioFormatError("backend produced a corrupt WAV") from None
    return NormalizedAudio(wav=data, frames=frames, seconds=frames / SAMPLE_RATE)


def validate_output_wav(data: bytes, seconds: float) -> bytes:
    output = inspect_canonical_wav(data)
    expected_frames = round(seconds * SAMPLE_RATE)
    if output.frames != expected_frames:
        raise AudioFormatError(
            f"backend produced {output.frames} frames; expected {expected_frames}"
        )
    return data
