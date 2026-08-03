# M24 samples library hardware checklist — persisting generated samples & loops

Manual verification of the generated-samples library (ADR-0022): deck freezes,
generated pads, and the Media Explorer's short SFX/Music compositions persist to
`~/Documents/LSDJ/generated_samples` and reload into a deck loop slot. The folder
+ registry plumbing (`library.rs`, `samples.rs`), the engine slot read-back
(`read_loop_slot`), and the UI/deck wiring are unit-tested; what tests can't hear is
the freeze round-trip and the seam — this checklist is the last hop: real audio,
real ears.

## Setup

- [ ] `just tauri-dev`, app open, a live (Realtime) deck playing.
- [ ] Two Finder windows handy: `~/Documents/LSDJ/generated_songs` (unchanged) and
      `~/Documents/LSDJ/generated_samples` (new).

## Auto-save: freezes

- [ ] **SAMPLER** pad (or the on-screen loop pad) freezes the last bars on a deck.
- [ ] A `.wav` appears in `generated_samples` and a row in `registry.json` with
      `model: "freeze"`, `oneShot: false`, no prompt.
- [ ] Freeze a **1 s** and a **2 s** loop (the short lengths): both save (the old
      `capture_sample` floor would have refused these — `save_loop_slot` reads the
      slot directly, so any length saves).

## Auto-save: generated pads & composed clips

- [ ] Generate a pad on a deck (SFX / Music / Magenta): a `.wav` appears with the
      prompt as title, `model` = the engine, and the `oneShot` you chose.
- [ ] In the Media Explorer **Samples** tab, compose an SFX and a Music clip (try
      both **Loop** and **One-shot**): each saves with the right `oneShot`.
- [ ] The Generate tab now offers only **Track** and **Magenta**; SFX/Music live on
      the Samples tab.

## Reload into a slot — layering (ADR-0022)

- [ ] In the Samples tab, **→ A** / **→ B** loads a saved sample into the first free
      loop slot on that deck. Press that SAMPLER pad: the sample plays **over** the
      live stream (you still hear the deck underneath) — it layers, it does not
      replace. A one-shot still fires once.
- [ ] **Stacking:** load two or three samples into slots and play them together —
      they sum on top of the live stream simultaneously (watch levels; the master
      limiter will catch hot sums, ride the channel fader).
- [ ] **Generated pads layer too:** generate a pad LOOP on a deck (not one-shot) and
      play it — it layers over the live stream and stacks, same as a loaded sample. A
      generated one-shot still overlays once.
- [ ] **Independent toggle:** press a layered pad again — only that layer stops; the
      others keep playing.
- [ ] **Freeze still replaces:** freeze a loop on the same deck while samples are
      layering — the freeze REPLACES the live stream (hold-and-re-steer, ADR-0009)
      and the layers keep summing on top of the freeze. Pressing the freeze pad again
      returns to live with the layers still going.
- [ ] **Deck STOP** silences everything (freeze + all layers + one-shots); the slots
      keep their buffers for a re-press.
- [ ] **Freeze round-trip / the seam:** freeze a loop, then reload that same freeze
      from the Samples tab into a slot and play it. It loops cleanly. Listen at the
      wrap point: a freeze reloads through one extra seam fold (~30 ms shorter, a
      slightly re-blended seam) — confirm it's inaudible on your material (the
      exact-fidelity verbatim-install path is the documented follow-up).
- [ ] Reload onto a deck currently in **playback** mode (a track loaded): it is
      refused honestly (an error under the list / no phantom load), not silent.
- [ ] All four slots full, then load a sample: a clear "every loop slot is full"
      message, nothing loaded.

## List, delete, restore

- [ ] **✕** on a row moves its file to the Trash and removes the row; `registry.json`
      no longer lists it.
- [ ] **Open samples folder** reveals `generated_samples` in Finder.
- [ ] Quit and relaunch: the Samples tab lists everything still on disk; loading a
      restored sample reads its bytes from disk and plays.

## Live reload (folder watcher, ADR-0022)

- [ ] With the **Samples tab open**, freeze/generate a sample on a deck: it appears in
      the list within ~½ s — no tab switch needed — and the rows already showing keep
      their `#id` (no churn/reshuffle).
- [ ] Drop a hand-made `.wav` into `generated_samples` in **Finder**: it appears live
      as **Imported** (a loop, no prompt) and loads into a slot.
- [ ] Delete a `.wav` from `generated_samples` in **Finder**: its row disappears live.
- [ ] The same live behaviour holds for the **Generate tab** and `generated_songs`
      (drop/delete a `.wav` there → the take list updates live).
- [ ] Editing `registry.json` by hand (or our own save rewriting it) does NOT trigger
      a reload loop (the watcher ignores it).

## Songs unchanged (no regression)

- [ ] Compose a **Track** in the Generate tab: it still saves to `generated_songs`
      and loads onto a deck as a playback track exactly as before.
