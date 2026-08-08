# Stable Audio 3 backend contract

LSDJ selects one Stable Audio backend explicitly:

- Apple Silicon macOS uses the existing MLX runtime.
- Linux and Windows use the official LiteRT/TFLite CPU runtime.
- Unsupported platforms fail with a diagnostic. They do not silently select a
  different runtime.
- `LSDJ_SA3_BACKEND=mlx|tflite` is a diagnostic/developer override. The MLX
  override remains restricted to Apple Silicon; TFLite remains restricted to
  supported Linux/Windows x64 targets.

Both adapters consume the same `GenerationRequest` contract and share one
argument translator. A populated control is either forwarded or rejected; it
is never silently discarded.

## Feature matrix

| Capability | MLX | TFLite | Notes |
| --- | --- | --- | --- |
| Music and SFX | Yes | Yes | Official small Music/SFX DiTs |
| Medium / 380 seconds | Yes | Yes | Runtime correctness is model-free tested; Windows/Linux performance still needs hardware evidence |
| Audio-to-audio | Yes | Yes | LSDJ normalizes input before either CLI sees it |
| Inpainting | Yes | Yes | Shared `inpaint_range` control |
| Continuation | Yes | Yes | The official continuation primitive is an inpaint range from source duration to requested duration |
| Positive/negative prompt | Yes | Yes | Negative prompt requires CFG other than 1 |
| Seed, duration, steps, CFG, APG | Yes | Yes | Shared validation and CLI spelling |
| Stacked LoRA with strength | Yes | Yes | TFLite runs fp32 because upstream cannot merge LoRA into quantized graphs |
| Per-step LoRA gating | Yes upstream | No | Not exposed by LSDJ; the TFLite CLI explicitly rejects it |
| Progress | Text stream | Text stream | LSDJ normalizes sampling/decode messages; upstream has no structured progress protocol |
| Cancellation | Process stop | Process stop | A cancelled request stops the isolated generation process |
| Partial audio preview | No | No | Neither pinned CLI exposes audio before the final WAV is written |

The `/api/sa3/status` endpoint reports the selected backend, readiness,
capabilities, real limitations, and current queued/running state.

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
