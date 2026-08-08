"""Deterministic, model-free Stable Audio WAV boundary tests."""

import io
import struct
import wave

import pytest

from lsdj import sa3_audio


def pcm_wav(
    raw: bytes,
    *,
    sample_rate: int,
    channels: int,
    sample_width: int,
) -> bytes:
    output = io.BytesIO()
    with wave.open(output, "wb") as target:
        target.setnchannels(channels)
        target.setsampwidth(sample_width)
        target.setframerate(sample_rate)
        target.writeframes(raw)
    return output.getvalue()


def test_canonical_wav_is_validated_without_reencoding():
    source = pcm_wav(
        b"\0" * 100 * 4,
        sample_rate=44_100,
        channels=2,
        sample_width=2,
    )
    normalized = sa3_audio.normalize_wav(source)
    assert normalized.wav is source
    assert normalized.frames == 100


def test_mono_8khz_pcm8_is_resampled_and_duplicated_to_stereo():
    source = pcm_wav(
        bytes([128]) * 8_000,
        sample_rate=8_000,
        channels=1,
        sample_width=1,
    )
    normalized = sa3_audio.normalize_wav(source)
    with wave.open(io.BytesIO(normalized.wav), "rb") as result:
        assert result.getframerate() == 44_100
        assert result.getnchannels() == 2
        assert result.getsampwidth() == 2
        assert result.getnframes() == 44_100
        assert result.readframes(1) == b"\0\0\0\0"


def test_multichannel_input_keeps_the_first_two_channels():
    frame = struct.pack("<hhh", 1000, -1000, 30_000)
    normalized = sa3_audio.normalize_wav(
        pcm_wav(frame, sample_rate=44_100, channels=3, sample_width=2)
    )
    with wave.open(io.BytesIO(normalized.wav), "rb") as result:
        assert struct.unpack("<hh", result.readframes(1)) == (1000, -1000)


def test_pcm24_conversion_is_deterministic():
    negative_half = (-(1 << 22)) & 0xFFFFFF
    positive_half = 1 << 22
    raw = negative_half.to_bytes(3, "little") + positive_half.to_bytes(3, "little")
    source = pcm_wav(raw, sample_rate=44_100, channels=2, sample_width=3)
    first = sa3_audio.normalize_wav(source).wav
    second = sa3_audio.normalize_wav(source).wav
    assert first == second
    with wave.open(io.BytesIO(first), "rb") as result:
        left, right = struct.unpack("<hh", result.readframes(1))
    assert left == -16_384
    assert right in {16_383, 16_384}


@pytest.mark.parametrize(
    "data",
    [
        b"",
        b"not a wave",
        pcm_wav(b"", sample_rate=44_100, channels=2, sample_width=2),
        pcm_wav(b"\0" * 16, sample_rate=44_100, channels=2, sample_width=2)[:-2],
    ],
)
def test_corrupt_empty_and_truncated_input_is_rejected(data):
    with pytest.raises(sa3_audio.AudioFormatError):
        sa3_audio.normalize_wav(data)


def test_declared_input_longer_than_medium_limit_is_rejected_before_allocation():
    frames = round((sa3_audio.MAX_INPUT_SECONDS + 1) * 8_000)
    data_bytes = frames * 2
    header = b"RIFF" + (36 + data_bytes).to_bytes(4, "little") + b"WAVEfmt "
    header += (16).to_bytes(4, "little")
    header += struct.pack("<HHIIHH", 1, 1, 8_000, 16_000, 2, 16)
    header += b"data" + data_bytes.to_bytes(4, "little")
    with pytest.raises(sa3_audio.AudioFormatError, match="at most 380"):
        sa3_audio.normalize_wav(header)


def test_output_validation_rejects_corruption_and_wrong_duration():
    valid = pcm_wav(
        b"\0" * 44_100 * 4,
        sample_rate=44_100,
        channels=2,
        sample_width=2,
    )
    assert sa3_audio.validate_output_wav(valid, 1.0) == valid
    with pytest.raises(sa3_audio.AudioFormatError, match="expected"):
        sa3_audio.validate_output_wav(valid, 0.5)
    with pytest.raises(sa3_audio.AudioFormatError, match="corrupt"):
        sa3_audio.validate_output_wav(b"RIFFbad", 1.0)


def test_long_duration_frame_contract_is_exact_without_allocating_a_fixture():
    assert round(380.0 * sa3_audio.SAMPLE_RATE) == 16_758_000
