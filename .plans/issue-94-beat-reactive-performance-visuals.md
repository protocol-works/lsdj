---
issue: 94
url: https://github.com/protocol-works/lsdj/issues/94
title: "Add beat-reactive performance backdrop and deck-frame pulses"
date: 2026-07-30
baseline: 7309354
branch: performance-animations
status: "implementation complete; native QA pending"
---

# Plan: Beat-reactive performance backdrop and deck-frame pulses (#94)

## Progress

- [x] File and review issue #94.
- [x] Inspect the existing beat clocks, engine snapshot getters, crossfader law,
  realtime primed state, playback transport, appearance persistence, theme
  tokens, motion-preference precedent, tests, and project rules.
- [x] Settle the data, animation, CSS, persistence, accessibility, and test
  contracts below.
- [x] Mark this plan implementation-ready with no blocking product decisions.
- [x] Phase 1: add and test the pure visual math/state model.
- [x] Phase 2: implement the single-loop `PerformanceVisuals` runtime and its
  lifecycle tests.
- [x] Phase 3: add the backdrop and deck-frame presentation layer.
- [x] Phase 4: wire App state, the appearance toggle, persistence, and i18n.
- [x] Phase 5: complete integration, accessibility, and regression tests.
- [x] Phase 6: add the issue-94 native visual/performance checklist and run all
  automated verification.
- [x] Native visibility follow-up: correct the imperceptible below-panel
  presentation reported from the running app, then re-run focused and project
  verification.
- [x] Owner-directed visual redesign: replace the subtle overlay and deck frames
  with a smoked global underlay, gain/beat glows, and a breathing plus lattice.
- [x] Iterate with a demanding user/design subagent until it gives genuine visual
  approval; iteration 2 was approved as distinctive and club-ready.
- [x] Re-run focused, production, and full-project gates after the global-stage
  redesign.
- [x] Owner live-density retune: shrink and soften the on-beat plus lattice based
  on the populated native booth, then re-run automated verification.
- [x] Owner final visual choice: remove beat-driven plus growth entirely, retain
  one fixed-size gain-reactive grid, and re-run verification before publishing.
- [ ] Complete the native visual/performance checklist on the packaged macOS app.

Update this section as work lands. Do not mark the issue complete while native
visual QA remains unchecked; use the front-matter status to distinguish
`implementation complete; native QA pending` from `complete`.

## Verification log

Record exact commands and manual evidence here during implementation rather than
relying on the eventual PR summary.

