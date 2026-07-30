---
issue: 59
url: https://github.com/protocol-works/lsdj/issues/59
title: "Negative-prompt and guidance steering for generation"
date: 2026-07-29
baseline: 10320b1
status: complete
---

# Plan: Advanced SA3 prompting and reproducible generation (#59)

## Progress

- [x] Read issue #59 and dependency #54 through the Keychain-backed `gh` CLI.
- [x] Inspect the merged LoRA generation UI, every current generation call site,
  the `/api/generate` contract, SA3 CLI adapter, song/sample registries, i18n,
  tests, and applicable ADRs.
- [x] Verify CFG/APG/negative-prompt behavior against the exact pinned upstream
  SA3 commit (`0385302`).
- [x] Settle the Basic/Advanced behavior, default-preservation rule, control
  semantics, recipe shape, and scope boundary below.
- [x] Confirm the owner decisions: complete text steering in the Generate tab
  only, paused Advanced parameters in Basic, full song recipes, curated CFG/APG,
  and reproducible client-generated seeds.
- [x] Implement the Generate-tab generation model and controls.
- [x] Integrate the Generate tab and persist/recall song recipes.
- [x] Complete automated and responsive visual verification.
- [x] Complete the subjective ratings in the real SA3 by-ear checklist. A
  controlled Medium-model set has been generated from cached weights. Avoid,
  Guidance Off/On, and APG passed human review. CFG 7.0 failed due to severe
  source clipping; the UI ceiling is now 4.0 and its replacement endpoint passed
  human review as clean and substantially stronger than CFG 1.1.

## Implementation goal prompt

> Implement issue #59 from this plan end to end. Add Basic | Advanced SA3 text
> steering to the Generate tab only, preserve Basic and all other generation
> surfaces exactly, persist and recall full versioned song recipes, verify the
> behavior with focused tests, type-checking, `just check`, and the SA3 listening
> checklist, and update this plan's progress as each phase is completed.

## Verification log

- 2026-07-29: focused frontend tests passed (163 tests across generation,
  Media Explorer, design-system controls, persistence, LoRAs, and deck scope).
- 2026-07-29: `npx tsc -p tsconfig.app.json --noEmit` and frontend ESLint passed.
- 2026-07-29: song recipe Rust tests passed, including legacy and unknown-version
  registry rows.
- 2026-07-29: `just check` passed: 209 backend, 617 frontend, 227 shell, and 109
  engine tests, plus Ruff, ESLint, TypeScript, and Clippy.
- 2026-07-29: local-browser smoke test passed at the default viewport and 760 px
  responsive width. Advanced stacked without horizontal overflow; chips,
  automatic guidance, ARIA state, and the Magenta unavailable state were
  exercised. Only the existing Three.js Clock deprecation warning appeared.
- 2026-07-29: generated seven controlled 12-second Medium/SAME-L clips for
  negative prompt, Guidance Off/On, CFG 1.1/7.0, APG 0.0/1.0, and fixed-seed
  comparisons. The repeated fixed-seed take was byte-identical (SHA-256
  `95ab708a676d9089eed9e1b4140d0f27099490bfe81fe5a7d412dbcfebefe1c6`).
  Objective onset metrics moved in the expected direction for Avoid = `drums`;
  the subjective musical verdicts remain in `docs/issue-59-checklist.md`.
- 2026-07-29: human review passed Avoid = `drums`, Guidance Off/On, and APG,
  but rejected CFG 7.0 for severe clipping. PCM analysis found 18.1% of its
  samples within 1% of full scale. A same-recipe sweep measured 0.8% at CFG 4.0,
  4.5% at 5.0, and 8.1% at 6.0; the curated UI ceiling was reduced to 4.0.
- 2026-07-29: after lowering the CFG ceiling, focused generation/UI tests passed
  (55 tests), the app type-check passed, and `just check` passed again: 209
  backend, 618 frontend, 227 shell, and 109 engine tests, plus Ruff, ESLint,
  TypeScript, and Clippy.
