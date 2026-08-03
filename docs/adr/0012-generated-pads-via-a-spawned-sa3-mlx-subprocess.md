# 0012. Generated pads via a spawned sa3_mlx subprocess

- **Status:** Accepted
- **Date:** 2026-06-12
- **Deciders:** Daniel Peter
- **Extended by:** ADR-0028 (Stable Audio 3 LoRA finetunes via the MLX path),
  issue #54 (audio-to-audio, inpainting, CFG/APG, and fixed seeds), issue #59
  (Generate-tab text steering and reproducible song recipes)

## Context

M18 adds "generate to pad": text-to-audio one-shots and bar-quantised
loops filling the M13 freeze-pad slots. The obvious engine is Stable
Audio 3's open-weight small models (Stability AI Community License),
but the reference package is unusable here: `stable-audio-tools` pins
Python `>=3.10,<3.11` against the backend's 3.13 and hard-pins
`torch==2.7.1` beside our MLX stack.

Stability ships its own Apple-Silicon port — `optimized/mlx` in the
[`stable-audio-3`](https://github.com/Stability-AI/stable-audio-3)
repo (`sa3_mlx`) — pure MLX with four runtime deps (`mlx`, `numpy`,
`sentencepiece`, `huggingface_hub`), driven by a CLI that writes a
stereo 44.1 kHz 16-bit WAV trimmed to an exact float `--seconds`.
Measured on this machine (Apple M5, 16 GB, 2026-06-12, fresh process
per run, model load included):

| model | clip | wall | peak RSS |
| --- | --- | --- | --- |
| sm-sfx | 3 s | 1.30 s | 1.06 GB |
| sm-sfx | 6 s | 0.97 s | 1.45 GB |
| sm-music | 7.74 s (4 bars @ 124) | 1.12 s | 1.10 GB |
| sm-music | 30 s | 1.52 s | 1.47 GB |

Quality cleared the go/no-go by ear (both models, all four clips).
One length limit surfaced in the integration listen: **sm-music breaks
up below ~4 s** — a 3.6 s request came back garbled from the CLI and
the API alike while 7.2 s (and the probe's 7.74 s) were clean — so the
frontend floors generated-loop requests at 7 s
(`GENERATED_LOOP_MIN_SECONDS`), in whole bars when the tempo is
locked; more bars beat broken audio. Weights (~2.3 GB for sm-sfx +
sm-music + shared codec + T5Gemma) auto-download from HF into the
cache and symlink into the checkout.

Decisions needed: how the backend runs it, where the checkout lives,
and what happens when it's missing or fails.

## Decision

- **Spawn, never import.** One backend module owns the contract and
  runs `.venv/bin/python scripts/sa3_mlx.py --prompt … --dit … --out …`
  as a short-lived subprocess per generation, reading back the WAV.
  Nothing in the backend imports `sa3_mlx` code — an isolation rule
  stronger than ADR-0002's one-importer rule, available because the
  measured whole-process wall time (~1–1.5 s, load included) makes a
  resident model pointless.
- **The venv's python, not `uv run` and not `./sa3`.** `uv run` walks
  up to the checkout's *repo root* and resolves the torch project
  (measured: it built an ephemeral torch env and crashed on
  `import mlx`); the `./sa3` wrapper exists for humans and may prompt.
  The module invokes the interpreter the checkout's installer created.
- **The checkout lives outside this repo, pinned.** A small resolver takes
  `$SA3_MLX_HOME` first, then the in-app model manager's app-data checkout at
  `~/Library/Application Support/LSDJ/stable-audio-3` (each expected to
  contain `optimized/mlx/.venv`). `sa3-pin.json` records the repository and
  commit; issue #54 is validated against the current `36ef977` pin. The
  resolver is pure and unit-tested.
- **Generations are serialised** behind a single-slot semaphore: peak
  RSS is ~1.5 GB transient and two decks can request at once; one at a
  time keeps worst-case memory flat next to two model workers on a
  16 GB machine, and a queued request still lands in ~3 s.
- **Absence degrades, never crashes.** No checkout → the endpoint
  answers 503 with a setup hint and the decks are untouched; a failing
  CLI → 502 with the captured stderr tail; prompt/seconds validation
  (non-blank, bounded length and duration) → 422 at the trust
  boundary.
- **The SA3 generation surface is one backward-compatible endpoint.** Existing
  text-only calls remain `application/json` with `{prompt, seconds, kind}` and
  spawn the exact same argv. Optional JSON fields carry `init_noise_level`,
  `inpaint_range`, `negative_prompt`, numeric `cfg`/`apg`, and `seed`. A request
  with source audio uses `multipart/form-data`: one `request` field containing
  that JSON and one `init_audio` WAV file. The upload is bounded at 16 MiB both
  before and during body consumption; metadata is bounded at 64 KiB. Init audio
  is 44.1 kHz PCM16 mono/stereo, the format the pinned CLI reads natively, so the
  packaged app never acquires an undeclared `ffmpeg` dependency. All numeric,
  type, ordering, and cross-field rules are rejected at the HTTP boundary before
  the generation lock or subprocess. `python-multipart` is the only added
  backend runtime dependency.
- **Optional flags do not perturb defaults.** `sa3.generate()` accepts the new
  controls as keyword-only values, writes an optional `init.wav` inside the
  existing locked temporary directory, and appends only explicitly supplied
  flags. Inpainting requires init audio; negative prompt and APG require CFG
  other than 1, where the CLI actually runs its unconditional branch. A seed is
  bounded to the CLI's non-negative 31-bit random domain. `/api/render` remains
  the separate Magenta worker and gains none of these SA3-specific controls.
- **The product exposes progressive text steering only where songs are authored.**
  Media Explorer's Generate tab has Basic and Advanced modes. Basic omits the
  negative-prompt and guidance fields but sends a webview-minted random seed so
  a good take can be reproduced later. Advanced exposes a concept-oriented
  negative prompt, CFG, APG, and random/fixed seed for the medium Track DiT.
  Effective seeds are saved with the generation recipe. Switching back to Basic
  pauses the Advanced draft without applying it. Magenta, Samples, deck
  generation, audio-to-audio, and inpainting remain outside this UI boundary.
- **Magenta is the third engine, as its own worker.** A dedicated
  render process — the deck worker loop reused verbatim; a render
  worker is a deck worker that never receives `play` — serves
  `render_clip` commands, so pads can speak the booth's own sound
  world *while both decks stream*. It spawns lazily on the first
  `/api/render` call (a resident third model, ~2 GB for `mrt2_small`,
  is only paid for by sessions that use it; the first request carries
  the model load inside its pending state) and respawns if found
  dead. Renders use a fresh generation state per clip; results travel
  a dedicated clip queue, matched to requests by id with stale
  answers discarded, and the endpoint returns a float32 WAV (IEEE
  format 3 — no quantisation between the worker and
  `decodeAudioData`). A render that misses its deadline gets its
  worker killed, so the next request respawns clean instead of
  inheriting the wedge. The deck workers, which carry no clip queue,
  drop a misrouted render with a warning as a safety net — nothing
  routes renders at them anymore.

## Consequences

- Easier: no SA3 model/runtime dependency enters the backend venv; generation
  memory is transient (nothing competes with the deck workers between runs).
  The small multipart parser is a normal locked backend dependency. Upgrading
  sa3_mlx is update the pin + re-validate, not a model-runtime lockfile dance.
- The subprocess spawn + model load (~0.5 s of the wall) is paid on
  every generation. Accepted: at 1–1.5 s total, a resident-model
  server is complexity without a user-visible win.
- An un-versioned upstream: the CLI flags are the contract and a
  rebase upstream can break it. Mitigated by the pinned commit, one
  owning module, and errors that name the problem.
- Multipart source audio creates bounded copies in the HTTP client, parser,
  final bytes object, and temporary file. The 16 MiB file/whole-body ceilings
  keep that exposure fixed; base64 JSON was rejected because it adds about 33%
  expansion and another large text copy.
- A session that touches the Magenta engine holds a third model
  resident (~2 GB) until shutdown. Accepted: it is lazy, and the
  alternative — borrowing a deck's worker — made generation
  availability depend on transport state, a coupling the booth could
  feel.
- First-ever generation on a fresh machine pays the HF weight
  download (~2.3 GB) inside the request timeout; the 503 setup hint
  documents pre-warming via one manual `sa3` run.
- macOS/Apple-Silicon only — already a standing assumption of this
  project (MLX deck workers).

## Alternatives considered

- **`stable-audio-tools` in the backend venv** — impossible today:
  Python `<3.11` pin against 3.13, `torch==2.7.1` beside MLX.
- **A resident generator worker (the deck-worker pattern)** — keeps
  the model warm to save ~0.5 s per run, at the cost of a supervised
  process, a command protocol, and 1+ GB held permanently. The
  measured numbers say cold spawn is fast enough.
- **Vendoring `models/defs` into our backend** — same-venv imports
  with no subprocess, but adopts ~100 KB of model code we'd have to
  maintain and re-validate against every upstream fix; the CLI surface
  is smaller than the Python one.
- **Hosted API (Stability platform / fal.ai)** — adds a key, a cost,
  and a network dependency to a deliberately local-first instrument.

<!-- Status values: Proposed | Accepted | Rejected | Deprecated |
     Superseded by ADR-NNNN -->
