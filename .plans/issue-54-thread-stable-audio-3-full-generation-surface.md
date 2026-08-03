---
issue: 54
url: https://github.com/brxs/lsdjai/issues/54
title: "Thread Stable Audio 3's full generation surface through the pipeline"
date: 2026-07-10
baseline: 76466aa
status: complete
---

# Plan: Thread Stable Audio 3's full generation surface (#54)

## Progress

- [x] Read the issue and inspect the current backend, frontend, Rust capture paths,
  tests, project rules, and ADR-0012.
- [x] Verify the currently pinned SA3 CLI flags and defaults.
- [x] Settle the three product/contract decisions in **Decisions**.
- [x] Mark this plan implementation-ready.
- [x] Implement the SA3 CLI adapter and exact argv/init-file regression tests.
- [x] Implement JSON/multipart HTTP parsing and trust-boundary validation.
- [x] Add automated and API-level model-loaded verification.
- [x] Run the model-loaded verifier through JSON and multipart; machine verdict
  passes (fixed seed exact, inpaint concentration 12.445x).
- [x] Run `just check` after final hardening: 174 backend, 572 frontend, and
  308 Rust tests pass; Ruff, ESLint, TypeScript, Clippy, and format gates pass.
- [x] Run `npx tsc -p tsconfig.app.json --noEmit` from `frontend/`.
- [x] Complete the listening and unchanged-app regression sections in
  `docs/issue-54-checklist.md`.

## Problem

`backend/lsdj/sa3.py::generate()` currently constructs one fixed text-to-audio
argv. `/api/generate` accepts only `prompt`, `seconds`, and `kind`, and every
frontend caller sends that same shape. The pinned SA3 MLX CLI already supports
audio-to-audio, inpainting, negative prompting/CFG/APG, variation strength, and
fixed seeds, but none of those values can cross LSDJ's pipeline.

The implementation must light up that existing model surface without changing
the normal pad, sample, or track generation path. The production workflows that
will use it later (reskin, variations, loop-to-track, transitions, palette UI)
remain separate issues.

## Baseline facts

- The repository is on clean `main` at `76466aa` when this plan was drafted.
- `sa3-pin.json` now pins `brxs/stable-audio-3` at `36ef977`, not the older
  `bccf5b7` named in issue 54. No pin bump is needed.
- The current CLI takes `--apg` as a **float in `[0, 1]`**, not a boolean flag.
  Its defaults are `init_noise_level=1.0`, `cfg=1.0`, and `apg=1.0`; omitting a
  seed chooses one in `0..2^31-1` and prints it.
- The CLI reads 44.1 kHz, 16-bit PCM WAV natively. Other WAV shapes fall back to
  `ffmpeg`, which LSDJ does not bundle and must not become a hidden runtime
  requirement.
- Native deck capture already returns up to 30 seconds of 48 kHz interleaved
  stereo float PCM through `DeckChannel.captureSample()`. Loop slots can be read
  in Rust through `Host::read_loop_slot`, and library samples are already WAV
  bytes. No new audio recorder is needed.
- FastAPI/Starlette multipart parsing needs `python-multipart`; it is not in the
  current backend environment or lockfile.

## Decisions

1. **SA3 only.** Extend `/api/generate`; leave the separate Magenta `/api/render`
   worker unchanged because SA3's init latent, CFG, APG, and inpaint concepts do
   not map to that worker's contract.
2. **Multipart init audio.** Keep ordinary calls as the existing JSON request;
   when init audio is present, send multipart/form-data with a `request` JSON
   field and one `init_audio` WAV file. Use the recommended 16 MiB ceiling: the
   owner selected multipart without selecting the 160 MiB full-track variant,
   and 16 MiB comfortably bounds the short source material this plumbing is for.
3. **API-only verification for now.** Do not add app controls or change frontend
   generation calls in issue 54. Prove every mode through `/api/generate` with a
   script and model-loaded checklist. Production frontend source selection and
   controls remain for the dependent feature issues.

## Proposed contract

### HTTP request

Keep `/api/generate` backward compatible and accept two content types:

1. `application/json`: the existing object, widened with optional SA3 fields.
   Every current caller remains on this path, and a request with no new fields is
   identical to today's request.
2. `multipart/form-data`: exactly one `request` field containing the same JSON
   object and exactly one `init_audio` file containing WAV bytes. The client
   supplies the multipart boundary and must not hard-code `Content-Type` without
   that boundary.

The metadata shape is:

```json
{
  "prompt": "warm dub loop",
  "seconds": 8,
  "kind": "music",
  "init_noise_level": 0.6,
  "inpaint_range": [2, 4],
  "negative_prompt": "vocals",
  "cfg": 4,
  "apg": 1,
  "seed": 12345
}
```