- 2026-07-29: human review accepted CFG 4.0 as clean and much stronger than CFG
  1.1. All subjective and automated acceptance checks are complete.
- 2026-07-29: moved Basic/Advanced beside LoRA and made both the mode toggle and
  steering panel SA3-only. Focused UI tests (45), TypeScript, ESLint, and a local
  browser check passed for both Track and Magenta layouts.
- 2026-07-29: aligned the LoRA and Generation mode labels above their controls,
  removed Reset advanced, and added accessible `(?)` ELI5 tooltips for CFG/APG.
  Focused UI tests (49), TypeScript, ESLint, and a browser focus/overflow check
  passed.
- 2026-07-29: renamed Use recipe to Reuse settings and made every Basic SA3
  take send and save an explicit random seed. Reusing a Basic take opens
  Advanced with Guidance off and that seed fixed. Focused tests (59) and
  `just check` passed: 209 backend, 619 frontend, 227 shell, and 109 engine
  tests, plus Ruff, ESLint, TypeScript, and Clippy.
- 2026-07-29: fixed persisted Basic recipe recall: legacy null-valued CFG/APG
  fields now normalize to Guidance off, and new Rust registry rows omit
  absent guidance fields. Regression coverage verifies Guidance can be enabled
  and CFG/APG edited after recall. Focused tests (55) and `just check` passed.
- 2026-07-29: made Guidance independently switchable after recalling an
  Advanced recipe with Avoid concepts. Turning it off preserves the Avoid draft
  while omitting Avoid/CFG/APG from generation; turning it on restores the
  controls. Focused tests (56) and `just check` passed: 209 backend, 620
  frontend, 227 shell, and 109 engine tests.
- 2026-07-29: grouped Avoid/CFG/APG beneath a Guidance fieldset and moved Seed
  below a divider. Guidance remains interactive while the entire steering body
  becomes disabled and visibly dimmed. Focused tests (56), TypeScript, ESLint,
  and wide/760 px browser checks passed without horizontal overflow. The full
  `just check` passed: 209 backend, 620 frontend, 227 shell, and 109 engine tests.
- 2026-07-29: addressed branch-review findings for recipe compatibility and
  paused Guidance help. Current recipes remain typed on write, while stored
  recipes are opaque on read so future shapes survive library reconciliation;
  CFG/APG `(?)` help stays usable while steering edits are disabled. Focused
  tests (60) and `just check` passed: 209 backend, 620 frontend, 227 shell, and
  109 engine tests. Per product decision, early LoRA recall behavior remains
  unchanged.

## Problem

Issue #54 already threaded `negative_prompt`, `cfg`, `apg`, `seed`, init audio,
variation strength, and inpainting through `/api/generate` to the pinned SA3
CLI. It deliberately shipped no app UI. The current product surfaces still send
only prompt, duration, engine, and the newly merged LoRA stack:

- Media Explorer → Generate: full SA3 Track or Magenta song;
- Media Explorer → Samples: short SA3 SFX/Music clip;
- deck A and deck B: SA3 SFX/Music or Magenta pad.

That leaves SA3 prompt-only and additive. A request for a bassline can still
return drums, vocals, or melody, and there is no way to trade diversity against
prompt adherence or reproduce a useful take. The backend already has the trust
boundary; the missing layer is a coherent product model, progressive disclosure,
request construction, and persisted provenance.

The existing song persistence is also too thin for the issue's recipe
requirement. `SongEntry` stores only the prompt and model. LoRA choices and SA3
steering disappear after the WAV is saved, and there is no action that restores
a saved generation into the Generate form.

## Pinned SA3 facts that shape the UI

- `cfg = 1.0` means guidance is **off**, runs one conditional forward pass, and
  ignores a negative prompt. Any value other than `1.0` costs roughly twice the
  DiT work because SA3 runs conditional and negative/unconditional branches.