| Date | Check | Result | Notes |
| --- | --- | --- | --- |
| 2026-07-30 | Planning inspection | complete | Baseline `7309354` on `performance-animations`; worktree clean before this plan file. |
| 2026-07-30 | `cd frontend && npm run test -- --run src/visuals/performanceMath.test.ts` | pass | 1 file, 12 tests. |
| 2026-07-30 | `cd frontend && npm run test -- --run src/visuals/performanceMath.test.ts src/visuals/PerformanceVisuals.test.tsx` | pass | 2 files, 21 tests; deterministic RAF and motion-preference lifecycle coverage. |
| 2026-07-30 | Targeted visual ESLint + explicit frontend TypeScript | pass | `npx eslint` over the four visual source/test files, then `npx tsc -p tsconfig.app.json --noEmit`; final project-wide results are recorded below. |
| 2026-07-30 | `cd frontend && npm run test -- --run src/visuals/performanceMath.test.ts src/visuals/PerformanceVisuals.test.tsx src/persistence.test.ts src/App.test.tsx src/App.performanceVisuals.test.tsx` | pass | 5 files, 47 tests. App-render tests emit jsdom's existing unimplemented-canvas diagnostic; no test failure. |
| 2026-07-30 | `cd frontend && npx tsc -p tsconfig.app.json --noEmit` | pass | Exit 0 after the complete implementation. |
| 2026-07-30 | `cd frontend && npm run build` | pass | Vite production build completed; only the existing >500 kB chunk-size advisory was reported. |
| 2026-07-30 | `just check` | pass | Ruff format/check, ESLint, frontend project typing, and Clippy passed; backend 209 passed with 4 known warnings, frontend 646 passed across 54 files with jsdom canvas/navigation diagnostics, Rust 336 passed with 1 timing diagnostic ignored. The first sandboxed attempt could not open the existing uv cache; the approved identical rerun passed. |
| 2026-07-30 | Requirement-by-requirement completion audit | implementation pass | Re-read the current worktree against every numbered decision, phase checkbox, scope boundary, and React high-frequency rule. Source/test/build evidence proves the implementation and fallback status; only packaged-app layering/theme, audible timing, and sustained performance observations remain intentionally pending. No backend, Rust, audio-engine, analysis, IPC, MIDI, or MCP file changed. |
| 2026-07-30 | User native screenshot + forced-value browser reproduction | visibility failure reproduced | Both playback decks were visibly running but no performance treatment was perceptible. With known `0.5` wash/pulse values, computed styles were valid (`0.10` glow, `0.35` frame), yet the negative-z wash appeared only in exposed gaps because every major booth panel is opaque. |
| 2026-07-30 | Forced-value presentation after stacking/frame correction | visual pass | The same fixture visibly tinted the matte panels while retaining the `0.20` hard cap and `pointer-events: none`; frame border remained at `0.35` contribution opacity with stronger token color. Computed styles confirmed stack level `0`, inert interaction, and the unchanged contribution caps. |
| 2026-07-30 | Visibility-fix focused suite, ESLint, explicit TypeScript, and production build | pass | The six focused files passed 49 tests, including the new static presentation contract; targeted ESLint and `npx tsc -p tsconfig.app.json --noEmit` exited 0; Vite built successfully with only the existing >500 kB advisory. |
| 2026-07-30 | `just check` after visibility correction | pass | Ruff format/check, ESLint, frontend project typing, and Clippy passed; backend 209 passed with 3 known warnings, frontend 648 passed across 55 files with existing jsdom canvas/navigation diagnostics, and Rust 336 passed with 1 timing diagnostic ignored. |
| 2026-07-30 | Deterministic browser review, global-stage iteration 1 | visual revision requested | At moderate centered energy, the global field and plus lattice read clearly, but the critic found the resting grid too prominent, the beat/rest contrast too weak, and the color fields too edge-bound. |
| 2026-07-30 | Deterministic browser review, global-stage iteration 2 | visual approval | Moderate centered energy was compared at beat peak and between beats. After lowering the resting lattice, strengthening and blooming deck-colored hits, widening the light fields, increasing panel transmission, and extending decay to 140 ms, the demanding user/design subagent said it was genuinely impressed and would ship the direction. |
| 2026-07-30 | Global-stage focused suite, targeted ESLint, explicit TypeScript, and production build | pass | Six focused files passed 50 tests; targeted ESLint and `npx tsc -p tsconfig.app.json --noEmit` exited 0; Vite built successfully with only the existing >500 kB advisory. |
| 2026-07-30 | `just check` after owner-approved redesign | pass | Ruff format/check, ESLint, frontend project typing, and Clippy passed; backend 209 passed with 3 known warnings, frontend 649 passed across 55 files with existing jsdom canvas/navigation diagnostics, and Rust 336 passed with 1 timing diagnostic ignored. The first sandboxed attempt could not access the existing uv cache; the approved identical rerun passed. |
| 2026-07-30 | Populated native booth screenshot after global-stage redesign | lattice tuning requested | The global stage and gain glow read correctly, but the on-beat plus arms and bloom became a dominant foreground pattern across the dense mixer/deck UI. Owner requested a subtler beat lattice while retaining the broader effect. |
| 2026-07-30 | Subtle-lattice focused suite, targeted ESLint, explicit TypeScript, and production build | pass | The three directly affected visual files passed 24 tests; targeted ESLint and `npx tsc -p tsconfig.app.json --noEmit` exited 0; Vite built successfully with only the existing >500 kB advisory. |
| 2026-07-30 | `just check` after subtle-lattice retune | pass | Ruff format/check, ESLint, frontend project typing, and Clippy passed; backend 209 passed with 4 warnings (including an intermittent test-thread broken-pipe warning), frontend 649 passed across 55 files with existing jsdom canvas/navigation diagnostics, and Rust 336 passed with 1 timing diagnostic ignored. |
| 2026-07-30 | Owner review after subtle-lattice retune | final presentation revision | Owner preferred the global stage without any growing/shrinking plus marks. The larger deck-colored hit lattices are removed; the fixed neutral plus texture continues to follow output gain while beat response remains in the broad deck glows. |
| 2026-07-30 | Fixed-grid focused suite and production build | pass | The three directly affected visual files passed 24 tests; Vite built successfully with only the existing >500 kB advisory. Presentation contracts now enforce fixed 1×5 px plus geometry and the absence of beat-hit grid layers. |
| 2026-07-30 | Final `just check` after removing plus growth | pass | Ruff format/check, ESLint, frontend project typing, and Clippy passed; backend 209 passed with 4 warnings (including an intermittent test-sidecar broken-pipe warning), frontend 649 passed across 55 files with existing jsdom canvas/navigation diagnostics, and Rust 336 passed with 1 timing diagnostic ignored. |
| 2026-07-30 | `docs/issue-94-performance-visuals-checklist.md` | pending native QA | Checklist created; packaged-app, audible timing, and sustained performance observations remain unchecked. |

## Problem and intended outcome

LSDJ already communicates detailed deck state through meters, close-up
waveforms, transport lights, and the phase meter, but the overall booth stays
visually static. Issue #94 adds a restrained layer of performance motion without
turning the control surface into a full-screen visualizer:

1. a dark, panel-colored global stage behind the booth, with one gain/beat glow
   per deck; and
2. a sparse, fixed-size plus-mark lattice whose visibility breathes with audible
   output gain while the broader deck glows carry trustworthy beat motion.

Both effects must project what reaches the speakers. A loud primed deck, a deck
behind a crossfader endpoint, a paused track, or an unconfident beat clock must
not create a false performance cue. At the center of a transition, the two decks
remain independent: aligned clocks pulse together, while offset clocks pulse
apart.

The target look is a smoked, retro-futurist instrument that charges into a
deck-colored circuitry field on each beat. It must not move controls, reduce
foreground contrast, resemble an error state, or become a full-screen strobe.

## Baseline facts

- `App` owns the projected crossfader value and both `useDeck` control objects.
  It is the narrowest integration point that can see both decks and the master
  engine without adding another state store.
- `DeckControls.getLiveBeat()` returns a speaker-clock `BeatClock` only when the
  shell's honesty gate has published a live anchor, the stream is playing, and
  worklet/native stats are fresh. It already rejects stats older than 2.5 s.
- `DeckControls.getTrackBeat()` returns a speaker-clock `BeatClock` only for a
  playing track with an offline beatgrid. It accounts for current varispeed.
- A `BeatClock` is `{ periodSeconds, beatAtContext }`. The phase meter already
  chooses `getTrackBeat()` in playback mode and `getLiveBeat()` in realtime
  mode. Performance visuals must use the same selection rule.
