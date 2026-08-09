# PyTorch MRT2 two-deck spike harness

This directory is an isolated issue #109 fixture. It does not import or change
the production backend. It benchmarks the immutable upstream Transformers
snapshots in two process topologies, at 25 frames (~1 second) and 5 frames
(~200 ms), with a 1.5 second playback-prebuffer simulation.

The harness records cold start, warm-up, per-deck p50/p95/p99 generation
latency, output duration, an underrun proxy, RSS, PyTorch CUDA allocation, and
`nvidia-smi` VRAM/driver/temperature rows. JSON marks dry runs as `synthetic`;
they are never qualification evidence.

## CI/dry run

From the repository root:

```sh
python3 -m unittest discover -s spike/mrt2_pytorch/tests -v
python3 -m spike.mrt2_pytorch.harness \
  --backend dry-run --duration-seconds 2 \
  --output /tmp/mrt2-dry-run.json
```

## NVIDIA run

Use Python 3.12 and install the direct candidate pins:

```sh
python -m venv .venv-mrt2-spike
.venv-mrt2-spike/bin/python -m pip install -r spike/mrt2_pytorch/requirements-candidate.txt
```

On Windows, use `.venv-mrt2-spike\Scripts\python.exe` for the same commands.
Prefetch the immutable snapshots while online:

```sh
hf download magenta-community/magenta-realtime-2-small \
  --revision 7037d99551c84ac5c6afb7f1a5e58c65e7233dbb
hf download magenta-community/magenta-rt-musiccoca-torch \
  --revision 236c488e38aa98643805514996934d705668298b
```

Then disconnect or set `HF_HUB_OFFLINE=1` and run the complete matrix:

```sh
python -m spike.mrt2_pytorch.harness \
  --backend upstream \
  --model mrt2_small \
  --topologies shared-worker,per-deck \
  --frames 25,5 \
  --duration-seconds 600 \
  --prompt-change-seconds 60 \
  --output mrt2-small-eager.json
```

Repeat with `--acceleration torch-compile`; do not treat a runtime compiler as
a distributable solution until Windows packaging proves it needs no developer
toolchain. The default enables classifier-free guidance because that matches
LSDJ's `.mlxfn` path. `--token-cfg` intentionally measures upstream's cheaper,
non-parity conditioning-token path.

The model adapter always uses exact revisions and `local_files_only=True`.
Any cache miss therefore fails clearly instead of silently downloading a
different asset. See `provenance.json` and
`docs/issue-109-hardware-checklist.md` before running qualification.