- `cfg > 1.0` pushes toward the positive prompt and away from the negative
  branch. A negative prompt should name the unwanted concept (`drums`), not
  describe its absence (`no drums`).
- APG is a number in `[0, 1]`, not a boolean: `0` is vanilla CFG and `1` is full
  adaptive projected guidance, which reduces over-saturation at high CFG. It
  only has an effect when CFG is active.
- The pinned upstream UI exposes CFG over `0–10`; LSDJ's backend safely accepts
  `[-20, 20]`. The product UI should use a curated musical range rather than
  exposing pathological/inversion values merely because the API can validate
  them.
- An omitted seed is chosen inside the CLI and is not returned in the WAV
  response. A client-chosen random seed is required if a saved recipe should be
  able to reproduce its exact stochastic input.
- Audio-to-audio and inpainting are different authoring modes: they need a
  compatible source WAV, optional conversion, multipart upload, and a time-range
  editor. They are not ordinary prompt controls.

## Chosen product direction

Add **Basic | Advanced** to Media Explorer's **Generate tab only** and keep its
current surface as Basic. Advanced reveals the complete *text-prompt steering*
surface for the SA3 Track engine:

- **Avoid** — free text plus one-tap `No vocals`, `No drums`, `No cymbals`, and
  `No melody` chips;
- **Guidance** — off by default; when on, an adherence/diversity slider mapped
  to CFG;
- **Guidance stabilization (APG)** — a `[0, 1]` slider, available only while
  guidance is on;
- **Seed** — random-per-take or fixed;
- the existing contextual **LoRA** control remains visible in Basic and
  Advanced because it is already part of the current generation surface.

Samples and both deck generation panels remain exactly as they are: no mode
toggle, no Advanced controls, and no payload or persistence changes. Their
existing LoRA controls continue to work independently.

The selected Generate-tab mode is a persisted UI preference, defaulting to Basic
on first launch and using the existing app-settings/localStorage path. The one
Advanced draft belongs to the Generate form and survives tab changes while Media
Explorer remains mounted.

Basic is an execution mode, not merely CSS disclosure:

- Basic always omits `negative_prompt`, `cfg`, and `apg`, preserving the current
  unguided SA3 behavior. It sends a newly minted seed so every good Basic take
  can be reproduced and promoted into Advanced.
- Switching to Basic keeps that form's Advanced draft in memory but pauses it;
  hidden values never silently affect a generation. Returning to Advanced
  restores the draft without adding an explanatory label to the Basic surface.

For Magenta, the Basic/Advanced toggle and SA3 steering panel are hidden. It
never sends SA3 fields to `/api/render` and does not discard the remembered SA3
mode or draft when the user switches back.

## UX contract

The media-drawer form should read approximately:

```text
Title       Prompt                         Engine   Length   [ Compose ]
LoRA        [ Off ▾ ]   Generation mode   [ Basic | Advanced ]  (SA3 only)

Advanced only (SA3):
Avoid       [ No vocals ] [ No drums ] [ No cymbals ] [ No melody ]
            [ concepts to keep out of the result........................ ]
Classifier-Free Guidance (CFG)
        (?) [ Off | On ]  Diversity ─────●──────── Adherence   3.0
Adaptive Projected Guidance (APG)
        (?) Vanilla CFG ─────────────────● Stabilized          1.0
Seed        [ Random each take ▾ ] [ 18420593 ]
```

At narrow drawer widths, label/control pairs become rows and chip groups wrap.

### Avoid behavior

- Chip labels describe the outcome (`No drums`) but insert/remove the canonical
  negative concept (`drums`) in the request draft. Do not send `no drums` to the
  negative branch, which would condition it on the absence of drums and then
  steer away from that concept.
- The free-text hint asks for unwanted concepts such as `vocals, cymbals`, not
  negated prose. Chips and text edit one underlying, comma-separated draft so
  state cannot disagree.