- `AudioEngine.getContextTime()` returns the cached native render clock in
  seconds. It is null before the first engine snapshot.
- `DeckControls.getChannelLevel()` reads the cached per-deck peak. Rust measures
  it post-trim/EQ/FX/channel fader but **before** the equal-power crossfader and
  the on-air gate. A primed or crossfaded-out deck can therefore meter while
  being inaudible; the visual layer must add those missing gates itself.
- `AudioEngine.getMasterLevel()` reads the cached post-mix master peak. Both
  level getters and the context getter are synchronous cache reads; using them
  does not add IPC.
- The Rust graph uses the equal-power law
  `A = cos(position × π/2)`, `B = sin(position × π/2)`.
- Realtime audibility is `state.playing && !primed`. Playback audibility is the
  loaded track's `playing` state. The crossfader gain may still reduce either
  audible source to zero.
- `createNativeEngine()` already owns one RAF-driven snapshot poll. This feature
  cannot subscribe to that internal loop, so it may add one visual RAF loop but
  must not add another snapshot request or per-deck loop.
- The deck colors are live CSS tokens: `--color-deck-a` and
  `--color-deck-b`. Existing CSS already relies on `color-mix()`, so the new
  presentation can remain theme-driven without reading resolved colors in JS.
- `AppSettings` persists webview-owned appearance values in `localStorage`.
  Beat View and Accent establish the pattern this setting should follow.
- The existing `Switch` primitive supplies a labelled `role="switch"` with
  `aria-checked`; no new toggle primitive is needed.
- The project is React 19, but its established high-frequency pattern is stable
  callbacks plus latest-value refs. The animation should follow that pattern and
  keep dynamic reads out of React state.
- The frontend uses jsdom and does not globally install RAF or `matchMedia`
  mocks. The component must be safe when either API is absent, and its focused
  tests must provide deterministic fakes.
- There is no formatter. New TS/TSX follows single quotes and no semicolons.
- No backend, Rust, MIDI, MCP, audio graph, or IPC contract change is required.
  No ADR is warranted for a frontend presentation feature over existing
  contracts.

## Decisions for this implementation

### 1. Treat “active deck” as continuous audible contribution

There is no binary active-deck flag. On every visual frame:

```ts
const gainA = Math.cos(clamp01(crossfade) * Math.PI / 2)
const gainB = Math.sin(clamp01(crossfade) * Math.PI / 2)

const rawA = audibleA ? smoothedLevelA * gainA : 0
const rawB = audibleB ? smoothedLevelB * gainB : 0
```

The current on-air boolean and crossfader gain are hard gates applied **after**
level smoothing. This means an off-air/faded-out deck becomes visually silent
at once instead of leaving a misleading release tail, while normal level drops
still decay smoothly.

Guard `rawA + rawB` with a small silence epsilon before normalizing:

```ts
const total = rawA + rawB
const balanceA = total > SILENCE_EPSILON ? rawA / total : 0
const balanceB = total > SILENCE_EPSILON ? rawB / total : 0
const energy = perceptual(smoothedMasterLevel)
const washA = balanceA * energy
const washB = balanceB * energy
```

The per-deck values decide color balance; the smoothed master peak decides total
energy. This preserves the decks' relative audible contribution without letting
two centered decks make the full-frame wash twice as bright as one deck. If the
master getter is unavailable/zero, both washes settle to zero rather than
claiming audible output from pre-crossfade meters.

Clamp non-finite/negative levels to zero and cap the visual domain at `1` before
perceptual compression. The shipped first pass uses `sqrt(clamp01(value))` as
the perceptual curve. Keep it as a named pure function so booth tuning can
change it without rewriting the loop.

### 2. Use the existing honest clock and continuous phase, not beat events

`App` provides one stable `getBeat()` callback per deck:

```ts
deck.mode === 'playback' ? deck.getTrackBeat() : deck.getLiveBeat()
```

The visual loop reads `engine.getContextTime()` once, then derives each pulse
from the clock's wrapped phase:

```ts
elapsed = contextTime - beatAtContext
secondsSinceBeat = positiveModulo(elapsed, periodSeconds)
pulse = Math.exp(-secondsSinceBeat / PULSE_DECAY_SECONDS)
```

This deliberately does not schedule timers or count beat events. It stays
speaker-aligned after a suspended tab, rate change, seek, or refreshed live
anchor because each frame derives the current phase from the authoritative
clock.

Return pulse `0` when context time is null/non-finite, the clock is null, its
period is non-finite/non-positive, the deck is not audible, or its contribution
is below the silence epsilon. Never make every fourth beat stronger: the current
anchor is not a trustworthy musical downbeat.

### 3. Separate signal math from presentation

Create a pure module, tentatively `frontend/src/visuals/performanceMath.ts`, for:

- finite clamping and positive modulo
- equal-power gains
- attack/release envelope smoothing
- audible contribution normalization
- perceptual level mapping
- seconds-since-beat and pulse-envelope calculation
- the complete one-frame visual-state reducer

The reducer contract should be explicit and mutation-free at its boundary:

```ts
type BeatClock = { periodSeconds: number; beatAtContext: number }

type VisualFrameInput = {
  deltaSeconds: number
  contextTime: number | null
  crossfade: number
  masterLevel: number
  decks: {
    a: { level: number; audible: boolean; beat: BeatClock | null }
    b: { level: number; audible: boolean; beat: BeatClock | null }
  }
}

type VisualFrameState = {
  smoothedMaster: number
  smoothedLevels: { a: number; b: number }
  wash: { a: number; b: number }
  pulse: { a: number; b: number }
}
```