All new keys are optional. `init_audio` exists only as the multipart file, never
inside the JSON. JSON remains useful for deterministic text-to-audio and CFG
without a source clip.

### Boundary validation

Centralise request parsing/validation so JSON and multipart apply exactly the
same rules before calling `sa3.generate()`:

- Existing `prompt`, `seconds`, and `kind` checks remain unchanged.
- `negative_prompt`: string, stripped, at most `MAX_PROMPT_LENGTH`; blank becomes
  absent. Reject it when effective CFG is `1.0`, where the CLI would silently
  ignore it.
- `init_noise_level`: non-bool finite number in `[0.01, 5.0]`. The lower bound is
  the CLI's measured hard floor; `5.0` is a conservative safety ceiling over the
  documented overshoot mode.
- `cfg`: non-bool finite number in `[-20.0, 20.0]`. This preserves both
  push-toward and push-away modes while bounding pathological values.
- `apg`: non-bool finite number in `[0.0, 1.0]`; reject an explicitly supplied
  APG when effective CFG is `1.0`, where it has no effect.
- `seed`: non-bool integer in `0..2^31-1`, matching the CLI's own random-seed
  domain and avoiding the negative key that current MLX rejects.
- `inpaint_range`: a two-element JSON array of finite numbers satisfying
  `0 <= start < end <= seconds`; requires `init_audio`.
- `init_audio`: non-empty WAV, no more than 16 MiB, and a natively supported CLI
  shape (44.1 kHz, 16-bit PCM, mono or stereo). Parse the RIFF/WAVE header rather
  than trusting the filename or MIME type. Reject multipart requests with
  missing/duplicate/unexpected parts. Limit the metadata field separately to
  64 KiB. This avoids turning the CLI's optional `ffmpeg` fallback into an
  undeclared app dependency.
- Reject unsupported content types and malformed JSON/form data with 422.
  Return 413 for an oversized init-audio request. Keep CLI-unavailable at 503 and
  CLI failure/timeout at 502 exactly as today.

Use an early `Content-Length` check when present, but also enforce the file limit
while reading the upload because chunked requests can omit or lie about it.
Starlette's upload file can spool to disk; only the final bounded bytes object is
passed into `sa3.generate()`.

### Python generation API

Widen the function with keyword-only options so existing positional calls remain
valid and call sites name every new axis:

```python
async def generate(
    prompt: str,
    seconds: float,
    kind: str,
    *,
    init_audio: bytes | None = None,
    init_noise_level: float | None = None,
    inpaint_range: tuple[float, float] | None = None,
    negative_prompt: str | None = None,
    cfg: float | None = None,
    apg: float | None = None,
    seed: int | None = None,
) -> bytes:
```

When present, write init bytes to `init.wav` in the existing temporary directory.
Build the current base argv in its current order, then append only explicitly set
flags in one documented order. `--apg` receives its numeric value. Keep temporary
file work and subprocess execution inside `_generation_lock`, retain the
length-scaled timeout, and continue returning the output WAV bytes.

The regression invariant is the exact argv list: with every new option omitted,
the spawned argv must equal today's list element-for-element.

## API verification surface

Add one developer script that drives the actual FastAPI route using multipart,
not `sa3.generate()` directly. It accepts a compatible source WAV, prompt, output
directory, optional inpaint range, and optional negative prompt; it then submits:

1. a seeded text-only baseline as JSON;
2. audio-to-audio at a sub-1.0 noise level as multipart;
3. an inpaint request as multipart;
4. a negative-prompt request with non-default CFG/APG;
5. two otherwise identical fixed-seed requests.

Write every response WAV for audition and print hashes/difference measurements.
Fail when the repeated seed does not produce byte-identical output, when the
audio-to-audio result does not differ from both source and text-only baseline,
or when inpaint changes are not concentrated inside the requested region (with
a documented codec guard band).

Use `fastapi.testclient.TestClient` against `controller.app` so the script crosses
the real HTTP parser, validation, error mapping, generation lock, subprocess, and
response path without requiring a separately managed server. A generated SA3
sample from `~/Documents/LSDJ/generated_samples` is a convenient compatible
44.1 kHz PCM16 source. Deck-capture/loop-slot conversion and app selection remain
deferred with the frontend UX.

## Implementation steps

1. **Dependencies and constants** (`backend/pyproject.toml`, `backend/uv.lock`,
   `backend/lsdj/sa3.py`): add `python-multipart`; define validation/domain
   constants beside the existing prompt/length limits, including the selected
   init-audio ceiling.
2. **CLI adapter** (`backend/lsdj/sa3.py`): add keyword-only options, write the
   optional init WAV inside the locked temporary directory, and append the seven
   optional CLI flags without disturbing the base argv or timeout/error mapping.
