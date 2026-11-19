"""Tests for silence_gaps.py: `cd backend && uv run pytest ../scripts`."""

import struct

import numpy as np
import pytest
from silence_gaps import FLOAT, PCM, main

RATE = 48000
QUIET = 1e-4  # -80 dBFS: hiss, not a hard zero


def tone(seconds, level=0.1):
    t = np.arange(int(seconds * RATE)) / RATE
    return level * np.sin(2 * np.pi * 440 * t)


def write_wav(path, x, tag=PCM, bits=16):
    """`x` is (frames, channels) in -1..1."""
    if tag == FLOAT:
        data = x.astype("<f4").tobytes()
    elif bits == 24:
        ints = np.round(x * (2**23 - 1)).astype("<i4")
        data = ints.view(np.uint8).reshape(-1, 4)[:, :3].tobytes()
    else:
        data = np.round(x * (2**15 - 1)).astype("<i2").tobytes()
    channels = x.shape[1]
    block = channels * bits // 8
    fmt = struct.pack("<HHIIHH", tag, channels, RATE, RATE * block, block, bits)
    path.write_bytes(
        b"RIFF"
        + struct.pack("<I", 4 + 8 + len(fmt) + 8 + len(data))
        + b"WAVE"
        + b"fmt "
        + struct.pack("<I", len(fmt))
        + fmt
        + b"data"
        + struct.pack("<I", len(data))
        + data
    )
    return str(path)


def two_gaps():
    """A 300 ms gap at 1.0 s and a 100 ms one at 2.0 s, in both channels."""
    mono = np.concatenate(
        [tone(1.0), tone(0.3, QUIET), tone(0.7), tone(0.1, QUIET), tone(0.9)]
    )
    return np.stack([mono, mono], axis=1)


@pytest.mark.parametrize(
    "tag, bits", [(PCM, 16), (PCM, 24), (FLOAT, 32)], ids=["pcm16", "pcm24", "float32"]
)
def test_reports_only_the_gap_longer_than_min_ms(tmp_path, capsys, tag, bits):
    wav = write_wav(tmp_path / "set.wav", two_gaps(), tag, bits)
    assert main([wav]) == 1
    assert capsys.readouterr().out.splitlines() == [
        "0:01.000  0:01.300  300 ms",
        "1 gaps, longest 300 ms",
    ]
    assert main([wav, "--min-ms", "50"]) == 1
    assert capsys.readouterr().out.splitlines()[-1] == "2 gaps, longest 300 ms"


def test_one_silent_channel_is_not_a_gap(tmp_path, capsys):
    music = tone(2.0)
    wav = write_wav(tmp_path / "left.wav", np.stack([music, 0 * music], axis=1))
    assert main([wav]) == 0
    assert capsys.readouterr().out == "0 gaps, longest 0 ms\n"


def test_unreadable_input_exits_2(tmp_path):
    not_wav = tmp_path / "notes.wav"
    not_wav.write_text("not audio")
    assert main([str(not_wav)]) == 2
    assert main([str(tmp_path / "missing.wav")]) == 2
