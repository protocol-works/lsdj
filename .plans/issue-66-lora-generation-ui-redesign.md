---
issue: 66
url: https://github.com/protocol-works/lsdj/issues/66
pr: https://github.com/protocol-works/lsdj/pull/91
title: "Move LoRA management into generation and clarify steering state"
date: 2026-07-24
baseline: 9e7ad0c
status: "implemented; native listening verification pending"
---

# Plan: LoRA generation UI redesign (#66 / PR #91)

## Progress

- [x] Inspect PR #91, issue #66, the LoRA registry/management flow, all
  generation call sites, steering state, styling, and tests.
- [x] Agree on the product direction: progressive disclosure from the generation
  surface, with explicit off/on/bypassed states.
- [x] Finalise the compact trigger, expanded-panel wireframe, and UI copy.
- [x] Phase 1: centralise LoRA library state and lifecycle actions.
- [x] Phase 2: build the accessible anchored-panel primitive and compact control.
- [x] Phase 3: implement selection, steering, compatibility, and management in
  the contextual panel.
- [x] Phase 4: replace every rack, remove LoRA management from Settings, and
  preserve generation payload behaviour.
- [x] Phase 5: complete automated/accessibility verification and prepare the
  visual-density/manual UX checklist (native hardware checks remain manual).
- [x] Run `just check`.
- [ ] Complete the updated issue-66 hardware/audio UX checklist on a machine
  with the SA3 runtime and test adapters installed.

Automated verification completed 2026-07-24: `just check` passes (209 backend
tests, 593 frontend tests, 333 Rust tests passed with 1 timing diagnostic
ignored). The native listening, bit-exact generated-audio, and real install
flows remain deliberately unchecked in `docs/issue-66-checklist.md`.

## Problem

PR #91 delivers the complete LoRA lifecycle and generation contract, but the
current frontend splits one workflow across two places:

- adapter installation, listing, deletion, and progress live in a top-level
  Settings drawer section; and
- adapter selection and strength live in an always-visible `LoraRack` repeated
  across Generate, Samples, and both deck generation panels.

The rack renders every compatible installed adapter, so its footprint grows with
the library instead of with the current generation choice. Its visual state is
also ambiguous: an applied adapter differs mainly through accent-coloured text
and border, while strength `0` remains selected but is only dimmed. When no
compatible adapters exist, the rack renders nothing, leaving no generation-side
way to discover or install the feature.

## Chosen direction

Replace the always-visible rack with one compact `LoRA` status/disclosure control
in every generation surface. Opening it shows a contextual, anchored panel for
selection, strength, compatibility, installation, and management.

The collapsed generation UI must stay one stable control high regardless of the
number of installed adapters. Example summaries:

```text
LoRA  [ Off ▾ ]
LoRA  [ ● Maqam ×1 · +1 active ▾ ]
LoRA  [ 1 active · 1 bypassed ▾ ]
```

The expanded panel has three progressively disclosed sections:

1. **Applied to this generation** — selected adapters, explicit state, strength,
   bypass, and removal.
2. **Available adapters** — compatible adapters with an explicit Apply action,
   plus a collapsed explanation of incompatible adapters.
3. **Install and manage** — an `Install adapter…` disclosure, per-adapter
   management actions, and Open Folder.

Do not automatically apply a newly installed adapter. Keep the panel open,
highlight the new compatible row, and offer an explicit Apply action so an
installation never silently changes the next generation.

## UX state contract

The visible and accessible UI must use text/state attributes as well as colour.

| Effective state | Collapsed summary | Expanded row |
| --- | --- | --- |
| No adapters installed | `LoRA · None installed` | Installation entry point |
| Compatible adapters, none selected | `LoRA · Off` | Neutral available rows |
| Selected at strength greater than 0 | Adapter/value or `N active` | Lit state, explicit `On`, labelled strength |
| Selected at strength 0 | `N bypassed` where relevant | Explicit `Bypassed`; never described as active |
| Adapter incompatible with current kind/base | Excluded from active count | Listed under `Incompatible`, with required engine/base |
| Magenta generation | `LoRA · Unavailable for Magenta` | Management remains reachable; application is disabled |
| Four effective stack entries | Current active summary | Remaining Apply actions disabled with the cap explained |