- Guidance is the explicit master switch for Avoid, CFG, and APG. While it is
  off, the Avoid input and chips are visibly disabled along with the sliders.
- Guidance can always be turned off. Doing so preserves but pauses the Avoid
  draft; the effective request and saved recipe omit Avoid/CFG/APG. Turning
  Guidance back on restores the draft and makes those controls editable.
- Trimming and maximum-length behavior mirror the positive prompt. The backend
  remains authoritative and returns the existing 422 errors for malformed data.

### CFG/APG behavior

- Guidance is off initially and after reset, represented by omission—not by
  sending `cfg: 1`—so the default request remains unchanged.
- Turning Guidance on starts at CFG `3.0`, the pinned upstream negative-prompt
  example. Expose a curated `[1.1, 4.0]`, step `0.1` range: left is looser/more
  diverse; right is more literal/stronger avoidance. The separate Off state is
  the honest representation of CFG `1.0`. Real Medium/SAME-L listening rejected
  7.0: its WAV hard-limited 18.1% of samples, while 4.0 reduced that to 0.8%.
- APG defaults to `1.0`, ranges `[0, 1]` in `0.1` steps, and is disabled when
  guidance is off. When guidance is on, send both `cfg` and `apg` explicitly so
  a recipe does not change behavior if an upstream CLI default changes later.
- The UI explains the approximate 2× DiT cost when guidance is active. It makes
  no wall-time promise because duration, model, machine, and LoRA stack vary.

### Seed behavior

- `Random each take` is the Advanced default, and Basic also uses an implicit
  random-per-take seed. At submit time, mint a 31-bit seed with Web Crypto, send
  it explicitly, and capture that exact value in the saved request snapshot.
  Each press gets a new value, including repeated Enter/click submissions
  batched before React renders.
- `Fixed` enables an integer field bounded to `0..2^31-1`. Invalid text disables
  generation with a localized inline explanation; it is never coerced or
  rounded silently.
- Recalling any SA3 recipe restores its used seed as Fixed and opens Advanced.
  A Basic take arrives with Guidance off, ready for optional steering from the
  same stochastic starting point.

## Shared frontend model and request seam

Create a small generation domain rather than adding more positional arguments
and duplicating JSON assembly in three places. The exact names may adjust during
implementation, but the ownership should be:

```ts
type GenerationMode = 'basic' | 'advanced'

type Sa3SteeringDraft = {
  negativePrompt: string
  guidance: 'off' | 'on'
  cfg: number
  apg: number
  seed: { mode: 'random' } | { mode: 'fixed'; value: string }
}

type Sa3RequestOptions = {
  negative_prompt?: string
  cfg?: number
  apg?: number
  seed?: number
  loras?: LoraChoice[]
}
```

- A pure builder turns `(mode, engine, draft, effective LoRA stack, takeSeed)`
  into validated request options. It is the single place that omits guidance
  options in Basic and all SA3 options in Magenta, enforces the CFG cross-field
  rules, and produces the effective recipe snapshot.
- A focused `postSa3Generate` client owns the Generate tab's `/api/generate`
  JSON construction and existing error-detail parsing. `/api/render` remains a
  separate Magenta path.
- Keep pending-generation snapshots immutable: title, authored prompt, engine,
  seconds, effective LoRAs, steering, and used seed are captured before the
  asynchronous request. Editing the form during generation must not change the
  metadata saved with the returning WAV.

## Versioned recipe persistence and recall

Add an optional, versioned `recipe` to song registry rows while retaining the
existing top-level `prompt` and `model` fields. Old registries and hand-added
files must continue to deserialize unchanged.

```ts
type GenerationRecipeV1 = {
  version: 1
  prompt: string
  engine: 'track' | 'magenta'
  seconds: number
  loras: { name: string; strength: number }[]
  sa3?: {
    negativePrompt?: string
    cfg?: number
    apg?: number
    seed?: number
  }
}
```

