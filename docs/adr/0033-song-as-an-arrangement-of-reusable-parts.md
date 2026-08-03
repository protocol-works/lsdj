# 0033. A song is an arrangement of reusable parts

- **Status:** Accepted (2026-07-10)
- **Date:** 2026-07-10
- **Deciders:** Daniel Peter

## Context

SA3 song generation is strictly **one-shot**: one prompt renders one WAV, up to
`TRACK_MAX_SECONDS` = 380 s (6:20), authored in the Generate tab of
`frontend/src/media/MediaExplorer.tsx` and persisted as a flat `GeneratedTrack`
in the songs library ([ADR-0013](0013-playback-decks-play-decoded-tracks-loading-decides-the-mode.md),
`src-tauri/src/songs.rs`, `~/Documents/LSDJ/generated_songs/` + `registry.json`).
There is **no song-structure concept anywhere** — no parts, sections,
arrangement, or ABAB — at any layer (frontend state, the `/api/generate`
contract, the SA3 CLI, or persistence).

Two forces make that a real limit:

- **Songs are structured and repetitive** — verse/chorus/hooks, sections that
  return. A one-shot SA3 render over minutes meanders and **cannot reliably
  bring a section back identically**; the model has no notion that bar 96 is
  "the chorus again."
- **The primitives for something better already exist.** Issue #54
  ([ADR-0012](0012-generated-pads-via-a-spawned-sa3-mlx-subprocess.md) amendment)
  threaded SA3's `init_audio`/audio-to-audio, `inpaint_range`, `negative_prompt`,
  numeric `cfg`/`apg`, and fixed `seed` through `/api/generate`, but **API-only**
  — none of it is reachable from the UI, and none of it composes into a song.

Persistence is **Rust-shell-owned**: the webview is untrusted, and the
security-critical path/registry helpers live once in `src-tauri/src/library.rs`
([ADR-0022](0022-persist-generated-samples-and-loops.md)). Today the songs
`registry.json` records each take **inline** — its WAV filename (the registry
identity), title, a single `prompt` string, and engine/model (`SongEntry` in
`src-tauri/src/songs.rs`). That row holds a one-shot prompt but cannot express a
structured song. Any new persisted entity must ride that boundary, not a
webview-supplied path.

A decision is needed on **what a structured song *is*** — its data model,
where it is persisted, and where it is authored — before the render mechanics
([ADR-0034](0034-structured-song-render-via-backend-continuation-conditioning.md))
or any UI can be specified.

## Decision

We will model a structured song as **parts referenced by an arrangement**, and
add it as a second generation mode beside one-shot:

- A **Part** is a **prompt extension** — text that extends the song's base
  prompt — plus optional SA3 controls (`seed`, `cfg`/`apg`, `negative_prompt`,
  init audio) and a length; it renders to **exactly one clip**. A part's
  *effective* prompt is the song's base prompt combined with the part's
  extension, so each part inherits the song's identity and only says how it
  differs ("+ sparse intro, no drums" / "+ full drop, driving bass"). A part may
  also be a **variation of another part** — it inherits that parent's extension
  and controls and renders conditioned on the parent's clip to stay recognisably
  the same, written `A'` in an arrangement.
- An **Arrangement** is an ordered sequence of part references expressed as a
  letter string (`ABABCD`, `ABCBCD`, `AB A' B`, …). Each slot resolves one of
  three ways: a **fresh letter** (`A`, `B`, `C`) is a new part and the only slot
  that costs a full generation; a **repeated letter** reuses the same rendered
  clip, so a repeat is free and a returning chorus is byte-identical rather than
  a re-roll; a **variation** (`A'`) renders its own clip but derives from its
  parent, landing as "the same, evolved" (e.g. `AB A' B`, a subtly reworked
  second verse). Patterns stay cheap: only fresh parts and variations generate,
  repeats are reuse.
- A **Song** is `{base prompt, parts, arrangement, coherence settings}`. The
  **base prompt** is the shared musical foundation every part extends; the
  coherence settings are a shared `seed` / target BPM / key applied across
  parts.

We will keep **one-shot generation exactly as it is** — a distinct mode, the
existing single-prompt/single-length path — and add **structured/pattern** as
the new mode.

We will keep the **output artifact shared, and only split the authoring
surface**:

- The **final rendered song is a flat `GeneratedTrack` WAV** that loads to a
  deck through the existing MediaExplorer save/preview/load plumbing.
  Persistence of the *audio* is **not forked** — a consumer that only plays a
  track keeps seeing a normal track.