3. **HTTP parser and validation** (`backend/lsdj/controller.py`): factor one
   metadata validator used by JSON and multipart; enforce content type, form
   shape, metadata/file limits, cross-field requirements, and status codes; pass
   validated values to `sa3.generate()` by keyword.
4. **Backend contract tests** (`backend/tests/test_sa3.py`,
   `backend/tests/test_controller.py`): pin unchanged argv, every optional flag,
   uploaded file bytes, valid JSON/multipart requests, every type/range/order
   rejection, 413 handling, and unchanged 502/503 behaviour.
5. **Contract documentation** (`docs/adr/0012-...md`): amend ADR-0012 with the
   optional CLI surface, dual JSON/multipart request contract, body bound, and
   explicit statement that normal generation is unchanged. No new ADR is needed.
6. **API/model verification** (`backend/scripts/verify_sa3_surface.py`, new):
   submit JSON and multipart requests through `TestClient`, save auditionable
   outputs, and enforce the seeded/difference/locality checks above.
7. **Human checklist** (`docs/issue-54-checklist.md`, new): run the API verifier,
   compare its outputs by ear, exercise boundary failures, and confirm existing
   app pad/sample/track generation still works unchanged.
8. **Verification pass**: run `just check`, then run the model-loaded verifier
   and complete the checklist on a machine with SA3 installed.

## Tests and acceptance mapping

- **Default path unchanged:** exact subprocess argv test plus a controller test
  that pins the existing no-options JSON body and generated call.
- **All CLI values forwarded:** subprocess stub records argv and copies/validates
  `init.wav` before the temporary directory disappears.
- **Trust boundary:** table-driven controller tests cover booleans masquerading
  as numbers, NaN/infinity, each boundary, bad range ordering, cross-field
  requirements, bad WAV/multipart shape, and oversized uploads.
- **Audio-to-audio differs:** model-loaded verifier compares the source,
  text-only baseline, and seeded variation, then writes all three for audition.
- **Inpaint is local:** verifier computes inside-mask and outside-mask deltas with
  a small boundary guard for codec receptive-field bleed; checklist confirms by
  ear.
- **Seed reproduces:** verifier runs the same complete request twice and requires
  byte-identical WAV output/hash.
- **HTTP-to-CLI proof:** the API verifier covers multipart parsing through the
  loaded model and writes the audible results; frontend plumbing is deferred by
  decision.
- **Failure compatibility:** controller tests retain 503 for missing checkout and
  502 for CLI failure/timeout; oversized and unsupported WAVs fail before launch.

## Affected areas

- `backend/lsdj/sa3.py`
- `backend/lsdj/controller.py`
- `backend/tests/test_sa3.py`
- `backend/tests/test_controller.py`
- `backend/pyproject.toml`, `backend/uv.lock`
- `backend/scripts/verify_sa3_surface.py` (new)
- `docs/adr/0012-generated-pads-via-a-spawned-sa3-mlx-subprocess.md`
- `docs/issue-54-checklist.md` (new)

Deliberately untouched under the decided scope: `/api/render`, the Magenta
worker/engine, all frontend code, Rust audio capture internals, model
installation/pinning, MIDI mappings, MCP, and persistence.

## Risks

- **Multipart parser dependency:** include it in the lockfile and frozen sidecar;
  exercise multipart through `TestClient` so packaging does not silently omit it.
- **Memory copies:** multipart construction, Starlette upload parsing, final
  `bytes`, and the subprocess file write each touch the audio. The hard ceiling
  bounds this; avoid base64's additional expansion/copies.
- **Source format:** the API deliberately accepts the pinned CLI's native WAV
  shape so verification does not depend on system `ffmpeg`. Future deck/loop
  sources need a conversion adapter, which is deferred with their UX.
- **Inpaint locality:** encoder/decoder receptive fields may cause small changes
  around mask edges. The verifier needs a documented guard band rather than an
  unrealistic byte-equality assertion outside the range.
- **Model cost:** CFG doubles the DiT forward work when `cfg != 1`; the verifier
  runs requests serially through the existing single generation lock.

## Definition of done

- The three planning decisions are recorded as final in this plan.
- Every new parameter crosses HTTP -> `sa3.generate` -> pinned CLI through the
  API verification script; frontend plumbing is explicitly deferred.
- Default pad/sample/track generation sends the same request and spawns the same
  argv as before.
- Invalid or oversized input is rejected before subprocess launch with the
  documented status; 502/503 behaviour is unchanged.
- Automated checks pass, the API/model verifier passes, and a human completes
  `docs/issue-54-checklist.md` including the unchanged app-generation regression.
