# 0022. Persist generated samples and loops in a samples library

- **Status:** Accepted
- **Date:** 2026-06-26
- **Deciders:** Daniel Peter

## Context

Full generated **tracks** persist: composing in the Media Explorer auto-saves a
WAV to `~/Documents/LSDJ/generated_songs/` with a JSON `registry.json`, and the
list restores on launch ([ADR-0013](0013-media-explorer-and-playback-mode.md),
`src-tauri/src/songs.rs`).

Short **samples / loops** had no home and died at session end:

- **Deck freeze captures** (M13, [ADR-0009](0009-freeze-pads-loop-played-audio-at-the-channel-head.md)) —
  the seam-folded PCM lives only inside the Rust engine's loop slot.
- **Deck generated pads** (M18, [ADR-0012](0012-generated-pads-from-the-sampler-bank.md)) —
  the backend WAV is held only briefly before it is loaded into a slot.
- The Media Explorer's short **SFX/Music** compositions filed into
  `generated_songs`, lumped in with full tracks.

The session-only design was deliberate for performance state, but losing a good
freeze or a crafted pad on quit is a real gap once the booth is used to *build* a
set rather than only perform one.

## Decision

Add a **generated-samples library** mirroring the songs library — a fixed folder
`~/Documents/LSDJ/generated_samples/` plus a reconcile-on-list `registry.json`,
written through the Rust shell (the webview is untrusted; the folder is never a
webview-supplied path). It auto-saves all three sources, and a new **Samples** tab
in the Media Explorer lists them and loads one back into a deck loop slot.

Specifics:

- **Shared boundary, separate libraries.** The security-critical filesystem
  helpers (`safe_stem`, `scoped_path`, unique-name, registry IO) move into a shared
  `src-tauri/src/library.rs` so the traversal defenses live once, not copy-pasted; a
  `SampleLibrary` parallels `SongLibrary` over them. A `SampleEntry` is a `SongEntry`
  plus `oneShot` — the loop-vs-one-shot verdict reload needs.
- **Source-specific save paths.**
  - Generated pads and composed SFX/Music persist the **raw backend WAV** (it carries
    the `LOOP_CROSSFADE_SECONDS` seam surplus), so the single fold the engine applies
    on reload reproduces the loop exactly.
  - A freeze's audio exists only in the engine slot, so a dedicated `save_loop_slot`
    command reads the **exact stored slot buffer** through a new engine read-back
    (`Engine::read_loop_slot` / `Host::read_loop_slot`, the `capture_sample`
    round-trip pattern), encodes a float32 WAV, and records it — any loop length, no
    drift.
- **Reload into a slot, as a LAYER.** A saved sample reloads through
  `load_generated_loop` (loop or one-shot per its `oneShot`) via a new
  `useDeck.loadSampleToSlot`; a new `onLoadSample` prop wires the Samples tab to the
  decks like `onLoadTrack`. Unlike a freeze (which REPLACES the live stream to hold a
  moment, ADR-0009), a loaded-sample loop **layers**: it is summed on top of the base
  (live, or the active freeze) and several stack at once — load a riff and play it
  *over* the deck. The engine grew a `layers` set alongside the single replacing
  `active`; `play_loop` takes a `layer` flag the shell sets from the slot's provenance,
  and `stop_layer` toggles one off. The rule: a **freeze capture replaces** (hold a
  moment, ADR-0009); **every other loop layers** — a loaded sample AND a deck-generated
  pad (M18) loop both sum and stack. One-shots overlay (sum once) as before.
- **Reclassification.** The Generate tab keeps the full-track engines (Track,
  Magenta); SFX/Music compose in the Samples tab now and file into the sample library.
- **Auto-save everything.** Every freeze, generated pad, and composed clip persists
  automatically, fire-and-forget — a save failure never disturbs the live audio.
- **Live-reload via a folder watcher.** A Rust `notify` watcher (`watcher.rs`) on both
  library folders emits a `library://changed` event when an audio file appears or
  disappears — a deck auto-saving out-of-band, or a hand-drop/-delete — and each tab
  re-lists. Rust owns the watch and emits (the webview gets no filesystem access);
  `registry.json` writes are ignored so reconcile-on-list can't loop; a short debounce
  coalesces a save burst. The re-list reuses rows by filename, so it never churns ids
  or drops in-memory bytes while a tab is open.

## Consequences

- A captured freeze, a generated pad, and a composed loop all survive a relaunch and
  reload into a slot — the session-only gap closed for samples.
- Loaded samples **layer and stack** over the deck rather than replacing it, so a deck
  becomes a live jam plus stacked riffs. Summing several loops + the live stream pushes
  levels up; the master limiter and per-deck trim absorb it, but it is hotter than a
  single replace — the performer rides the fader. Freezes are unchanged (replace).
- A freeze's saved buffer is *already* seam-folded, so reloading it through
  `load_generated_loop` folds it a **second** time: it loses ~30 ms and lightly
  re-blends the seam. One-time and inaudible on sustained material; flagged on the
  hardware checklist. Exact-fidelity reload (a non-folding "verbatim install" path +
  a `folded` registry flag) is a documented follow-up, not this cut.
- "Auto-save everything" fills `generated_samples` quickly with throwaway freezes;
  the Samples tab's ✕ trashes a file and prunes the registry, and the folder is one
  click away ("Open samples folder"). A future refinement could curate or expire.
- The two libraries now share `library.rs`; a change to the path/registry boundary is
  made once and covered by tests there.
- A new hardware checklist (`docs/m24-samples-hardware-checklist.md`) covers the
  freeze→save→reload round-trip and the double-fold seam, which unit tests can't hear.

## Alternatives considered

- **Re-capture the freeze tail via the existing `capture_sample`** instead of reading
  the slot buffer — rejected: it refuses below 3 s (so 1 s/2 s freezes couldn't save)
  and re-reads the moving played-history head, so the saved bytes drift from what is
  actually looping.
- **Exact-fidelity reload now** (read the folded slot buffer and install it verbatim,
  no re-fold) — deferred: it needs a new non-folding engine load path and a per-entry
  `folded` flag; the one-time ~30 ms loss is acceptable for v1.
- **One generic library over a single entry type** rather than `Song`/`Sample`
  siblings — rejected per the house "boring, explicit" style; only the security/path
  helpers are shared, the per-entry schema/reconcile stay explicit.
- **Per-save Tauri events** (emit on each `record`) instead of a folder watcher —
  rejected: a watcher also catches hand-drops/-deletes in Finder and needs no hook in
  every write path. Reconcile-on-list stays the cheap fallback (tab open / startup) if
  the watch can't install.

<!-- Status values: Proposed | Accepted | Rejected | Deprecated |
     Superseded by ADR-NNNN -->