`NewSong` accepts the current typed `GenerationRecipe`, while persisted
`SongEntry` rows retain `recipe` as an optional opaque JSON value. The frontend
validates version 1 before recall. This prevents a future recipe shape from
making an older shell reject and rewrite the entire registry. Keep the versioned
recipe in the song-provenance module; Samples do not gain a dormant schema with
no UI consumer.

Recipe rules:

- Persist the **effective request snapshot**, not whatever happens to be in the
  form when the generation finishes.
- Preserve the authored prompt and selected song duration.
- Record only LoRAs that were effective for the selected SA3 kind. A recalled
  missing/deleted adapter is reported as unavailable and omitted from the next
  request; it is never silently substituted.
- Magenta may carry the common prompt/engine/duration recipe but never an `sa3`
  object.
- Basic SA3 recipes store `sa3: { negativePrompt: "", seed }` with CFG/APG
  absent. Advanced random mode stores the actual client-minted seed; fixed mode
  stores the chosen seed.

Add a localized **Reuse settings** action to generated-song rows that have a recipe.
It populates the Generate form's prompt, engine, duration, LoRA stack, and
steering. It leaves Title alone because title is artifact identity rather than a
generation parameter. Every SA3 recipe switches the presentation to Advanced
and restores the saved seed as Fixed; a Basic recipe has Guidance off. Legacy
rows remain playable and inspectable but do not show a fake recall action.

This fulfills recipe save/reload as an observable Generate-tab workflow, not
merely extra JSON that no interface consumes. Because the GitHub issue currently
says “pad recipe,” its acceptance wording should be updated to “generated-song
recipe” to match the chosen Generate-tab-only scope.

## Implementation plan

### Phase 1 — generation domain and client

- [x] Add shared types, defaults, bounds, Web-Crypto seed minting, effective
  option builder, and recipe-snapshot builder under a focused frontend
  `generation/` module.
- [x] Add a JSON-only `postSa3Generate` client over `getApiBaseUrl` and centralize
  current HTTP error parsing.
- [x] Keep `/api/render` separate and unable to accept SA3 option types.
- [x] Refactor Media Explorer's SA3 Track request through the new seam with Basic
  defaults first; preserve its unguided behavior, then add the explicit
  reproducibility seed. Leave Samples and deck callers untouched.

### Phase 2 — reusable progressive-disclosure controls

- [x] Add a design-system `SegmentedControl` (two-or-more exclusive options)
  with roving keyboard focus, arrow-key selection, visible focus, disabled
  states, and `aria` semantics; do not hand-roll a screen-local toggle.
- [x] Build `Sa3AdvancedControls` from existing `TextField`, `Slider`, `Select`,
  `Switch`/`Button`, and the new segmented control. It receives a draft and emits
  changes; it does not own API calls or persistence.
- [x] Implement Avoid chip/text synchronization, an explicit Guidance master
  switch, CFG/APG enablement, random/fixed seed validation, the
  guidance-cost hint, plain-language CFG/APG tooltips, and SA3-only control
  visibility.
- [x] Put all copy in `frontend/src/i18n/en.json`; chip labels and canonical
  negative concepts are separate keys/values where needed.
- [x] Add responsive styles through existing tokens in `ui.css` and `media.css`;
  verify wrapped chips and stacked controls in a small media drawer.

### Phase 3 — integrate the Generate tab

- [x] Own the Basic/Advanced preference and the one steering draft in Media
  Explorer; persist only the mode through `persistence.ts`.
- [x] Integrate the control beside the current prompt/engine/length/LoRA form in
  the Generate tab.
- [x] Capture one immutable effective request/recipe snapshot per submit.
- [x] Send Advanced fields only for SA3 engines; preserve Magenta payloads.
- [x] Keep the current Track/Magenta engine switch, length menus, pending-row
  behavior, auto-save, LoRA base filtering, stack cap, strength behavior, and
  contextual management panel unchanged.
- [x] Add regression tests proving Samples and both deck generation panels have
  no mode control and retain their exact current payloads.