Definitions:

- **Selected** means the adapter remains in the local stack state.
- **Active** means selected, compatible with the current generation, and strength
  is greater than `0`.
- **Bypassed** means selected and compatible, but strength is exactly `0`; the
  request may retain the zero-strength entry for the existing bit-exact bypass
  contract.
- **Off** means no compatible adapter will affect the next generation.

## Implementation plan

### Phase 1 — shared library state

- [x] Extract registry loading, `models://changed` subscription, install
  progress, errors, and lifecycle actions from `LoraLibrary`/`useLoras` into one
  shared `LoraProvider` or equivalent external store.
- [x] Mount the provider once above `MediaExplorer` and both `DeckColumn`
  instances so the app does not create independent `modelStatus()` reads and
  subscriptions for every surface.
- [x] Expose installed adapters, current install state, install/cancel/delete,
  and Open Folder through a small stable hook API.
- [x] Keep `useLoraStack()` local to each generation form. Track, Samples, deck
  A, and deck B must continue to have independent pending choices and remembered
  strengths.
- [x] Preserve stale-choice filtering when an adapter is deleted or the engine
  switches to a different base.

### Phase 2 — compact trigger and anchored panel

- [x] Build a reusable `LoraControl` that accepts generation kind/base, local
  stack state/actions, and deck/master accent.
- [x] Always render the control for generation surfaces, including empty-library
  and unsupported-engine states, so LoRA discovery and management remain
  reachable.
- [x] Make the trigger expose `aria-expanded`, a suitable popup relationship,
  explicit summary copy, and a lit treatment only when at least one adapter is
  effectively active.
- [x] Add an accessible anchored-panel primitive rendered through a portal.
  Media and deck containers scroll/clip, so an in-flow or locally absolute panel
  is not sufficient.
- [x] Support outside-click dismissal, Escape, focus entry, focus containment or
  documented non-modal behaviour, focus restoration, viewport-edge flipping,
  and a narrow-window width cap.

### Phase 3 — steering, compatibility, and management

- [x] Render only selected adapters in **Applied to this generation**.
- [x] Give every applied row an explicit `On` or `Bypassed` label and a visible
  `Strength ×N` label; do not rely on the knob readout or accent alone.
- [x] Keep the curated strength range and reset behaviour unless the final
  wireframe selects a clearer horizontal range control. In either case, retain
  quarter-step input and the `×1` reset.
- [x] Make removal from the stack a separate action from strength adjustment.
- [x] Show base-compatible adapters in **Available adapters** with Apply actions.
- [x] Show incompatible installed adapters in a collapsed group with concrete
  guidance such as `Medium DiT — select Track` instead of silently filtering
  them out.
- [x] Move the HF repo/link field and local `.safetensors` import into a collapsed
  `Install adapter…` section.
- [x] Keep base detection as the default and move the small/medium override under
  an Advanced disclosure for rank-only adapters.
- [x] Surface live install/cancel/error progress inside the open LoRA panel and
  preserve progress when the panel is closed and reopened.
- [x] Put Delete and Open Folder behind clear management actions. Confirm a
  delete when the adapter is selected in a visible generation stack, and ensure
  its stale choices disappear safely after deletion.

### Phase 4 — integrate all generation surfaces

- [x] Replace the track rack in Media Explorer → Generate with `LoraControl`.
- [x] Replace the small-DiT rack in Media Explorer → Samples.
- [x] Replace the rack in deck A's generated-pad panel.
- [x] Replace the rack in deck B's generated-pad panel.
- [x] Remove the top-level LoRA Library section and import from `App.tsx`.
- [x] Remove or repurpose `LoraLibrary` and `LoraRack` once all behaviour is
  covered by the shared provider/control/panel components.