The runtime retains the previous `VisualFrameState` in a ref/local closure and
passes it into the reducer. Tests exercise the reducer without React, DOM,
audio, or fake IPC.

### 4. Use time-based smoothing with bounded resume behavior

Use exponential attack/release smoothing so behavior is independent of refresh
rate:

```ts
alpha = 1 - Math.exp(-deltaSeconds / timeConstantSeconds)
next = previous + (target - previous) * alpha
```

Initial constants, kept together and exported only if tests require them:

- deck/master attack: `0.045 s`
- deck/master release: `0.30 s`
- beat pulse decay: `0.14 s` (roughly 95% gone after 420 ms)
- silence epsilon: `0.002`
- maximum accepted RAF delta: `0.10 s`

The constants are starting values, not product preferences exposed in settings.
Tune them during native QA, then record the final values in this plan's
implementation retrospective.

On the first frame use delta `0`; on later frames clamp the RAF timestamp delta
to `[0, 0.10]`. RAF pauses in a backgrounded/minimized webview must not create a
large catch-up interpolation on resume. Beat phase still comes from the current
audio context, so it resumes at the correct point in the lattice.

### 5. Add one stable visual RAF loop and zero React frame state

Create `frontend/src/visuals/PerformanceVisuals.tsx`. Its public shape is:

```ts
type PerformanceDeckSource = {
  audible: boolean
  getLevel: () => number
  getBeat: () => BeatClock | null
}

type PerformanceVisualsProps = {
  enabled: boolean
  rootRef: RefObject<HTMLElement | null>
  crossfade: number
  getContextTime: () => number | null
  getMasterLevel: () => number
  decks: Record<DeckId, PerformanceDeckSource>
}
```

Exact implementation rules:

- Copy changing props/source callbacks into one latest-value ref during render.
- Start the animation effect only when enabled, motion is not reduced, a root
  element exists, and RAF is available.
- The RAF callback reads the latest ref; crossfader moves, deck mode changes,
  transport changes, and App object churn must not tear down/restart the loop.
- Read each getter at most once per frame.
- Keep the previous numeric visual state and prior RAF timestamp outside React
  state. A frame writes CSS custom properties only.
- Batch the four CSS-property writes together and perform no layout/style reads
  in the loop. This follows the React performance guidance: high-frequency reads
  happen at their usage point, changing handlers live in refs, effect
  dependencies stay narrow, and DOM writes are not interleaved with layout
  queries.
- Schedule exactly one next RAF from the current callback. Keep its id and call
  `cancelAnimationFrame` during cleanup.
- If a getter throws or returns invalid data, sanitize that input for the frame;
  do not kill the loop or surface an unhandled error.
- If RAF is unavailable (notably plain jsdom), render an inert layer and do not
  throw.
- On disable or unmount, cancel the loop and remove all inline visual custom
  properties from the root so CSS falls back to zero.

The component renders only the presentation layer:

```tsx
<div className="performance-visuals" aria-hidden="true" hidden={!enabled}>
  <span className="performance-visuals__glow performance-visuals__glow--a" />
  <span className="performance-visuals__glow performance-visuals__glow--b" />
</div>
```

It exposes no role, text, focus target, event handler, or pointer hit area.

### 6. Drive a small, named CSS variable contract

Define zero fallbacks on `.app` and write only these inline variables:

```css
--performance-wash-a: 0;
--performance-wash-b: 0;
--performance-pulse-a: 0;
--performance-pulse-b: 0;
```

Each value is a finite decimal in `[0, 1]`; format to four decimal places to
avoid meaningless string churn. Before setting a property, compare with the
last emitted string and skip an identical write.

Do not send resolved colors through JS. CSS consumes `--color-deck-a` and
`--color-deck-b`, so changing `data-accent` recolors the next composite without
restarting the runtime or adding a `MutationObserver`.

### 7. Use composited DOM glows, not canvas, SVG, filters, or blend modes

The backdrop is an absolute, overflow-hidden layer covering `.app`:

- `.app` becomes an isolated stacking context.
- `.performance-visuals` uses `position: absolute; inset: 0; z-index: -1;
  pointer-events: none; overflow: hidden`.
- `.app` keeps a transparent background; `body` continues to provide
  `var(--color-bg)`. Inside the isolated stacking context, the negative-z layer
  paints above the body background and behind all booth content.
- Each glow is an oversized left/right radial gradient using its deck token.
- The glow's static gradient/shadow geometry does not change per frame. Animate
  only `opacity` and a small `transform: scale(...)`, with transform origins on
  the matching deck side.
- Do not use `filter: blur()`, `mix-blend-mode`, per-frame gradient strings, or
  canvas. Those add overdraw/tuning variability without helping this first
  restrained effect.

Initial CSS caps:

- resting wash contribution: at most `0.08` opacity
- beat bloom addition: at most `0.12` opacity
- combined glow opacity: hard cap `0.20`
- beat scale: at most `1.06`

Use CSS `clamp()` with the numeric variables so a JS defect cannot push the
layer beyond the visual cap. These are native-QA tuning values; any change must
retain the `0.20` full-glow ceiling unless the issue scope is revisited.

### 8. Implement frame thumps with inert deck pseudo-elements

Add `position: relative` to `.deck` and one `::after` overlay:

- absolute at the existing frame edge; no box-model participation
- `pointer-events: none`
- transparent at pulse zero
- color selected by `.deck--a` / `.deck--b`
- a static 1 px inset outline plus a small static outer glow
- per-frame animation through opacity only; no changing border width or shadow
  geometry