- The **editable song generation config** — base prompt, parts (each with its
  extension, controls, and optional parent ref), arrangement, and coherence
  settings — is **serialized as its own artifact under a `configs/` subfolder of
  `generated_songs/`, and referenced by relative path from the song registry
  entry**. The flat `prompt` string a `SongEntry` stores today cannot express a
  structured song, so the config is serialized separately and the registry row
  points at its `configs/…` path (the rendered `.wav` stays the registry
  identity). It is written through the Rust shell, and the relative path is
  resolved through the existing `scoped_path` boundary so a registry-supplied
  path cannot traverse outside the library — so a song can be reopened and its
  parts re-rolled.
- The song is authored in a **standalone Song Builder window** applying the
  [ADR-0032](0032-standalone-midi-keyboard-window.md) pattern verbatim — its own
  Tauri window (`index.html?window=…`, `main.tsx` branches on the param), scoped
  into the default capability, a toggle command that creates/shows/hides,
  close-intercepted-to-hide, and visibility mirrored to the store. The window
  also **lists the saved configs from `configs/`** so a performer can reload a
  previous song's config and re-roll or adapt it, not just replay its rendered
  WAV.

## Consequences

- **Real repetition, and cheaper.** A song renders only its *distinct* letters,
  not its full length; repeats reuse audio, so `ABABCD` is four generations and
  a returning chorus is identical, not approximate.
- **Per-part reroll.** Disliking the bridge re-rolls `C` alone; `A`/`B` stand.
  This is why the editable document, not just the WAV, has to persist —
  otherwise a song can't be reopened and reworked.
- **Base + extension keeps a song coherent by construction.** Every part is a
  delta on one shared identity rather than N unrelated prompts, so cross-part
  drift is bounded before rendering even starts. The trade: editing the base
  prompt changes every part's *effective* prompt at once; already-rendered parts
  keep their audio until re-rolled, so a base-prompt change is heard only on the
  next render.
- **Variations reuse the same machinery.** `A'` is not a new render concept — it
  is #54's existing variation surface
  ([ADR-0034](0034-structured-song-render-via-backend-continuation-conditioning.md))
  pointed at its parent's clip — audio-to-audio for a global evolution, or
  inpainting to rework just a window — so "a slight variation of A" costs one
  conditioned generation and needs no new model surface.
- **A new persisted schema to own.** The song generation config — base prompt,
  parts (each optionally referencing a parent it varies), arrangement, and
  coherence settings — is new project state with a versioned shape and a
  migration surface. It is **serialized separately and referenced from the
  `SongEntry` registry row** (which today holds only a flat `prompt` string)
  rather than squeezed into that field, and it rides the `library.rs` boundary
  rather than inventing a new trusted path.
- **Configs become a reusable library.** Because configs are saved under
  `generated_songs/configs/` and listed in the Song Builder window, a good
  arrangement can be reloaded and re-rolled or adapted later — a song is a
  reusable recipe, not just a one-off rendered WAV.
- **Render and coherence are deferred.** *How* distinct parts are made coherent
  and stitched (continuation conditioning, beat-aligned seams, backend
  orchestration) is [ADR-0034](0034-structured-song-render-via-backend-continuation-conditioning.md),
  not this ADR. This ADR only fixes the model, persistence, and surface.
- **Multi-window cost, already paid once.** A second authoring window inherits
  ADR-0032's scoping/lifecycle obligations (capability scope, hide-not-close,
  store mirror); no new mechanism, but the discipline applies again.
- **Length wants bars, SA3 speaks seconds.** A part length is most musical in
  bars, but SA3 has no tempo parameter
  ([ADR-0004](0004-style-is-a-weighted-prompt-blend-tempo-is-not-a-parameter.md)),
  so a bar↔seconds mapping needs a target BPM. That ties structured songs to the
  beat-estimator work in issue #77 for both bar-accurate parts and clean seams.

## Alternatives considered

- **Keep one-shot only, lean on longer generations** — rejected: no reliable
  repetition, a full-length render is the most expensive path, and nothing is
  rerollable per section.
- **A flat list of independent clips, no letters/reuse** — rejected: it loses
  the "a repeat is free reuse" insight that makes patterns cheap and makes a
  returning chorus *identical*; every "same" section would be a separate,
  drifting generation.
- **Persist only the rendered WAV, not the editable document** — rejected: a
  song could be played but never reopened or re-rolled, defeating the point of
  modelling structure at all.
- **A richer mode inside the Generate tab rather than a window** — rejected this
  session: an arrangement editor with per-part settings and a timeline wants
  space the docked media tray doesn't have, and ADR-0032 already sanctions a
  standalone window for exactly this kind of app-wide authoring surface.

<!-- Status values: Proposed | Accepted | Rejected | Deprecated |
     Superseded by ADR-NNNN -->
