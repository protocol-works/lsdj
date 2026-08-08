# Issue #114 — Windows Stable Audio 3 CUDA qualification

This checklist is intentionally unchecked. Unit tests prove policy, mapping,
broker behavior, and failure containment without weights; they are not NVIDIA
performance evidence. Until every release gate is complete, Auto and the model
manager continue to use the supported TFLite backend.

Set `LSDJ_ALLOW_UNVERIFIED_SA3_CUDA=1` only on a dedicated qualification host.
It permits explicit GPU probes; it does not enable Auto or make a build
release-ready.

## Immutable inputs and shared runtime

- [x] Pin the official upstream source commit without an LSDJ fork.
- [x] Resolve one 44-package, fully hash-locked Windows x64 candidate shared by
  SA3 and MRT2: Python 3.12, PyTorch/torchaudio 2.7.1+cu126, Transformers 5.8.0,
  Hugging Face Hub 1.7.1.
- [x] Record the source archive, Small Music/SFX root weights, optional Medium
  root weight, exact immutable revisions, known hashes, and incomplete gates in
  `sa3-pytorch-cuda-pin.json`.
- [ ] Through #108's authenticated terms flow, record SHA-256 and byte count for
  every required Small config and nested T5Gemma artifact.
- [ ] Regenerate the lock with the release uv version on a clean Windows x64
  host and prove `--require-hashes --only-binary :all:` installation.
- [ ] Re-run all MRT2 functional fixtures on the shared PyTorch 2.7.1/CUDA 12.6
  runtime. The #110 PyTorch 2.12.1/CUDA 13.0 results do not transfer.
- [ ] Confirm the source/runtime/model provenance shown by diagnostics matches
  the compiled manifest and that an altered stamp fails before model import.
- [ ] Complete #108 acknowledgement, attribution, and terms UX before exposing
  the CUDA download.

Run the local candidate audit with:

```console
python3 scripts/audit-sa3-cuda-pin.py --allow-incomplete
```

The same command without `--allow-incomplete` is the release gate and must fail
until the required gated hashes exist.

## Required host inventory

For each row, save the LSDJ version, Windows build, GPU, VRAM, NVIDIA driver,
PyTorch version, CUDA runtime, source revision, model revision, peak VRAM, and
generated WAV hash. CUDA 12.6's provisional minimum Windows driver is 560.76;
replace that value with the measured support floor before release.

| Host tier | GPU / VRAM | driver | Small Music reserve | Small SFX reserve | result |
| --- | --- | --- | ---: | ---: | --- |
| proposed minimum | | | | | [ ] |
| mid-range | | | | | [ ] |
| high-end | | | | | [ ] |
| insufficient VRAM | | | n/a | n/a | [ ] clean failure |

- [ ] Measure cold-load, sampling, decode, peak allocated/reserved VRAM, and
  post-exit VRAM for Small Music.
- [ ] Repeat for Small SFX.
- [ ] Choose conservative per-model reservations and at least 1 GiB headroom
  from results; never infer them from marketed card capacity.
- [ ] Add an unrelated VRAM consumer before and during admission. Free VRAM must
  be treated as advisory and unsafe work must fail before start.
- [ ] Unsupported GPU, old driver, CUDA mismatch, and no GPU fail without a
  PyTorch CPU attempt.

## Shared-contract parity

For both Small models, compare the same pinned fixtures against TFLite:

- [ ] text-to-audio;
- [ ] audio-to-audio and init-noise level;
- [ ] inpainting and continuation;
- [ ] positive and negative prompt;
- [ ] fixed seed, duration, sampling steps, CFG, and APG;
- [ ] one LoRA and a stacked LoRA with independent strengths;
- [ ] normalized progress and cancellation;
- [ ] exact 44.1 kHz stereo PCM16 duration/output boundary.

Record intentional numerical/performance differences. A populated control may
not be ignored. If the pinned API cannot represent it, coordinate upstream or
route it explicitly to TFLite.

## Broker, isolation, and lifecycle

- [ ] Start SA3, then request MRT2 work. At the next sampler callback SA3 exits,
  releases its lease/context, and MRT2 proceeds.
- [ ] Queue SA3 while MRT2 holds a lease. SA3 waits without disturbing either
  deck or the native audio callback.
- [ ] Cancel while waiting, loading, sampling, and decoding; no child or CUDA
  allocation remains.
- [ ] Force CUDA OOM, worker exception, invalid output, and abrupt worker death;
  MRT2 and deck audio continue.
- [ ] Switch to CPU/TFLite during/after generation and exit the app at every
  worker stage; the Windows Job Object removes every descendant.
- [ ] Corrupt the broker state and provenance stamp; both fail closed with a
  bounded, non-sensitive diagnostic.

## Dual-deck realtime acceptance

Run two active `mrt2_small` decks for at least ten minutes per row while
repeatedly queueing/running/cancelling alternating Small Music and Small SFX
jobs. Save native engine underrun telemetry; silence or ring occupancy is not a
substitute.

| frames/chunk | duration | SA3 workload | engine underruns | MRT2 p50/p95/p99 | SA3 p50/p95 | peak VRAM | result |
| ---: | ---: | --- | ---: | --- | --- | ---: | --- |
| 25 | 10 min | alternating Small Music/SFX | | | | | [ ] |
| 5 | 10 min | alternating Small Music/SFX | | | | | [ ] |

- [ ] Both rows have zero engine-reported underruns.
- [ ] Exercise weighted prompts, notes, drums, seed/reset, and both deck states
  during the 5-frame run.
- [ ] Verify broker yield does not kill or reset the shared MRT2 worker.

## Selection and release

- [ ] Auto selects CUDA only on a fully qualified configuration and explains
  why it chose TFLite otherwise.
- [ ] Explicit GPU shows requirements and fails or offers a user-confirmed
  TFLite fallback before generation; it never silently falls back after start.
- [ ] CPU/TFLite always selects the independent verified portable runtime.
- [ ] Active backend, worker state, fallback reason, estimate/reservation, GPU,
  VRAM, driver, CUDA, PyTorch, and immutable revisions are visible.
- [ ] Keep Medium on TFLite. Enable it only in a later evidence-bearing change
  with an official Windows FlashAttention path; never ship an unofficial wheel.
- [ ] Flip `HARDWARE_QUALIFIED` and the manifest release gate only in the PR that
  links all evidence above.
