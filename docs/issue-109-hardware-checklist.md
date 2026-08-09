# Issue #109 Linux/Windows NVIDIA qualification checklist

Run every item once on representative Ubuntu 22.04+ and Windows 11 systems.
Attach the JSON and logs to #109; do not summarize a failing/missing run as a
pass.

## Host record

- [ ] Record OS/build, CPU, physical RAM, GPU model, VRAM, NVIDIA driver, power
      mode, and whether the GPU drives a display.
- [ ] Save `nvidia-smi` output before and after each run.
- [ ] Record Python, PyTorch, Transformers, CUDA runtime, cuDNN, and exact
      `provenance.json` revisions.
- [ ] Confirm `torch.cuda.is_available()` and the reported compute capability.
- [ ] Start from a clean runtime with no system Python, Git, shell, CUDA toolkit,
      or compiler dependency in the packaged execution path.

## Acquisition/offline proof

- [ ] Acquire the small model at `7037d99551c84ac5c6afb7f1a5e58c65e7233dbb`.
- [ ] Acquire MusicCoCa at `236c488e38aa98643805514996934d705668298b`.
- [ ] Verify hashes, disconnect network (or set `HF_HUB_OFFLINE=1`), and confirm
      the harness starts. A cache miss must fail clearly without a download.
- [ ] Repeat for base `92087988d05d0fe38b11f021f0b0d00a75afb86b`
      only if base is a proposed supported model.

## Required matrix

For every proposed acceleration mode, run:

```text
shared-worker × 25 frames × 600 seconds
shared-worker ×  5 frames × 600 seconds
per-deck     × 25 frames × 600 seconds
per-deck     ×  5 frames × 600 seconds
```

- [ ] Run the dry adapter first and retain it separately as synthetic evidence.
- [ ] Run `python -m spike.mrt2_pytorch.harness --backend upstream ...` for the
      full matrix with parity guidance (the default).
- [ ] Repeat with a live app build feeding the native engine and capture its
      engine-reported underrun counter; the harness proxy is not a substitute.
- [ ] Confirm both decks prime, generate/play continuously, and report zero
      native engine underruns.
- [ ] Confirm p50/p95/p99/max latency, generated-audio/wall ratio, RSS peak,
      PyTorch CUDA peak, process VRAM peak, driver, temperature, P-state, and
      power rows are present.
- [ ] Inspect thermal clocks/temperature across the full ten minutes; record any
      throttling or laptop power-mode dependency.

## Controls and continuity

- [ ] At the scheduled change, verify weighted prompt, temperature, top-k,
      prompt CFG, note CFG, drum CFG, MIDI onset, and onset-to-sustain take effect
      without resetting continuation state.
- [ ] Listen/inspect boundaries in both chunk modes for gaps, repeats, channel
      swaps, clipping, or a sample-rate mismatch (required: float32, stereo,
      48 kHz).
- [ ] Run twice from a fresh state with the same seed and record determinism;
      confirm changing seed with retained state does not falsely claim reseeding.
- [ ] Exercise text and captured-audio style inputs.
- [ ] Switch small/base models and verify reset/readiness behavior.

## Supervision/topology

- [ ] Record cold start, warm-up, readiness, and clean shutdown time.
- [ ] Kill deck A in the per-deck topology; deck B must continue and the parent
      must report the failure.
- [ ] Kill the shared worker; both decks must stop/report failure coherently.
- [ ] Close the parent/app during generation and confirm no Python, helper,
      compiler, or GPU process remains.
- [ ] Force one malformed control payload and one generation exception; no
      silent hang or stale-playing state is allowed.

## Release gate

- [ ] Name the lowest GPU/VRAM/driver configuration that passed every required
      run on both operating systems.
- [ ] Confirm packaged execution requires no runtime compiler; otherwise mark the
      acceleration path no-go.
- [ ] #108 confirms code, derived-weight, MusicCoCa, and original-weight notices.
- [ ] Publish the topology recommendation and raw evidence on #109 before #110
      starts.