### Phase 4 — persist versioned song recipes

- [x] Add Rust `GenerationRecipe`/SA3 steering/LoRA structs for validated writes
  with camelCase serde and backward-compatible defaults.
- [x] Let `NewSong` accept that current schema, retain `SongEntry.recipe` as
  opaque JSON for forward-compatible reads, and carry it unchanged through the
  existing binary meta frame and registry.
- [x] Update frontend track entry/pending/ready types so a take carries the
  immutable recipe through request, response, save, re-list, and row rendering.
- [x] Add defensive recipe validation/normalization at recall and continue to
  rely on `/api/generate` as the authoritative generation trust boundary.
- [x] Amend ADR-0013 with the optional versioned song-provenance schema and
  ADR-0012 with the product UI now consuming its text-steering fields. No new
  ADR is needed: this extends the two accepted boundaries without changing
  ownership.

### Phase 5 — recipe recall

- [x] Add **Reuse settings** to generated-song rows only when a versioned recipe is
  present.
- [x] Restore common fields, LoRAs, steering, fixed seed, and the appropriate
  Basic/Advanced mode into the Generate form without changing Title or starting
  a generation automatically.
- [x] Surface unavailable adapter names and out-of-date/unknown recipe versions
  honestly; keep the WAV load/preview/delete actions usable.
- [x] Ensure recall never loads a deck or changes a generation already in flight.

### Phase 6 — verification and documentation

- [x] Add pure unit tests for option building, exact default omission, CFG/APG
  cross-field behavior, chip concepts, seed bounds/randomness, and recipe
  snapshots.
- [x] Add component/accessibility tests for Basic/Advanced, keyboard operation,
  focus, SA3-only visibility, and hidden-draft restoration.
- [x] Extend Media Explorer tests to pin exact Basic, Advanced, Magenta, LoRA,
  saved-recipe, and recall payloads; retain deck/Samples regression assertions.
- [x] Add Rust tests for new song-row round trips, old rows without `recipe`,
  unknown future recipe versions, and reconcile behavior.
- [x] Add `docs/issue-59-checklist.md` for real SA3 listening and layout checks.
- [x] Run focused tests while iterating, then `npx tsc -p tsconfig.app.json
  --noEmit` from `frontend/` and `just check` from the repository root.

## Acceptance mapping

- **Named element is suppressed:** by-ear checklist compares same prompt, seed,
  engine, length, LoRAs, CFG, and APG with only the negative prompt changed to
  `drums`; the negative take must be materially drum-lighter.
- **Adherence/diversity moves visibly and audibly:** the enabled slider exposes
  the numeric CFG value and endpoint labels; same-seed low/high CFG takes are
  auditioned for variation versus fidelity. Off omits CFG and reproduces the
  current behavior.
- **Recipe saves and reloads:** the song registry round-trips
  avoid/CFG/APG/seed and LoRAs; UI tests recall them; the manual checklist
  restarts the app, recalls a saved recipe, and verifies the populated controls
  before re-generating.
- **Backend bounds:** issue #54's existing controller table tests already cover
  CFG `[-20,20]`, APG `[0,1]`, negative-prompt length/type, seed, and cross-field
  requirements. Keep those green; do not duplicate a second backend contract.
- **i18n/house style/type-check:** all copy is keyed, controls use design-system
  primitives/tokens, the real app tsconfig runs clean, and `just check` passes.

## Likely affected areas

- New shared frontend generation types/client/hook/control under
  `frontend/src/generation/`.
- `frontend/src/persistence.ts` and tests.
- `frontend/src/media/MediaExplorer.tsx`, `frontend/src/media/media.css`, and
  `frontend/src/media/MediaExplorer.test.tsx`.
