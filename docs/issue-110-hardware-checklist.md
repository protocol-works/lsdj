# Issue #110 — PyTorch MRT2 production qualification

This checklist is intentionally unchecked.  The implementation was developed
without Linux or Windows NVIDIA hardware; unit tests are not performance or
driver evidence.

## Immutable inputs

- [ ] Confirm the source, model, and processor revisions printed by
  `python -m lsdj.sidecar --runtime-info` match the release inventory.
- [x] Resolve the candidate Python/PyTorch/CUDA dependencies into separate,
  hash-locked Linux x86_64 and Windows x86_64 requirement sets. Clean-host
  installation remains unchecked below.
- [ ] Install each runtime through the atomic #107 installer on a clean host
  without system Python, Git, shell tools, a compiler, or CUDA toolkit.
- [ ] Disconnect networking and prove startup and prompt/audio embedding use
  only the verified app-owned snapshot cache.

## Required hosts and diagnostics

Record the app version, OS build, GPU, VRAM, NVIDIA driver, torch version, CUDA
runtime, model revision, processor revision, upstream source revision, and
acceleration mode from the worker's `ready.runtime` payload.

- [ ] Ubuntu 22.04+ on the proposed minimum NVIDIA GPU.
- [ ] Windows 11 x64 on the proposed minimum NVIDIA GPU.
- [ ] Unsupported/no GPU fails at startup and never starts CPU inference.
- [ ] An insufficient-VRAM case fails cleanly without leaving a child process.

Set `LSDJ_ALLOW_UNVERIFIED_MRT2_CUDA=1` only for these qualification runs.  It
does not turn an unchecked configuration into a supported one.

## Two-deck real-time matrix

For every row, run two armed decks continuously for at least ten minutes.  Save
the native engine telemetry and worker diagnostics; the #109 ring proxy alone
is not an underrun measurement.

| OS | model | frames/chunk | duration | engine underruns | p50/p95/p99 latency | peak VRAM | result |
| --- | --- | ---: | ---: | ---: | --- | ---: | --- |
| Ubuntu | mrt2_small | 25 | 10 min | | | | [ ] |
| Ubuntu | mrt2_small | 5 | 10 min | | | | [ ] |
| Windows | mrt2_small | 25 | 10 min | | | | [ ] |
| Windows | mrt2_small | 5 | 10 min | | | | [ ] |

- [ ] Repeat the matrix for `mrt2_base` if it will be offered on that support
  floor; otherwise document its higher minimum VRAM separately.
- [ ] Change weighted prompts, temperature, top-k, prompt CFG, notes, drum state,
  and drum CFG during each 5-frame run.
- [ ] Verify note onset decays to sustain after one chunk.
- [ ] Verify reset with a fixed seed starts a repeatable fresh stream and never
  pretends to reseed existing continuation state.
- [ ] Compare prompt/note guidance behavior against MLX parity fixtures.
- [ ] Listen across every chunk boundary and verify continuous stereo 48 kHz
  audio with the approximately 1.5-second playback safety ring.

## Lifecycle and recovery

- [ ] Readiness arrives only after CUDA/model/processor warm-up completes.
- [ ] Generation latency and command queue depth remain visible while playing.
- [ ] Kill the shared worker: both decks stop clearly and can recover.
- [ ] Corrupt or remove one snapshot: startup identifies the repair action.
- [ ] Cancel install/update at every stage; the previous verified runtime works.
- [ ] Exit and crash the app during warm-up and generation; no Python or GPU
  process remains.
- [ ] Prove the production Rust host owns one model worker with independent deck
  continuation states, as selected by #109.
- [ ] On the minimum-VRAM Windows and Linux hosts, switch the shared worker from
  equal models to different models and back while both decks are active. Verify
  both decks enter loading/unavailable together, the old process and CUDA
  allocation are fully reaped before replacement allocation starts, and no
  transient second generation appears in process/VRAM telemetry.
- [ ] Force the replacement launch and model load to fail after the old shared
  worker is reaped. Verify both decks remain clearly unavailable and a later
  valid selection recovers them. This serialized minimum-VRAM transition does
  not promise live hardware rollback to the old worker.

## Release gate

- [ ] Issue #108 notices and download acknowledgement are complete.
- [ ] Both platform release checklists link the captured results above.
- [ ] The minimum GPU, VRAM, driver, and runtime are written from measured data.
- [ ] Flip the fail-closed qualification constant only in the PR containing all
  evidence and the final target-specific locks.
