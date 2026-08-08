# Issue #109 — PyTorch MRT2 portability spike

Audit date: 2026-08-08. This note covers software/API validation. No Linux or
Windows NVIDIA machine was available in this workspace, so it contains no
fabricated performance results.

## Outcome

The port is consumable without an LSDJ fork: use the Transformers model snapshot
at an exact Hugging Face revision, keep the adapter thin, prefetch all assets,
and run with `local_files_only=True`. The repository is public and Apache-2.0.
The earlier Space named in discovery material is now authentication-gated, but
it is not needed by the proposed path.

The production decision is **conditional no-go pending hardware qualification**.
Do not begin #110 until both target operating systems pass the two-deck,
ten-minute matrix and #108 resolves weight/processor licensing. This is a
maturity and evidence gate, not a rejection of the implementation.

## Immutable dependency path

| Component | Immutable reference | Finding |
| --- | --- | --- |
| PyTorch source/API | `multimodalart/magenta-realtime-torch@6d076baa3df3b10448876c400521a015a5137c59` | Public Apache-2.0 source; no PyTorch-specific release/tag |
| Base model + remote code | `magenta-community/magenta-realtime-2@92087988d05d0fe38b11f021f0b0d00a75afb86b` | Transformers custom model, ~2.46B reported parameters |
| Small model + remote code | `magenta-community/magenta-realtime-2-small@7037d99551c84ac5c6afb7f1a5e58c65e7233dbb` | Transformers custom model, ~282M reported parameters |
| MusicCoCa processor | `magenta-community/magenta-rt-musiccoca-torch@236c488e38aa98643805514996934d705668298b` | Text/audio encoder artifacts; exact revision must be supplied by the adapter |
| Original Google assets | `google/magenta-realtime-2@010aa0dcb0dfd27b24f0ad07b4dad63e8f9521cc` | Declared base model/weight provenance |

The runnable fixture is `spike/mrt2_pytorch/harness.py`; the complete
machine-readable inventory is `spike/mrt2_pytorch/provenance.json`.

The audited GitHub `pyproject.toml` is still the upstream JAX/MLX package: it
does not declare a PyTorch extra or pin `torch`/`transformers`. The Transformers
snapshot is therefore the cleaner dependency boundary. LSDJ must own a
target-specific lock for Python 3.12, PyTorch, Transformers, and CUDA wheels;
the spike's exact direct pins are candidates, not a production lock.

## Control and state parity

| LSDJ MLX behavior | PyTorch port | Disposition |
| --- | --- | --- |
| Weighted text prompt embeddings | `MusicCoCaProcessor.layer()` returns 12 style tokens | Thin adapter; cache tokens on changes, not per chunk |
| Text negative prompt | Not exposed by the MRT2 deck today | No mapping needed; upstream CFG negatives are masked conditioning, not negative text |
| Temperature and top-k per chunk | `generate(temperature=, top_k=)` | Direct |
| Prompt/note CFG matching `.mlxfn` | `generate(..., guidance=True)` | Direct but more expensive than upstream's default token-CFG path; benchmark parity mode |
| Drum adherence token | `cfg_drums` | Direct; true-CFG mode still treats drums through the learned token |
| Note states `-1/0/1/2/3` and drum `-1/0/1` | Raw `notes`/`drums` arrays | Direct; LSDJ retains onset-to-sustain decay |
| Small/base model selection | Separate pinned model repositories | Direct; switching requires a worker/model restart |
| Per-deck continuation | Returned state contains decoder, RNG, and codec state | One model can safely own two state objects; must be sustained-tested |
| Seed | Seed creates the RNG only when state is new | Adapter documents reset-to-reseed; current LSDJ UI has no MRT2 seed control |
| 25-frame and 5-frame chunks | Arbitrary `frames`; 40 ms/frame, 48 kHz stereo | Direct; output length/continuity must be measured on hardware |
| Audio style sampling | Processor source implements audio embedding/resampling | API match; golden/hardware parity is not present in upstream CI |
| Warm-up/reset | No stable high-level readiness API | Adapter performs a throwaway generate then clears state; readiness contract belongs in #110 |

The upstream workflow at the audited revision runs macOS MLX tests only. It
does not run the PyTorch port on Linux, Windows, or CUDA, and checkpoint-heavy
parity tests are skipped. Claims in model cards are useful provenance, not a
substitute for LSDJ qualification.

