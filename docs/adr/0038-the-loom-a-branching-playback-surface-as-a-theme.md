# 0038. The loom: a branching playback surface, as a theme

- **Status:** Proposed
- **Date:** 2026-10-30
- **Deciders:** Jake Hartnell

## Context

A DJ set is a path through a space of possible music. With Stable Audio 3 in
the booth that space is real: any clip can be continued, re-rolled or
branched in a few seconds. Today's booth shows none of it. Each deck column
holds one track; alternatives live in the explorer as a flat list, and
nothing records that one take continues another.

The pieces for a branching surface already exist. Playback decks decode and
play library songs (ADR-0013, ADR-0030). `/api/generate` takes a source
window and continues it (continue mode, ADR-0037); the output contains the
kept tail at its start, re-decoded. The songs library rates, tags, analyses
and lists every render (ADR-0036). The design is mocked up and settled in
[`mockups/loom/README.md`](../../mockups/loom/README.md), the design record
for this ADR: vocabulary, the shuttle rules, and the open questions.

The forces:

- **The engine has no hand-over.** A load replaces the deck's buffer and the
  shell re-decodes. Played from the webview's 4 Hz poll, a seam would be late
  by up to a poll plus a decode, or silent. On a show, silence is the
  failure.
- **Render time is real.** SA3 medium renders at about 0.125× realtime plus
  ~3 s to start (ROADMAP M19), so a plain 16-bar clip takes ≈7 s. A LoRA
  stack adds a flat ≈128 s merge per adapter
  ([`mcp-findings.md`](../mcp-findings.md) #16). Nothing stays warm
  (ADR-0012).
- **Generated audio drifts.** SA3 is told the tempo; it does not always keep
  it, and the detected grid on generated material can be wrong (M20).
- **The DJ booth works.** Its tests, hardware checklists and the measured
  FLX4 map are the regression suite. A new surface must not cost the old
  one.

## Decision

We will build the **loom**, a branching playback surface for Stable Audio 3
clips, as a second **theme** over the existing engine, mixer, explorer and
MIDI layer. The shuttle rules in the mockup README are part of this decision.

- **A theme, not a fork.** `AppSettings.theme: 'dj' | 'loom'` (Settings →
  Appearance), default `dj`, carried on `<html data-theme>`. Only the booth
  swaps. The statusbar, settings drawer, explorer, performance visuals, MIDI,
  both `useDeck`s and the crossfade/cue-mix projections are shared.
- **A node is a song.** Every render is saved through `save_generated_song`
  with model `track`; the recipe's `source` marks a continuation. A loom is a
  small graph over song filenames. Discarding a take removes it from the loom
  and leaves the file in songs: the songs library is the trash. Keep closes a
  take group and is never refused: every sibling take that no deck is on or
  tied to, and that has no children, is discarded; a protected sibling stays
  as an ordinary node.
- **The nominal grid.** A loom has one tempo per root. One default, 124 BPM,
  serves both a first weave and a dropped song with no registry BPM; a dropped
  song otherwise keeps its registry BPM. A bar is `240 / bpm` seconds. Seams,
  loops, cuts and trims snap to that grid from the node's own bar 0; the
  detected grid is not used for scheduling. Every
  derived end is clamped to the real clip, and a dropped song's bar count is
  floored, so a nominal end never runs past the audio.
- **Trim moves the seam, never under a branch.** The in-point can always
  move and only matters when a deck enters the node. The out-point is the
  seam, so trimming the node a deck is playing takes effect there. A node
  with children keeps its out-point (a refusal line says why): its children
  were rendered from the audio up to it.
- **Continue over the parent's last four bars.** A child is rendered in
  continue mode from the parent's last four bars up to its out-point. Its bar
  0 sits after the kept tail, so by default a child starts exactly where its
  parent ends. Remix-mode drift (`init_noise_level`) is not offered on
  continuations.
- **A seam is an engine swap.** The deck's buffer source holds one queued
  clip and the audio thread swaps to it when the playhead reaches the switch
  frame, checked before the loop fold and the end-of-clip branch. The swap is
  a move; the replaced buffer is dropped on the render thread, never on the
  callback. The webview queues a few seconds ahead and adopts the new track's
  analysis when it sees the swap counter advance.
- **Hold by default.** A pure scheduler turns the loom, each deck's runtime
  and its track status into one plan per deck (hold, loop or queue), applied
  idempotently on every poll. With nothing tied the deck loops the last four
  bars of its node; it never picks a branch and never goes idle while it has
  a node. A user loop wins over a tie. A tie (→A/→B) means next; a cut is a
  separate gesture that lands on the next bar.
- **Looms live beside the songs.** Each loom is a JSON file in
  `generated_songs/looms/<id>.json`, read and written by the shell
  (`looms.rs`, following `playlists.rs`) with atomic writes. The shell treats
  the body as opaque; the webview validates it on read. Deck runtime is never
  persisted. The webview is the only writer, so looms emit no
  `library://changed` event.
- **Autosave, no save button.** The webview saves the loom, debounced, under
  the name in the top bar; a bar menu creates, opens, duplicates and deletes
  looms. Deleting a loom keeps its songs.
- **Cue from a bar.** `audition_song_clip(name, start, end)` decodes the song
  shell-side and feeds the existing audition path, so no audio crosses IPC
  and cueing never touches the master.
- **Render ahead.** A setting, on by default: when a playing deck (its track
  is playing; the M10 on-air flag plays no part) lands on a leaf with nothing
  rendering, one `auto` child (same prompt, 16 bars) is woven behind it, so a
  hold is a choice rather than an accident. Nodes with a LoRA stack are
  skipped and the deck says so.
- **Stable Audio 3 only, for now.** The loom is playback-only. A Magenta RT
  deck is an endless stream with no seam to branch at; live threads come back
  as a later layer.
- **The hardware mapping is unchanged.** Every FLX4 intent keeps its meaning
  through `useDeck`, with one theme gate: in Loom theme App hands the loop
  section to the loom. 4 BEAT/EXIT (`track_beat_loop`) starts a four-bar
  loom loop on the nominal grid instead of the grid-required beat loop, or,
  while any loop runs, releases it; CUE/LOOP CALL ◄/►
  (`track_loop_halve` / `track_loop_double`) step a running loom loop
  4 → 8 → 16, clamped. LOOP IN and OUT keep their engine behaviour: a region
  the scheduler did not place is a user loop, and the scheduler places
  nothing until it is gone. BEAT SYNC stays unmapped on the FLX4; the loom
  uses the on-screen SYNC. No byte changes; loom-specific pads and jog are
  not built.

## Consequences

- The engine gains one branch on the audio thread (`queue_track` /
  `clear_queued_track`, `queued` and `swaps` in `TrackStatus`). A Rust test
  proves the frame after a switch is the queued clip's start frame, with no
  zero frame between and `assert_no_alloc` still armed.
- The shell gains `looms.rs`, `audition_song_clip`, `stitch_songs` (bounce a
  played path into one song), and the queue/clear/promote track commands.
  The webview gains `frontend/src/loom/`:
  a pure reducer, nominal-grid timing, layout, scheduler and render queue,
  plus the view, the mixer bar and the loom bar.
- The DJ theme is untouched and stays the default until the loom has been
  played on. Two surfaces now share the deck and mixer code; a change there
  must hold in both, and the DJ checklists are the regression suite.
- **Tempo drift against the nominal grid** is the accepted risk: a render
  that drifts from its stated tempo seams off the beat. It shows on the node
  (bar ruler against waveform) and trim fixes it. If it is frequent, a bar-0
  estimate from the detected grid may *suggest* a trim; it never moves a seam
  by itself.
- **LoRA renders cannot be woven live** at ≈128 s each. LoRA threads are
  rendered before a show, and render ahead skips them. A warm merge sidecar
  is a separate later phase.
- Every take is a file. The songs library grows with every weave; ratings and
  tags are how keepers are found. There is no undo for a discard beyond "the
  song is still in the library".
- A relaunch reopens the newest loom with every node and the decks stopped;
  →A on any node resumes the set. A node saved while queued or rendering
  reopens as failed, and its retry weaves it again, so no node waits on a
  job that died with the app.
- The new track's analysis (synced echo clock, zoom bands) follows a swap by
  at most one 250 ms poll. The audio does not wait for it.
- The shuttle rules' blinking FLX4 play LED at HOLD is not part of v1; the
  amber HOLD warning is on screen only.
- Left for later, each its own decision: drift on continuations, a negative
  in-point to expose the kept tail, quantised needle drops (v1 seeks at once),
  an explorer looms tab, live threads, loom-specific FLX4 pads and jog, MCP
  tools over the loom, tempo changes inside a loom.
- This ADR flips to Accepted once [`loom-checklist.md`](../loom-checklist.md)
  passes on the FLX4.

## Alternatives considered

- **Replace the deck UI outright** — throws away a working instrument and the
  regression suite it carries, and leaves no fallback mid-show. As a theme,
  either surface can be dropped later without a migration.
- **Nodes as a separate library** — a second audio store with its own
  registry, analysis and curation, duplicating what the songs library already
  does. As songs, nodes are rated, tagged and analysed for free, and a
  discard is never destructive.
- **Reload at the seam from the webview** — a load replaces the buffer and
  re-decodes, so the seam lands a poll and a decode late, or as a gap. Only
  the audio thread can hand over on the frame.
- **A pre-stitched thread as one WAV** — commits the path before it is
  played; every change of next means re-stitching. Stitching stays useful for
  exporting a played path (bounce), not for playing one.
- **Realtime threads first** — a Magenta RT stream has no end to branch from,
  so takes, ties and holds have nothing to attach to. Playback-first settles
  the seam mechanism the live layer will need anyway.
