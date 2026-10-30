"""Find unintended silence in a master recording: the loom's "never silent" metric.

    uv run --project backend python scripts/silence_gaps.py <wav> \
        [--threshold -60] [--min-ms 150]

A gap is a span where every channel stays under the threshold (dBFS, peak per
10 ms block) for longer than --min-ms. Prints `start  end  length` per gap and
`N gaps, longest X ms`; exits 1 when any gap is found, 0 when none, 2 when the
file cannot be read. Reads 16/24/32-bit PCM and 32/64-bit float WAV, plain or
extensible (the `● rec` recorder writes 48 kHz stereo 16-bit PCM).

Tests: `cd backend && uv run pytest ../scripts` (part of `just test`).
"""

import argparse
import os
import struct
import sys

import numpy as np

PCM, FLOAT, EXTENSIBLE = 1, 3, 0xFFFE
# (format tag, bits) -> (dtype, full scale). 24-bit is widened to int32 below.
FORMATS = {
    (PCM, 16): ("<i2", 2**15),
    (PCM, 24): ("<i4", 2**31),
    (PCM, 32): ("<i4", 2**31),
    (FLOAT, 32): ("<f4", 1.0),
    (FLOAT, 64): ("<f8", 1.0),
}


def read_wav(path):
    """The interleaved samples, channel count, sample rate and full scale."""
    with open(path, "rb") as f:
        head = f.read(12)
        if len(head) < 12 or head[:4] != b"RIFF" or head[8:] != b"WAVE":
            raise ValueError("not a RIFF/WAVE file")
        fmt = None
        while True:
            chunk = f.read(8)
            if len(chunk) < 8:
                raise ValueError("no data chunk")
            cid, size = chunk[:4], struct.unpack("<I", chunk[4:])[0]
            if cid == b"data":
                break
            body = f.read(size + size % 2)
            if cid == b"fmt " and len(body) >= 16:
                tag, channels, rate, _, _, bits = struct.unpack("<HHIIHH", body[:16])
                if tag == EXTENSIBLE and len(body) >= 26:
                    tag = struct.unpack("<H", body[24:26])[0]  # the SubFormat GUID
                fmt = tag, channels, rate, bits
        if fmt is None:
            raise ValueError("no fmt chunk before the data")
        tag, channels, rate, bits = fmt
        if (tag, bits) not in FORMATS or channels < 1 or rate < 100:
            raise ValueError(f"unsupported format {tag}/{bits}-bit/{channels} ch")
        # A recording cut off by a crash keeps the placeholder size (0): the
        # bytes present win.
        available = os.fstat(f.fileno()).st_size - f.tell()
        nbytes = size if 0 < size <= available else available
        frames = nbytes // (channels * bits // 8)
        dtype, full = FORMATS[tag, bits]
        if bits == 24:
            raw = np.fromfile(f, np.uint8, frames * channels * 3).reshape(-1, 3)
            wide = np.zeros((len(raw), 4), np.uint8)
            wide[:, 1:] = raw  # the 24-bit value << 8, so full scale is 2**31
            samples = wide.view("<i4").ravel()
        else:
            samples = np.fromfile(f, dtype, frames * channels)
    return samples, channels, rate, full


def find_gaps(samples, channels, rate, full, threshold_db, min_ms):
    """(start, end) seconds of each quiet span longer than `min_ms`."""
    block = rate // 100
    nblocks = len(samples) // (block * channels)
    # ponytail: a trailing partial block (< 10 ms) is not scanned.
    x = samples[: nblocks * block * channels].reshape(nblocks, block * channels)
    # max and -min separately: abs() of the most negative int overflows.
    peak = np.maximum(x.max(axis=1).astype(float), -x.min(axis=1).astype(float))
    quiet = np.concatenate(([0], peak < full * 10 ** (threshold_db / 20), [0]))
    edges = np.flatnonzero(np.diff(quiet.astype(np.int8)))
    spans = [
        (a * block / rate, b * block / rate) for a, b in zip(edges[::2], edges[1::2])
    ]
    return [(a, b) for a, b in spans if (b - a) * 1000 > min_ms]


def clock(seconds):
    minutes, rest = divmod(seconds, 60)
    return f"{int(minutes)}:{rest:06.3f}"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("wav")
    parser.add_argument("--threshold", type=float, default=-60, help="dBFS")
    parser.add_argument("--min-ms", type=float, default=150)
    args = parser.parse_args(argv)
    try:
        samples, channels, rate, full = read_wav(args.wav)
    except (OSError, ValueError) as error:
        print(f"silence_gaps: {args.wav}: {error}", file=sys.stderr)
        return 2
    gaps = find_gaps(samples, channels, rate, full, args.threshold, args.min_ms)
    for a, b in gaps:
        print(f"{clock(a)}  {clock(b)}  {(b - a) * 1000:.0f} ms")
    longest = max(((b - a) * 1000 for a, b in gaps), default=0)
    print(f"{len(gaps)} gaps, longest {longest:.0f} ms")
    return 1 if gaps else 0


if __name__ == "__main__":
    sys.exit(main())