- maximum opacity around `0.70`, localized to the deck perimeter

`.deck--a::after` consumes `--performance-pulse-a`; `.deck--b::after` consumes
`--performance-pulse-b`. The existing 2 px top accent remains unchanged beneath
the overlay. Do not add a CSS transition: the JS beat envelope already defines
timing, and a second easing layer would lag the speaker clock.

### 9. Reduced motion keeps only a static, interaction-derived wash

The final reduced-motion behavior is decided here:

- detect `window.matchMedia('(prefers-reduced-motion: reduce)')` with one listener
  owned by the single `PerformanceVisuals` instance
- respond if the preference changes while the app is open
- run **no visual RAF** while reduced motion is active
- write both pulse variables as zero
- compute a low-opacity static A/B balance from the current crossfader gains and
  audible booleans only; do not sample live/master levels
- update that static balance only when React supplies a new crossfader or
  audibility prop, so the display can follow a direct user control/transport
  action without continuous autonomous motion
- cap the reduced-motion wash at half the normal resting maximum (`0.04`)

If `matchMedia` is absent, treat the preference as not reduced. Toggle-off is
stronger than reduced motion: it hides the layer, emits zero properties, and
does not install RAF or motion listeners beyond what is required to observe the
current component lifecycle.

### 10. Persist one boolean appearance setting, default on

Add `performanceVisuals: boolean` to `AppSettings`.

- `loadAppSettings()` accepts only a real boolean and drops malformed values.
- `App` initializes with `loadAppSettings().performanceVisuals ?? true`.
- A stable handler updates React state and calls
  `updateAppSettings({ performanceVisuals: next })`.
- Settings → Appearance renders the existing `Switch` after the Beat View and
  Accent controls, labelled by `t('settings.performanceVisuals')`.
- Add the English string `Performance visuals`; no hint text or intensity
  control is part of this issue.
- Keep the setting webview-local with the other appearance options. Do not move
  it into the Rust interface store/settings file.

### 11. App owns source selection; `useDeck` remains unchanged

In `App.tsx`:

- add an `.app` root ref
- create stable `getBeatA` / `getBeatB` callbacks using the deck-mode rule above
- derive primitive `audibleA` / `audibleB` booleans once per render
- pass existing stable `getChannelLevel`, engine clock/master getters,
  crossfade, and those primitive values to `PerformanceVisuals`
- mount the visual layer as the first non-titlebar child of `.app` so its DOM
  order is obvious while z-index remains authoritative

Do not add `getBeat()` or visual state to `DeckControls`, the Rust interface
store, or `AudioEngine`. The mode-selection callback is App-only composition of
existing public deck methods.

## Implementation plan

### Phase 1 — pure visual model

- [x] Add `frontend/src/visuals/performanceMath.ts` with named constants, input
  and state types, equal-power gains, sanitation, positive modulo,
  attack/release smoothing, contribution normalization, pulse envelope, and
  one-frame reduction.
- [x] Keep all functions deterministic and independent of DOM/React time
  sources; callers provide both RAF delta and audio context time.
- [x] Add `frontend/src/visuals/performanceMath.test.ts` before runtime wiring.
- [x] Cover endpoint/center gains, invalid crossfade, silence normalization,
  hard audibility gates, master energy scaling, attack vs release, delta clamp,
  context before/after anchor, clock/period refusal, pulse decay, and output
  bounds.
- [x] Run the focused math test and record it in the verification log.

### Phase 2 — visual runtime and lifecycle

- [x] Add `frontend/src/visuals/PerformanceVisuals.tsx` with the prop contract
  above and a tiny internal reduced-motion observer.
- [x] Use a latest-value ref for changing sources and a single effect for RAF
  ownership; keep frame state outside React state.
- [x] Write the four root variables without layout reads and skip identical
  strings.
- [x] Implement cleanup for disable, reduced-motion transition, unmount, and
  missing RAF.
- [x] Add `frontend/src/visuals/PerformanceVisuals.test.tsx` with deterministic
  RAF/cancel queues and a controllable `matchMedia` fake.
- [x] Test one loop for two decks, current-prop reads without loop restart,
  synchronized and offset clocks, null clocks, hard audibility/crossfader
  gates, CSS-variable bounds, identical-write suppression, live
  reduced-motion changes, toggle off/on, missing browser APIs, and unmount.
- [x] Verify flushing visual RAF callbacks changes style variables without
  changing the static DOM tree or requiring React state updates.

### Phase 3 — backdrop and frame presentation

- [x] Add the `.app` stacking/variable defaults and `.performance-visuals`
  layer in `frontend/src/index.css`.
- [x] Implement two token-driven radial glows with the stated opacity/scale
  caps and no filter/blend/canvas dependency.
- [x] Add the inert deck `::after` frame overlay in
  `frontend/src/deck/deck.css` and map each deck to its pulse variable.
- [x] Add explicit reduced-motion CSS as a defense in depth: force glow
  transforms to `none` and deck-frame overlay opacity to `0`, even though the JS
  contract already emits zero pulses.
- [ ] Confirm the layer does not cover the titlebar drag region, drawers, media
  tray, or controls and does not create horizontal/vertical scrollbars.
- [ ] Confirm all three accent themes recolor through CSS variables without a
  JS observer or reload.

### Phase 4 — App, setting, persistence, and copy

- [x] Extend `AppSettings` and `loadAppSettings()` with the strict boolean.
- [x] Add persistence tests for true/false round-trip, merge behavior, malformed
  rejection, and absence/default semantics.
- [x] Add App state initialized to on-by-default and the persisted toggle
  handler.
