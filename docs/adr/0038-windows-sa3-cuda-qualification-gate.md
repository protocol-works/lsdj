# ADR-0038: Windows SA3 CUDA is a gated shared-runtime backend

Status: proposed, implemented behind a release gate

## Context

Windows already has the official LiteRT/TFLite Stable Audio 3 backend. CUDA can
make Small Music and Small SFX practical on a capable NVIDIA GPU, but it adds a
second large model family beside the realtime MRT2 decks. The two projects'
original dependency pins do not match: the pinned SA3 source requires PyTorch
2.7.1 and Hugging Face Hub 1.7.1 or newer, while the MRT2 candidate from #110
used PyTorch 2.12.1 and Hugging Face Hub 1.5.0.

The SA3 model repositories are gated. Public Hugging Face metadata establishes
immutable revisions and the root weight hashes, but cannot establish all
configuration and nested T5Gemma hashes without the authenticated terms flow
owned by #108. No Windows NVIDIA qualification host was available for this
change.

## Decision

- Consume `Stability-AI/stable-audio-3` at the immutable upstream commit in
  `sa3-pytorch-cuda-pin.json`. LSDJ owns a thin adapter; it does not fork or
  vendor upstream runtime code.
- Test one shared Windows PyTorch environment for MRT2 and SA3. The resolved
  candidate uses PyTorch/torchaudio 2.7.1+cu126 and Hugging Face Hub 1.7.1 in a
  fully hashed 44-package lock. This is a candidate, not a supported upgrade:
  MRT2 must be requalified on it.
- Keep the TFLite runtime separately installed and available. It is not placed
  inside the PyTorch environment and remains the release/default backend.
- Run SA3 in a new disposable child for every generation. Heavy imports, model
  allocation, and CUDA context creation occur only in that child. Completion,
  cancellation, broker yield, OOM, crash, backend switch, and app teardown end
  the process.
- Bind each private request to its supervised child with a per-launch secret:
  only its SHA-256 enters the bounded request file, the secret arrives through
  an allowlisted inherited environment value, and the worker removes it before
  upstream imports. A bounded job ID is attached to every progress/terminal
  event; neither value enters argv or diagnostics.
- Coordinate CUDA with a cross-process file-locked broker. MRT2 uses realtime
priority; background SA3 checks for a higher-priority waiter at every sampler
callback. A daemon watchdog covers model loading and decoding, whose upstream
calls have no callback, and hard-stops only the disposable SA3 process so the
MRT2 request can proceed.
- Expose Auto, GPU, and CPU/TFLite policy in the backend status contract. Auto
  remains on TFLite until the hardware gate is flipped with evidence. Explicit
  GPU fails before launch and asks for a confirmed TFLite fallback while the
  candidate is blocked; it never silently changes backend or runs PyTorch on
  CPU.
- Enable only Small Music and Small SFX in the CUDA capability contract. Medium
  remains on TFLite unless an official Windows FlashAttention path and a
  measured hardware tier are qualified. Unofficial wheels, custom extensions,
  and private model forks are forbidden.

## Trust and release gate

The native installer compiles the candidate manifest but rejects it unless all
of these are true in one reviewed change:

1. `releaseReady` and `gatedArtifactsComplete` are true and the blocker list is
   empty.
2. Every required Small model artifact has an exact SHA-256 and byte count.
3. The embedded shared lock matches the manifest's exact size and SHA-256.
4. A worker provenance stamp matches the source revision, lock digest, package
   versions, model repository, and model revision exactly.
5. The Windows NVIDIA matrix in the issue #114 checklist is complete.

The worker independently rechecks package, CUDA runtime, driver, free-memory,
reservation, provenance, and model path facts before importing a model. A
reported free-memory value is only an admission snapshot; the process boundary
is still the recovery mechanism for unrelated VRAM pressure and CUDA failure.

## Qualification no-go: resident MRT2 ownership

Commit `5cbe842` is approved as broker safety behavior. On Windows it prunes a
lease only when the owner is positively known to have exited; access or query
uncertainty retains the record and fails closed. This approval is not NVIDIA
hardware or VRAM evidence.

Commit `7fdbb32` deliberately acquires one shared, process-lifetime MRT2 lease
before CUDA availability is queried or any model is loaded. Keeping that lease
through model-load failure and for the lifetime of the worker is fail-closed:
SA3 cannot treat VRAM owned by a resident MRT2 model as free. The ordering must
remain until a measured replacement is available.

The current single-level lease consequently prevents SA3 admission while the
MRT2 worker is alive, so it cannot satisfy the coexistence and dual-deck tests
required by issue #114. Releasing the lease between MRT2 generations would not
be safe because the model remains resident. It would also undercount ownership
when multiple models or workers are present.

Issue #114 is a no-go until one of these paths is completed:

1. Measure resident and active-generation VRAM on Windows NVIDIA hardware, then
   implement a two-level broker. Long-lived reservations must account for every
   resident model and worker; short active-generation priority leases must let
   realtime MRT2 work interrupt or defer background SA3 work.
2. Obtain explicit product acceptance that MRT2 will unload before SA3 starts,
   then re-load and warm up afterward, and qualify that lifecycle instead of
   coexistence.

The required VRAM and hardware evidence does not yet exist. Auto selection,
public SA3 CUDA availability, `HARDWARE_QUALIFIED`, and the release manifest
gate remain disabled. This record does not change runtime code, dependency
pins, qualification gates, or make a release-support claim.

## Consequences

The design and model-free failure behavior can merge without delaying the
TFLite Windows release. CUDA is not advertised as installable or selected by
Auto in this state. Completing #108's authenticated audit, resolving measured
VRAM tiers, and running both SA3 parity and MRT2 realtime qualification are
mandatory follow-ups, not release notes that can be waived.
