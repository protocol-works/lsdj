# ADR 0037: Platform MRT2 runtimes behind one worker contract

## Status

Accepted for implementation; PyTorch release qualification remains blocked by
issue #109's Linux and Windows NVIDIA hardware matrix.

## Context

LSDJ's deck worker contract predates platform support and constructed the MLX
implementation directly.  Apolinario's PyTorch port provides the corresponding
CUDA implementation through immutable Hugging Face Transformers snapshots.  It
is an external dependency: LSDJ does not copy, patch, or publish that runtime.

The #109 spike validated the API surface but did not have Linux or Windows
NVIDIA hosts.  It therefore could not establish a supported GPU/driver floor
or prove two-deck real-time performance.

## Decision

- The native host passes an explicit runtime on every deck-sidecar launch:
  `mlx` on macOS and `pytorch-cuda` on Linux/Windows.
- Python resolves that name through the model-independent `Mrt2Engine` contract.
  It never catches a backend failure and tries another implementation.
- PyTorch requires CUDA.  Missing CUDA, a CPU-only torch build, missing pinned
  assets, corrupt assets, and initialization failures are startup failures.
- The PyTorch adapter loads the exact model and MusicCoCa revisions recorded by
  #109 with `local_files_only=True`.  `trust_remote_code=True` receives only the
  resolved local immutable snapshot path, never a mutable repository name.
- LSDJ owns input normalization, weighted-style caching, continuation state,
  reset-to-reseed semantics, PCM validation, lifecycle integration, and
  diagnostics.  Upstream owns model and processor code.
- The PyTorch runtime remains fail-closed until the required hardware evidence
  exists.  `LSDJ_ALLOW_UNVERIFIED_MRT2_CUDA=1` is an explicit qualification-only
  opt-in and is disclosed in diagnostics.
- The PyTorch topology is one supervised process with two deck loops. Equal
  model selections share one loaded upstream model behind serialized inference
  and keep independent continuation/style/control state. Different per-deck
  model selections preserve existing behavior by loading both models in that
  same process. macOS retains its two independent MLX workers.

## Consequences

macOS keeps its existing MLX behavior.  Linux and Windows builds have an
explicit CUDA path and actionable failure diagnostics, but cannot be described
as supported releases yet.  Platform-specific, hash-locked Python/CUDA inputs
exist for Linux x86_64 and Windows x86_64, but they remain installation
candidates until clean-host and NVIDIA qualification completes.  Their
existence does not imply a supported minimum GPU or driver.

To update upstream, change only immutable revisions and candidate versions in
`backend/lsdj/mrt2.py`, re-run the model-free contract suite, then repeat every
unchecked item in `docs/issue-110-hardware-checklist.md`.  Never copy upstream
runtime sources into this repository.