- [x] Add the root ref, mode-selecting beat callbacks, audibility booleans, and
  `PerformanceVisuals` wiring.
- [x] Place the `Switch` in Settings → Appearance and add the i18n string.
- [x] Extend App tests to prove default-on, switch semantics, persistence across
  remount, and disabled visual runtime behavior.
- [x] Keep dynamic effect dependencies primitive/stable; do not depend on the
  complete `deckA` / `deckB` objects in the runtime effect.

### Phase 5 — integration and regression tests

- [x] Exercise App with a fake engine/deck surface so a primed realtime deck
  cannot pulse despite a nonzero level.
- [x] Exercise playback selection so a playing grid clock pulses and a paused or
  gridless track does not.
- [x] Prove crossfader endpoints hard-zero the opposite deck and center uses the
  equal-power balance.
- [x] Prove missing context/snapshot data is inert and does not throw during
  application startup.
- [x] Verify the overlay is `aria-hidden`, pointer-inert, and contributes no
  focusable element or accessible name.
- [x] Retain existing Beat View, Accent, mixer, transport, and persistence test
  behavior.
- [x] Run focused tests, ESLint, and the real frontend TypeScript command before
  the full project gate.

### Phase 6 — native QA and completion evidence

- [x] Add `docs/issue-94-performance-visuals-checklist.md`.
- [x] Cover realtime and playback sources, primed/off-air state, pause/stop,
  crossfader endpoints/center/sweeps, aligned and intentionally offset clocks,
  missing confidence, silence, hot levels, all Accent and Beat View layouts,
  Settings persistence, live reduced-motion changes, resizing, minimizing and
  restoring the app, and media/drawer interaction.
- [x] Include audible click-track checks that compare the visual transient to
  the speakers, not merely the waveform/playhead.
- [ ] Inspect the packaged app with Web Inspector/Activity Monitor long enough
  to confirm one new RAF callback, no new per-frame IPC, no layout-thrash loop,
  and no obvious sustained CPU/GPU regression when enabled vs disabled.
- [ ] Tune only the centralized time/opacity/scale constants, record final values
  and observations in this plan, and keep all safety caps.
- [x] Run `just check` and the explicit frontend TypeScript command; record exact
  results in the verification log.
- [x] Update Progress, front-matter status, affected-file reality, deviations,
  and the implementation retrospective before calling the work done.

## Tests and acceptance mapping

| Issue acceptance criterion | Implementation evidence | Automated evidence | Native/manual evidence |
| --- | --- | --- | --- |
| Dual themed wash follows audible decks | contribution reducer + two CSS glows | math bounds and component CSS-variable tests | all themes; speaker-level crossfades |
| Each wash blooms on its own beat | continuous phase envelope per deck | aligned/offset fake-clock tests | click tracks on A/B |
| Fixed plus lattice follows output energy | one neutral field consumes combined wash | DOM/CSS presentation contract | subtle gain breathing without size changes |
| Equal-power and primed/off-air weighting | exact engine law + hard gates | endpoint, center, and primed tests | prime/drop-on-air and fader sweeps |
| Center aligns or separates by phase | independent deck clocks | same-clock/offset-clock tests | two-deck sync and deliberate nudge |
| No guessed beat | null/invalid clock returns zero pulse | missing/stale/gridless tests | low-confidence live material |
| Persisted on/off setting defaults on | `AppSettings` boolean + `Switch` | persistence and App remount tests | restart packaged app |
| Reduced motion has no pulses | no RAF + zero pulse + CSS defense | live `matchMedia` change tests | macOS Reduce Motion toggle |
| Inert and accessible | aria-hidden/pointer-none/nonfocusable layer | accessibility queries | control clicking/dragging/drawer use |
| No per-frame React state/IPC; one RAF | ref-owned runtime over cached getters | deterministic RAF ownership/cleanup tests | Web Inspector timeline |
| Theme switch recolors live | CSS token references only | DOM/CSS contract check | switch all accent themes |
| Tests/checklist/project gate | focused suites + `just check` | command log | completed issue-94 checklist |

## Likely affected areas

- `.plans/issue-94-beat-reactive-performance-visuals.md` (this living plan)
- `frontend/src/visuals/performanceMath.ts` (new)
- `frontend/src/visuals/performanceMath.test.ts` (new)
- `frontend/src/visuals/PerformanceVisuals.tsx` (new)
- `frontend/src/visuals/PerformanceVisuals.test.tsx` (new)
- `frontend/src/visuals/performancePresentation.test.ts` (new; CSS visibility
  and safety-cap contract)
- `frontend/src/App.tsx`
- `frontend/src/App.test.tsx`
- `frontend/src/App.performanceVisuals.test.tsx` (new; focused App/deck seam)
- `frontend/src/persistence.ts`
- `frontend/src/persistence.test.ts`
- `frontend/src/i18n/en.json`
- `frontend/src/index.css`
- `docs/issue-94-performance-visuals-checklist.md` (new)

The final list must be corrected during implementation if tests are placed in a
different existing harness. Deliberately untouched: `src-tauri/`, `backend/`,
audio/beat analysis, `nativeEngine` contracts, `useDeck` public API, MIDI, MCP,
waveform rendering, loop behavior, and recording.

## Risks and mitigations

- **A primed deck looks active because its meter still moves.** Apply the
  realtime on-air boolean after smoothing and before normalization; cover it in
  pure and integration tests.
- **Crossfader visuals disagree with the engine.** Copy the documented
  equal-power trigonometric law into one pure tested helper; do not approximate
  it with linear weights.