- `frontend/src/ui/SegmentedControl.tsx`, its test, and `frontend/src/ui/ui.css`.
- `frontend/src/i18n/en.json`.
- `src-tauri/src/songs.rs` and its tests.
- `docs/adr/0012-generated-pads-via-a-spawned-sa3-mlx-subprocess.md`,
  `docs/adr/0013-playback-decks-play-decoded-tracks-loading-decides-the-mode.md`,
  and new
  `docs/issue-59-checklist.md`.

`backend/lsdj/sa3.py` and `backend/lsdj/controller.py` should not require
production changes for #59: #54 already shipped the needed options and
validation. Their existing tests remain part of the completion gate. Deck
generation, Samples, `useDeck`, `DeckColumn`, `audio/types.ts`, and
`src-tauri/src/samples.rs` are deliberately unchanged beyond regression tests.

## Risks and mitigations

- **Hidden controls affect sound:** Basic actively omits Avoid/CFG/APG; its only
  extra field is an independently minted reproducibility seed. Paused Advanced
  drafts never ride a Basic request invisibly.
- **Negative-chip wording steers the wrong way:** display `No drums`, but send
  the concept `drums`; cover the separation with pure tests and the listening
  checklist.
- **CFG makes generation slower:** disclose the approximate 2× DiT work when it
  is enabled and keep it off in Basic/default Advanced state.
- **Async form edits corrupt provenance:** snapshot request and recipe before
  launch, keyed to the existing pending generation id.
- **Random mode cannot be reproduced:** every SA3 song take mints/sends its own
  seed and saves the used value; recall restores it as Fixed.
- **Stale LoRA recipes:** resolve recalled names against the installed,
  base-compatible library and report omissions; never substitute by slug.
- **Registry compatibility:** make `recipe` optional/defaulted and retain the
  old top-level fields so legacy rows and hand-added audio stay loadable.
- **Advanced UI overwhelms the drawer:** keep Basic visually identical to the
  current Generate form and include small-width visual checks.
- **Curated UI and wider API diverge:** define UI bounds once in the frontend,
  document that the backend intentionally accepts a wider expert/API domain,
  and leave the server authoritative.

## Deliberate follow-up: source-audio generation

Do not put audio-to-audio, `init_noise_level`, or `inpaint_range` into #59's
prompt panel. Complete those in a follow-up **Variation / Inpaint** authoring
mode with its own plan:

1. choose a source from the generated sample/song library or a deck/loop capture;
2. transcode through a trusted shell/backend path to the API's required 44.1 kHz
   PCM16 mono/stereo shape and enforce its 16 MiB upload ceiling;
3. show source duration and a waveform/range editor rather than raw start/end
   number boxes;
4. send multipart through an extension of the shared SA3 client;
5. persist a library identity/provenance reference, not an absolute filesystem
   path, and define behavior when the source file is later removed.

Keep sampler steps at the pinned eight-step sweet spot, DiT/decoder selected by
the product engine, and dtype/free-models/output path as runtime internals.
Per-LoRA step gating belongs in the LoRA control if it is productized later; it
should not leak into an unrelated Avoid/CFG form.

This scope makes #59 the complete SA3 **text-prompt steering** experience while
leaving audio editing and operational CLI flags in interfaces that match their
actual concepts.

## Definition of done

- Basic remains the beginner-friendly path in the Generate tab; Samples
  and both deck generation panels remain unchanged and have no mode toggle.
- Advanced exposes Avoid chips/text, honest CFG/adherence, numeric APG, random
  or fixed reproducible seed, and the existing LoRA stack for the SA3 Track
  engine; Magenta hides the SA3-only mode and remains unchanged on the wire.
- No hidden Advanced value affects Basic; it omits all guidance fields and sends
  only its independently minted seed.
- Every newly saved generated song can carry a backward-compatible, versioned
  effective recipe, and song rows with a recipe can restore it without
  generating.
- Same-seed listening checks demonstrate negative suppression and low/high CFG
  behavior; UI/accessibility/responsive checks pass.
- `npx tsc -p tsconfig.app.json --noEmit` and `just check` pass, and the real-SA3
  checklist is completed on a model-ready Mac.
