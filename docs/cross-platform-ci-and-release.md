# Cross-platform CI and release contract

Issue #107 introduces two related but deliberately separate gates:

1. shared software contracts that can run unattended on GitHub-hosted macOS,
   Ubuntu, and Windows runners; and
2. qualification that requires real audio, MIDI, GPU, installer, or signing
   hardware and credentials.

Passing CI means a change is portable at the code and build boundary. It does
not by itself claim that a particular device, accelerator, or installer has
been qualified.

## Automated shared checks

`.github/workflows/ci.yml` runs the same three-stack checks on `macos-15`,
`ubuntu-24.04`, and `windows-2025`:

- frontend lint, TypeScript checking, and the complete Vitest suite;
- Rust workspace tests and Clippy, including each OS's compiled platform code;
- Python lint plus controller, worker, transport, validation, model-free deck
  behavior, SA3 subprocess, and SA3 readiness tests; and
- release artifact contract tests, including checksum, identity, completeness,
  and pre-publication draft verification failures.

The backend's locked `ci` dependency group intentionally omits Magenta, MLX,
PyTorch, TFLite, and model weights. The SA3 process tests use a copied Python
interpreter and fake CLI, so argument passing, output handling, failure, and
timeout behavior run on all three operating systems. Only tests that actually
import a model runtime stay in the full local `just check` suite and in
backend-specific qualification. This keeps a shared Python regression gate
honest without installing an unsupported accelerator on a runner.

The Windows matrix uses runner-native `python`, `npm`, `rustup`, and `cargo`
commands. Shell-specific system package installation is restricted to the
Ubuntu step.

## Hardware-only qualification

The following evidence must be recorded in the platform issue or its linked
qualification run. It is not replaced by green hosted-runner CI:

| Surface | Minimum real-system evidence |
| --- | --- |
| Audio output | Enumerate and play through representative mono, stereo, integer, and multichannel devices; verify master/cue routing, underrun behavior, device loss, and recovery. |
| MIDI | Connect supported controllers; verify input, LEDs, hot-plug, shutdown, and reconnect behavior with native drivers. |
| MRT2 | Run two simultaneous decks at both chunk sizes on the supported CPU/GPU backend; capture startup, p50/p95/p99 latency, RAM/VRAM, sustained playback, and teardown. |
| Stable Audio 3 | Generate every supported duration/kind on the supported CPU/GPU backend; verify cancellation, progress, failure cleanup, and output playback. |
| App lifecycle | Install on a clean user account, launch without developer tools, install/remove models, survive paths with spaces/non-ASCII text, and leave no worker processes after normal or forced shutdown. |
| Distribution | Exercise the native installer/uninstaller, OS trust prompts, signing where applicable, checksum validation, offline behavior after install, and a representative antivirus scan. |

CI tests should use fakes or loopback devices only when they verify a shared
contract. A fake result must not be reported as hardware qualification.

## Release producer/publisher boundary

The tag workflow requires macOS and Linux release artifacts. It has three
stages:

1. `validate` accepts only a calendar-version `v*` tag whose commit is contained
   in `main`.
2. Independent producers build their platform artifacts. `produce-macos` waits
   behind the protected `macos-release` Environment,
   freezes the backend, imports ephemeral signing material, builds, signs,
   notarizes, staples, and verifies the app and DMG. It then uploads one Actions
   artifact containing the DMG, `SHA256SUMS.txt`, and metadata binding the
   producer to the tag and exact source revision. `produce-linux` builds the
   x86_64 AppImage on Ubuntu 22.04 (glibc 2.35), verifies its desktop/resource
   layout and ELF dependencies, performs an isolated-XDG virtual-X11 smoke, and
   uploads the AppImage with the same checksum/tag/revision contract.
3. `publish` is the only job with `contents: write`. It downloads every required
   producer bundle, requires the producer set to match exactly, recomputes all
   sizes and SHA-256 digests, and verifies tag/revision/platform metadata before
   it creates a GitHub Release.

Linux is fail-closed: a skipped or failed producer prevents the publisher from
running. This automated package smoke does not replace issue #112's NVIDIA,
Wayland/Xorg, audio, MIDI/FLX4, or suspend/resume hardware gate.

The publisher creates an unpublished draft, uploads the complete verified file
set, checks GitHub's returned asset names, sizes, upload state, and SHA-256
digest, and only then makes the release public. A missing digest fails closed.
The published checksum and release-index files provide the same cryptographic
verification surface to downloaders. Creation records the draft's immutable
numeric release ID plus its tag and exact source revision. Verification,
publication, and failure cleanup remain bound to that ID; cleanup rechecks that
the same release is still a draft with the expected tag and source marker before
deleting it. A tag lookup therefore cannot redirect cleanup to a collaborator's
replacement draft. A failure before publication keeps the release private and
attempts to remove only the draft created by that run. An existing release is
never overwritten.

Signing and notarization secrets exist only in the macOS producer. The
publisher receives no signing credentials, and producers never receive
`contents: write`.

## Adding a release platform

Linux or Windows artifacts become required only in the change that adds their
production installer. That change must, together:

- add a named producer job with its platform-native build and trust checks;
- add the producer policy to `scripts/release_artifact.py`;
- upload its installer, checksum, and tag/revision metadata as one Actions
  artifact;
- add the producer to the publisher's `needs` list and
  `--required-producer` arguments; and
- extend the release contract tests and real-system qualification record.

There must remain exactly one publisher job. It must wait for every required
producer and fail closed if any producer is absent, skipped, duplicated,
unexpected, or inconsistent. Optional best-effort release artifacts are not
published. The verifier treats every configured producer policy as required,
so the workflow's `--required-producer` list cannot silently omit a new policy.