- **Peak telemetry flickers.** Use refresh-rate-independent attack/release
  envelopes for both channel and master values, then hard-cap outputs.
- **The visual drifts from audible beats.** Derive phase each frame from the
  engine context and existing `BeatClock`; never run an independent timer.
- **A stale closure or changing deck object restarts the loop.** Store changing
  props/callbacks in one latest ref and keep the RAF effect dependencies narrow.
- **React rerenders at display rate.** Keep all frame state in the loop closure
  and write four CSS properties; prohibit per-frame `setState` in review.
- **The new loop accidentally doubles IPC.** Consume only synchronous cached
  getters. Any change to `nativeEngine` for this feature is a scope alarm.
- **Layout thrash or overdraw hurts a live audio app.** Perform no layout/style
  reads per frame, animate opacity/transform on two fixed glows, avoid blur and
  blend modes, and inspect native performance.
- **Negative z-index hides the backdrop or leaks behind the app in WebKit.**
  Isolate `.app`, keep its own background transparent over the body's matte
  background, and verify every layout in native WKWebView before tuning color.
- **The overlay blocks controls/window dragging.** Make the full tree
  pointer-inert and nonsemantic; explicitly test header dragging, drawers, media,
  and both decks.
- **The deck pseudo-element changes layout or covers content.** Position it
  absolutely, animate only opacity, and leave the existing physical border
  unchanged.
- **Theme changes require expensive JS color reads.** Reference deck color
  tokens directly in CSS; no computed-style reads or theme observer.
- **Reduced-motion users still receive autonomous flashes.** Stop RAF, emit zero
  pulse, and add a CSS media-query override as defense in depth.
- **A minimized tab resumes with a giant flash.** Clamp smoothing delta and
  recompute beat phase from current audio time.
- **App tests fail because browser APIs are missing.** Guard RAF/matchMedia in
  production code and use local deterministic fakes in the focused suite.
- **Visual tuning becomes an unreviewable scattering of magic numbers.** Keep
  time constants in the math module and opacity/scale caps together in the
  visuals CSS section; record any native-QA changes in this plan.

## Scope boundaries

In scope:

- global smoked performance stage and dual-deck ambient glow
- fixed-size gain-reactive plus lattice
- audible-contribution and beat-phase math
- one persisted on/off control
- reduced-motion behavior
- automated tests and a native visual/performance checklist

Out of scope:

- new onset, downbeat, bar, key, or spectral analysis
- low/mid/high particles, sparks, scan lines, or ribbons
- a new phase visualization
- loop-orbit/pad-trigger animations
- limiter/error visualization
- intensity, decay, color, or palette controls
- fullscreen/projector output
- backend/engine/IPC changes
- consolidating the native snapshot RAF with the frontend visual RAF

If implementation appears to need any out-of-scope item, stop and update this
plan with the reason before changing code.

## Open questions and deviation log

There are no blocking owner questions at plan time. The issue already decides
the effects, default-on toggle, honest-clock behavior, reduced-motion support,
and no-backend constraint. This plan resolves the remaining implementation
choices as follows:

- active deck is continuous audible contribution, not a winner-take-all side
- master peak sets overall energy; deck peaks set color balance
- reduced motion retains a static direct-control wash and no autonomous pulse
- CSS/DOM glows are used instead of canvas
- visual constants are centrally tunable within hard opacity limits

During implementation, append deviations here instead of silently rewriting the
original plan. Each entry must state the discovery, decision, affected phases,
and whether issue acceptance changed.

- **Latest-source ref timing (Phase 2, acceptance unchanged):** the planning
  draft said to assign the latest-source ref during render. The implementation
  updates it in a passive effect declared before RAF ownership, following the
  repository's React 19 hook/lint guidance and avoiding a render-time ref write.
  The stable RAF never restarts, and the next visual frame reads the committed
  values; deterministic rerender coverage proves the behavior.
- **Opaque-panel visibility failure (Phase 3, acceptance unchanged):** native
  feedback showed that the specified negative-z backdrop was mathematically
  active but hidden beneath the header, decks, mixer, and media tray, leaving
  too little exposed background to perceive. The intermediate correction
  painted the pointer-inert wash at stack level `0` over the matte panels while
  preserving the original caps; the later owner-directed redesign below
  superseded that presentation. No runtime, accessibility, signal, or scope
  contract changed in this intermediate correction.
- **Owner-directed global-stage redesign (Phase 3, acceptance revised):** after
  the visibility fix, owner feedback was that the effects remained too subtle
  and did not improve the experience. The deck-frame treatment was removed and
  the accepted presentation became a true panel-colored underlay, smoked
  top-level modules, broader gain/beat glows, and a sparse plus lattice with
  deck-local beat blooms. The pulse decay changed from 75 ms to 140 ms so the
  hit reads as musical rather than flickery. Signal honesty, one-RAF ownership,
  persistence, reduced motion, accessibility, and frontend-only scope did not
  change; the issue's visual acceptance intentionally did.
- **Populated-booth lattice retune (Phase 3, acceptance refined):** the approved
  sparse preview understated how dominant 1.5×9 px hit arms and their 4×13 px
  bloom would look behind a fully populated native mixer. Owner feedback kept
  the global stage but requested subtler beat marks. Hit arms are now 1.25×7 px,
  bloom radii are 2.5×9.5 px, and hit intensity is `pulse * 1.25` capped at
  `0.62`. Gain glows, resting lattice, timing, data honesty, reduced motion, and
  runtime architecture are unchanged.
