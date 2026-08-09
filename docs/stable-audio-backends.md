# Stable Audio 3 backend contract

LSDJ selects one Stable Audio backend explicitly:

- Apple Silicon macOS uses the existing MLX runtime.
- Linux and Windows use the official LiteRT/TFLite CPU runtime.
- Windows x64 has an optional PyTorch/CUDA candidate for Small Music and Small
  SFX. It remains behind a fail-closed release gate; Auto therefore continues
  to select TFLite until the issue #114 hardware and provenance matrix is done.
- Unsupported platforms fail with a diagnostic. They do not silently select a
  different runtime.
- `LSDJ_SA3_BACKEND=mlx|tflite` is a diagnostic/developer override. The MLX
  override remains restricted to Apple Silicon; TFLite remains restricted to
  supported Linux/Windows x64 targets.
- `LSDJ_SA3_PREFERENCE=auto|gpu|cpu_tflite` is the backend-policy seam. An
  explicit GPU request never silently falls back; while the release gate is
  incomplete it fails before launch and asks the caller to confirm TFLite.

Both adapters consume the same `GenerationRequest` contract and share one
argument translator. A populated control is either forwarded or rejected; it
is never silently discarded.

## Feature matrix

| Capability | MLX | TFLite | Windows CUDA candidate | Notes |
| --- | --- | --- | --- | --- |
| Music and SFX | Yes | Yes | Gated | Official Small Music/SFX models |
| Medium / 380 seconds | Yes | Yes | No | TFLite fallback; no unofficial FlashAttention build |
| Audio-to-audio | Yes | Yes | Gated | LSDJ normalizes input before every backend |
| Inpainting | Yes | Yes | Gated | Shared `inpaint_range` control |
| Continuation | Yes | Yes | Gated | Inpaint range from source duration to requested duration |
| Positive/negative prompt | Yes | Yes | Gated | Negative prompt requires CFG other than 1 |
| Seed, duration, steps, CFG, APG | Yes | Yes | Gated | CUDA maps directly to the pinned upstream Python API |
| Stacked LoRA with strength | Yes | Yes | Gated | Independent strength per adapter |
| Per-step LoRA gating | Yes upstream | No | No | Not exposed by LSDJ |
| Progress | Text stream | Text stream | Sampler callback | Normalized by LSDJ |
| Cancellation | Process stop | Process stop | Callback + process stop | CUDA also yields to realtime MRT2 |
| Partial audio preview | No | No | No | No pinned backend returns partial audio |

The `/api/sa3/status` endpoint reports the preference choices, active backend,
readiness, capabilities, real limitations, current queued/running state, and on
Windows the CUDA release gate and qualification blockers.

## Windows CUDA process and scheduling model

The CUDA adapter calls the official pinned Python API; no upstream code is
copied into LSDJ. Each request runs in a disposable child and checks an exact
provenance stamp before heavyweight imports. It then verifies the shared
package versions, CUDA runtime, NVIDIA driver, device, reported memory, and the
measured reservation before loading Small Music or Small SFX. It never invokes
the Hub downloader and never falls back to PyTorch CPU.

The managed launcher binds the private request to that child with an ephemeral
secret inherited through an allowlisted environment entry; the request contains
only its SHA-256. The worker verifies and removes the secret before importing
upstream code. Every structured event carries a bounded job ID, while secrets,
prompts, paths, and ambient credentials are excluded from diagnostics.

The file-locked GPU broker is shared with MRT2 across processes. MRT2 leases
have realtime priority. SA3 is admitted only when no MRT2 lease or waiter is
present and its measured reservation fits the conservative budget. If MRT2
arrives during sampling, the next upstream callback cancels the SA3 child. A
daemon watchdog provides the same hard process boundary while upstream model
loading or decoding offers no callback. Process exit releases the CUDA context.
OOM, driver reset, worker crash, and app exit are contained by the same process
boundary and the native process-tree supervisor.

The candidate lock resolves the pinned SA3 requirements as PyTorch/torchaudio
2.7.1+cu126 and Hugging Face Hub 1.7.1. This differs from #110's MRT2 candidate,
so it is a shared-runtime hypothesis rather than a production upgrade. MRT2
must pass its parity and dual-deck matrix on this exact lock. LSDJ will not ship
a second multi-gigabyte PyTorch environment if that qualification fails.

## Audio boundary

LSDJ accepts bounded, uncompressed integer PCM WAV input (8/16/24/32 bit,
8–384 kHz, mono through 32 channels). It converts internally to the official
runtime format: 44.1 kHz, stereo, PCM16. Mono is duplicated; multichannel input
uses its first two channels, matching the official TFLite path. No system
`ffmpeg`, shell, or media executable is invoked.

Generation remains outside the audio callback and is serialized across both
backends. The TFLite adapter caps XNNPACK and common numeric runtimes at four
threads by default (configurable from 1–8) and launches at background priority.
Timeouts and output bounds stop a wedged or runaway request. Platform hardware
runs still need to establish practical RAM/CPU admission thresholds while both
MRT2 decks are active.

Every generated file must be a non-empty 44.1 kHz stereo PCM16 WAV with exactly
`round(seconds * 44100)` frames before it can enter LSDJ's library/player.
Corrupt, truncated, oversized, or wrong-duration output fails the request.

## Pinned upstream and storage

The machine-readable trust handoff is
[`sa3-tflite-pin.json`](../sa3-tflite-pin.json):

- code: `Stability-AI/stable-audio-3` at
  `a0b57f5483c4588f827f3552b7d5c6ca2a9687be`;
- models: `stabilityai/stable-audio-3-optimized` at
  `6736003cb57d06b7b1fdc36fad31b2a3709e4774`;
- eight fp32 model artifacts carry exact byte counts and SHA-256 digests;
- the official runtime dependency surface is resolved into the universal,
  hash-locked `scripts/sa3-tflite-requirements.lock`.

Measured download totals, including the shared T5Gemma encoder, are:

- Small Music: 2,836,149,512 bytes;
- Small SFX: 2,836,149,512 bytes;
- both Small models together (shared files deduplicated): 4,674,908,056 bytes;
- Medium: 10,027,905,456 bytes;
- all three models (shared files deduplicated): 14,138,994,904 bytes.

The app installer must download these pinned artifacts, verify them, and write
the warm/readiness stamp plus `.lsdj-provenance.json` before generation. That
stamp records the exact runtime and model repositories/revisions above; a
missing or mismatched stamp is a failed state. The runtime process receives
`HF_HUB_OFFLINE=1` and no Hugging Face token, so a missing file fails closed
instead of using upstream's mutable first-run downloader. Licensing,
attribution, acknowledgement, and credential UX remain owned by issue #108.

## Evidence and remaining gates

The repository suite exercises backend selection, argument parity, every LSDJ
control, PCM normalization, exact output validation, corrupt output, progress,
cancellation, timeouts, missing assets, and the 380-second command contract
without loading model weights.

This does **not** claim a real model run. The hardened #107 installer now
consumes both manifests, verifies every byte, builds the isolated environment,
warms all three model pairs, and atomically promotes or rolls back the candidate.
Before release, Ubuntu plus Windows hardware runs must still verify Small/Medium
generation, LoRA, cancellation, storage, RAM/CPU use, and coexistence with both
active MRT2 decks. Partial preview and structured progress require a future
upstream API; they are reported as limitations today.