- [x] Keep `/api/generate`'s `loras: [{ name, strength }]` shape, backend
  validation, Rust registry/importer, and SA3 argv wiring unchanged.
- [x] Verify that engine switches report the effective current state honestly
  while remembered per-adapter strengths remain available when switching back.

### Phase 5 — verification and polish

- [x] Unit-test the collapsed summary for none installed, off, one active,
  multiple active, bypassed, stack-full, incompatible, and Magenta states.
- [x] Unit-test Apply, Remove, strength, bypass, reset, remembered strength,
  install, cancel, error, delete, and live registry updates.
- [x] Test keyboard opening/dismissal, focus restoration, accessible names and
  state attributes, and ensure no meaningful state is colour-only.
- [x] Retain integration tests proving the exact LoRA stack and trims sent by
  Generate, Samples, and both deck call sites.
- [x] Test engine/base switches, deletion of a selected adapter, and the
  four-adapter cap.
- [ ] Test long adapter names, many installed adapters, small deck widths, media
  tray scrolling, and panel viewport-edge placement.
- [ ] Confirm the collapsed generation layout does not grow as adapters are
  installed and does not reduce the media list's usable height.
- [x] Update `docs/issue-66-checklist.md` to replace Settings/rack instructions
  with the contextual-panel flow and the explicit state terminology.
- [x] Run frontend unit tests while iterating, then run `just check` before
  completion.

## Likely affected frontend areas

- `frontend/src/App.tsx`
- `frontend/src/media/MediaExplorer.tsx`
- `frontend/src/deck/DeckColumn.tsx`
- `frontend/src/models/useLoras.ts`
- `frontend/src/models/LoraLibrary.tsx`
- `frontend/src/ui/LoraRack.tsx`
- `frontend/src/ui/Knob.tsx` if the visible strength labelling changes
- `frontend/src/ui/ui.css`
- `frontend/src/media/media.css` and/or `frontend/src/deck/deck.css` for trigger
  placement only
- `frontend/src/i18n/en.json`
- corresponding frontend tests
- `docs/issue-66-checklist.md`

Backend Python, Rust registry/import validation, SA3 pinning, and the HTTP
contract are deliberately outside this redesign unless implementation uncovers
a contract defect.

## Risks and mitigations

- **Overlay clipping or incorrect placement:** render through a portal, calculate
  placement from the trigger, and test inside both scrolling surfaces.
- **A panel duplicated four times creates duplicated lifecycle state:** keep the
  open panel local but registry/install state singleton through the provider.
- **Colour remains the only active signal:** require visible status copy and
  semantic state attributes in the component contract and tests.
- **Strength zero is confused with active:** calculate active and bypassed counts
  separately and display `Bypassed` explicitly.
- **Incompatible adapters appear to vanish:** show them in an explanatory group
  rather than silently filtering the management view.
- **Management overwhelms steering:** collapse installation and advanced base
  override by default; keep applied adapters first.
- **Delete unexpectedly changes a pending generation:** confirm selected-adapter
  deletion and rely on the existing stale-choice filtering as the final safety
  boundary.

## Definition of done

- LoRA management is reachable from every generation context and no longer
  occupies a top-level Settings section.
- The closed generation surface uses one stable-height control regardless of
  installed-adapter count.
- Users can distinguish Off, On, Bypassed, incompatible, unavailable, and
  stack-full states without interpreting colour alone.
- Users can install from HF, import a local safetensors adapter, observe/cancel
  progress, delete, and open the registry folder from the contextual panel.
- Track, Samples, and both decks retain independent stacks and send the same
  validated request contract as PR #91.
- Automated checks pass and the updated issue-66 hardware/UX checklist is
  completed on a machine with the SA3 runtime available.