- **Owner removal of lattice growth (Phase 3, acceptance finalized):** after the
  smaller hit geometry was verified, the owner preferred the experience with no
  growing/shrinking plus marks at all. Both deck hit elements and their CSS were
  removed. One 1×5 px neutral lattice remains fixed in size and follows combined
  output energy; deck glows remain independently gain- and beat-reactive. This
  reduces DOM/compositing work and changes no signal, lifecycle, accessibility,
  persistence, or reduced-motion contract.

### Owner-approved global-stage presentation (supersedes sections 7–8)

- `.performance-visuals` is again the real negative-z underlay inside `.app`'s
  isolated stacking context. Its base is `var(--color-surface)` with a dark
  radial vignette, so exposed gutters are panel-colored rather than dead black.
- While `data-performance-visuals="on"`, only the status bar, deck shells,
  mixer shell, and direct media panel use a 76% `var(--color-surface)` mix.
  Inset controls remain opaque. Toggle-off restores the original surfaces.
- Each deck glow spans 106% of the stage and reaches through the center. Gain
  contributes `0.46`, beat contributes `0.68`, opacity is capped at `0.82`, and
  beat scale is `0.95 + pulse * 0.16`.
- The neutral 3 rem lattice uses 1×5 px plus arms and rests at
  `0.04 + energy * 0.14`, capped at `0.18`. Its geometry never changes.
- Reduced motion retains only the static contribution-colored underlay and
  neutral lattice. It suppresses glow scale and all autonomous sampling.
- The implementation continues to write only the existing four bounded CSS
  properties from one RAF; it adds no layout reads, per-frame React state,
  canvas, SVG, filter, blend mode, backend call, or IPC.

## Definition of done

- The deck glows match each existing speaker-clock beat and continuously follow
  audible contribution through the equal-power mix; the fixed-size lattice
  follows total visual energy without beat-driven geometry changes.
- Primed, stopped, paused, silent, crossfaded-out, unconfident, and pre-snapshot
  states are visually honest and safe.
- The implementation owns one stable visual RAF, no per-frame React state, no
  layout reads, and no new IPC/audio-analysis path.
- The default-on appearance toggle persists and fully stops the feature when
  off.
- Reduced motion produces no autonomous beat pulse or glow scale.
- Accent changes recolor the effects from existing CSS tokens, and every control
  remains readable, focusable, clickable, and draggable as before.
- Pure math, runtime lifecycle, persistence, App integration, and accessibility
  tests pass.
- The explicit TypeScript command and `just check` pass with results recorded.
- A human completes the packaged-app issue-94 checklist, final tuning values and
  deviations are recorded here, and front matter/progress reflect the actual
  completion state.

## Implementation retrospective

Fill this section after implementation; do not delete the original decisions.

### Outcome

- Implementation complete; native QA pending. The frontend now derives bounded
  deck wash and beat-pulse values from existing cached speaker-clock sources,
  emits them through one stable RAF, and exposes a default-on persisted
  Appearance switch. The approved presentation is a panel-colored global stage
  beneath smoked modules, broad deck glows, and a fixed-size plus lattice that
  breathes subtly with output gain. No backend, Rust, analysis,
  audio-engine, IPC, MIDI, or MCP contract changed.

### Final tuning constants

- Implemented values awaiting native validation: 45 ms level attack, 300 ms
  release, 140 ms beat-pulse decay, 100 ms maximum resumed-frame delta, 0.002
  silence epsilon, 0.46 gain glow, 0.68 beat glow, 0.82 glow cap, 0.95 base glow
  scale plus 0.16 beat scale, 76% panel surface mix, 0.04 + 0.14 energy neutral
  grid with a 0.18 cap and fixed 1×5 px arms. Reduced motion retains the existing
  half-strength static contribution wash and no transient.

### Deviations from the plan

- Latest-source ref assignment occurs in a passive effect before RAF ownership
  instead of mutating the ref during render; see the deviation log above.
  Acceptance and loop stability are unchanged and covered by rerender tests.
- The originally planned negative-z backdrop proved imperceptible beneath the
  booth's opaque panels. The intermediate stack-level-0 correction is recorded
  in the deviation log above and was subsequently superseded by the approved
  smoked-underlay design.
- Owner feedback superseded that intermediate overlay and the deck frames with
  the global-stage specification above. The final design returned the layer
  behind deliberately translucent top-level modules and added the breathing
  plus lattice; the visual acceptance changed, while data/runtime/accessibility
  contracts did not.
- The populated native booth showed that the preview-approved beat marks were
  too large in real density. The final hit geometry and cap were reduced as
  recorded above without changing the stage, glow, timing, or runtime model.
- The owner then chose the still cleaner presentation with no beat-driven mark
  growth. The hit layers were removed entirely; plus geometry is now constant.

### Verification evidence

- Demanding user/design subagent: iteration 1 rejected with concrete visual
  feedback; iteration 2 approved as genuinely impressive and shippable after a
  deterministic moderate-energy beat/rest comparison.
- Focused issue-94 suite after the owner-approved redesign: 6 files, 50 tests
  passed; the final fixed-grid presentation subset passed 24 tests.
- Explicit frontend TypeScript: passed.
- Frontend production build: passed (existing chunk-size advisory only).
- Full `just check`: passed (209 backend, 649 frontend, and 336 Rust tests; one
  Rust timing diagnostic intentionally ignored).
- Native checklist: created and intentionally left unchecked for a human run in
  the packaged app with audible references and performance tooling.
- Final completion audit: current source and tests satisfy every implementation
  requirement and stay within the documented frontend-only scope; the plan's
  explicit `implementation complete; native QA pending` fallback is accurate.