## Harness topology and ring model

`shared-worker` loads one model in one process and keeps independent deck A/B
state, scheduling generation round-robin. `per-deck` starts two processes and
therefore two model instances. Both use the same control-change sequence and
the same 1.5-second prebuffer gate.

The harness reports starvation duration/transitions as `underrun_proxy_*`.
LSDJ's Rust engine counts individual audio callback blocks after the ring is
primed, so the proxy is deliberately not named an engine underrun. Final
qualification must capture both the harness JSON and the app's native telemetry.

Start with the shared-worker/two-state topology: it avoids duplicating a large
model and has the smallest support-floor risk. Its failure domain covers both
decks and its serialized inference load may miss real time. Move to per-deck
workers only if concurrent GPU execution materially improves the 5-frame
two-deck result on supported hardware and the measured VRAM floor is acceptable.

## Packaging feasibility

An installer can ship without user-installed Python, Git, a shell, or a CUDA
toolkit by bundling an embedded Python runtime, exact binary wheels, and
prefetched snapshots. Users still need a compatible NVIDIA driver. The official
PyTorch release matrix publishes the same CUDA wheels for Linux and Windows.

Risks that must be closed before release:

- `trust_remote_code=True` executes snapshot code; only the audited commit may
  be acquired, hash-checked, and promoted atomically.
- `model.load_processor()` defaults to a mutable repository reference. The LSDJ
  adapter must resolve the exact processor revision locally, as the harness does.
- `torch.compile` can require a compiler on Windows. Production must not trigger
  an unbundled MSVC/toolchain install. AOTInductor artifacts are GPU-architecture
  specific, so one artifact cannot establish a broad GPU support floor.
- Eager, `torch.compile`, CUDA graph, and AOT behavior have not been compared on
  the target systems. Upstream exposes the fast CUDA graph path through a stream
  surface rather than the simple resumable `generate` call, so a stable chunk API
  may require an upstream contribution.
- Produce platform-specific, hash-locked wheels only after choosing the CUDA
  runtime and minimum driver from the hardware results.

## Licensing/provenance escalation

The fork's code is Apache-2.0. The derived Transformers model cards say
Apache-2.0, while they declare `google/magenta-realtime-2` as their base and the
Google weights are CC-BY-4.0. The MusicCoCa artifact card is also CC-BY-4.0.
Issue #108 must decide the effective redistribution/notice obligations; this
spike makes no legal conclusion.

## Decision gates for #110

Go only when all are true:

1. Linux and Windows each sustain both decks for ten minutes, at 25 and 5
   frames, with zero native engine underruns on the proposed minimum GPU.
2. The parity-guidance path meets the budget, including a live prompt and note
   onset/sustain change; token-CFG results cannot stand in for it.
3. Exact wheel and snapshot locks install offline in a clean bundled runtime.
4. Startup/readiness, one-deck crash behavior, whole-tree shutdown, and device
   recovery are demonstrated on both platforms.
5. #108 approves the notices, acknowledgement, and redistribution path.

Until those gates pass, #109's software deliverables are complete but the
production recommendation remains conditional no-go.

## Primary evidence

- Source/API: <https://github.com/multimodalart/magenta-realtime-torch/tree/6d076baa3df3b10448876c400521a015a5137c59/magenta_rt/torch>
- Source license: <https://github.com/multimodalart/magenta-realtime-torch/blob/6d076baa3df3b10448876c400521a015a5137c59/LICENSE>
- Base model: <https://huggingface.co/magenta-community/magenta-realtime-2/tree/92087988d05d0fe38b11f021f0b0d00a75afb86b>
- Small model: <https://huggingface.co/magenta-community/magenta-realtime-2-small/tree/7037d99551c84ac5c6afb7f1a5e58c65e7233dbb>
- Google model card/weights: <https://huggingface.co/google/magenta-realtime-2/tree/010aa0dcb0dfd27b24f0ad07b4dad63e8f9521cc>
- PyTorch binary matrix: <https://pytorch.org/get-started/previous-versions/>
- Windows compiler caveat: <https://docs.pytorch.org/tutorials/unstable/inductor_windows.html>
