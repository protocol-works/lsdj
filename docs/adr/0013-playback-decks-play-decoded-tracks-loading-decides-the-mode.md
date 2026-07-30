# 0013. Playback decks play decoded tracks; loading decides the mode

- **Status:** Accepted
- **Date:** 2026-06-12
- **Deciders:** Daniel Peter

## Context

M19 gives a deck a second life as a classical DJ deck: instead of the
live Magenta stream, it plays a single finished track — generated from
a prompt (SA3 Medium composes up to 6:20; the Magenta render worker
can compose shorter pieces) or loaded from a local folder — through
the unchanged channel strip, so EQ, Color FX, trim, fader, cue,
crossfader, and the FLX4 surface all keep working.

The roadmap left the source architecture open with a leaning: feed
decoded track PCM through the existing player ring "so the waveform
strip, beat tracker, freeze capture, and style sampling all keep
working unexamined". Reading the code shows that claim is weaker than
it looks:

- The waveform strip and meters tap the channel **output** (a
  post-fader analyser) — they work for any source, ring or not.
- The beat tracker feeds on the **WebSocket push**, not the ring; a
  ring feed only helps if the client re-pushes the track in paced
  chunks, re-creating the worker's pacing loop in the browser.
- Freeze capture and style sampling read the ring's **history** of
  played audio — machinery that exists because live audio is
  ephemeral. A decoded track is not ephemeral: the whole buffer is in
  hand, so any "capture" is a slice.
- The ring path has no transport: pause, seek, and a playhead would
  all need new worklet messages, while `AudioBufferSourceNode`
  playback — already proven in this exact graph by the M13 freeze
  loops — gives start-at-offset, `onended`, and position arithmetic
  for free.

Two more forces: the parked Magenta worker's fate (an idle worker
holds ~2 GB resident; releasing it buys RAM but costs a model reload
on return), and how the user switches modes at all (a standalone
toggle invites invalid states: a Playback deck with no track, a
Realtime deck holding a stale buffer).

## Decision

- **A deck's source is a mode: `realtime` (the live Magenta stream,
  today's behaviour) or `playback` (one decoded track).** Playback
  plays the track as an `AudioBufferSourceNode` at the channel head —
  the freeze-loop pattern with `loop = false` — summed at the same
  trim node, so everything downstream is untouched. The live worklet
  stays connected but silent; the player ring is not involved.
- **Loading decides the mode; there is no mode switch.** A new Media
  Explorer pane below the booth owns loading: it folds in the M16
  crates and adds Generate (prompt → track) and a minimal local-folder
  browser (session-only pick — no new storage layer for a directory
  handle). Loading a crate puts the deck in `realtime`; loading a
  track puts it in `playback`. So that playback always has an exit,
  each deck's live stream is itself a loadable item — and the playback
  deck carries its own "Back to live" control, the same exit within
  reach of the deck (a user with no saved crates must never depend on
  finding the explorer's live row). Returning to `realtime` is a load
  or that explicit exit, never a free-floating mode toggle. The FLX4
  rotary scrolls
  the *visible* tab's list and LOAD loads the highlighted item,
  whatever its type; "unified" means type-aware loading, not a hidden
  cross-tab list, and the measured byte map is untouched.
- **Track-deck features read the buffer, not the ring.** Position and
  seek are context-time arithmetic plus a new source at an offset;
  BPM comes from running the decoded buffer through the M14 tracker
  once at load (the honesty gate sampled at the same cadence over the
  buffer — one number per track, accepted even if a composed piece
  drifts mid-way). Style sampling and freeze captures from a track
  will slice the buffer at the playhead — deferred past M19; until
  then both are disabled on a playback deck, because the ring history
  holds the dead stream and must never leak into a capture.
  End of track is explicit: silence, position parked at the end,
  state visible.
- **The parked worker idles warm.** Entering playback sends `stop`
  (the worker blocks on its command queue — no CPU); leaving restores
  the stream as a normal play. No release-and-respawn in v1: the M3
  restart machinery is the recorded fallback if RAM measurement says
  otherwise.
- **Track generation reuses ADR-0012's paths.** SA3 tracks run the
  same spawned CLI with `--dit medium --decoder same-l` and longer
  bounds (Stability's 6:20 ceiling), still serialised; the ~5 GB
  transient and the one-time 5.9 GB weight download are pre-warmed
  outside a request, as the small models were. Magenta tracks run
  through `/api/render` with the timeout made proportional to the
  requested seconds, floored at the current 90 s (the flat value
  assumes pad-length clips) and a length cap set by the measured
  real-time factor (1.86× on this machine); a Magenta track render
  visibly queues pad renders behind the single worker — accepted, not
  engineered around. The same acceptance holds on the SA3 side: a
  medium track generation owns the generation lock for its whole wall
  time, so ADR-0012's "a queued request lands in ~3 s" does not hold
  while a track is composing — pad generations wait, visibly pending.
- **Generated-song provenance is optional and versioned.** The on-disk song
  registry retains the legacy title/prompt/model fields and may additionally
  carry a versioned recipe containing prompt, engine, duration, effective LoRA
  stack, and Advanced SA3 steering. Old and hand-added rows omit it. The
  Generate tab validates a supported recipe before recall, leaves Title alone,
  reports unavailable adapters instead of substituting them, and restores a
  random take's actual seed as Fixed. Unknown versions remain playable but are
  not applied. The WAV folder is still the library's source of truth.

## Consequences

- Easier: transport (pause/seek/CUE-to-top) is plain Web Audio with
  no worklet protocol changes; captures from a track are
  sample-accurate slices instead of ring history; BPM on a track is
  known the moment it loads; the deck strip and FLX4 mappings carry
  over untouched because the channel graph is unchanged.
- The Media Explorer makes the mode an invariant of what was loaded —
  no toggle, no invalid states — and gives the hardware one browse
  surface. The cost is that the crates UI moves home (M16 muscle
  memory changes) and the browse intents become item-type-aware.
- A decoded 6:20 stereo track holds ~145 MB of float32 in browser
  memory, session-only like every captured artefact. Accepted; one
  track per deck bounds it.
- The beat tracker runs out-of-band at load for tracks, so its code
  now has two callers (stream push and file analysis) — kept honest
  by the same gate thresholds.
- An idle-but-warm parked worker keeps ~2 GB resident during
  playback. On the 16 GB reference machine with a medium generation
  in flight (two decks + render worker + ~5 GB transient) the
  headroom is thin; the existing RAM warning covers it, and
  release-on-park is the revisit if it bites.
- Medium-model quality and wall time on this machine are unjudged
  (the roadmap's standing risk); the first build measures both, with
  park-here as the kill criterion for SA3 tracks specifically —
  Magenta tracks and folder playback stand on their own.
- A generated WAV now survives the session in the shell-owned songs folder,
  with the optional recipe in `registry.json`; browser-only sessions retain the
  take in memory. Recipe metadata adds reproducibility without changing deck
  loading, decoding, or playback ownership.

## Alternatives considered

- **Feed decoded track PCM through the player ring** (the roadmap's
  leaning) — re-creates worker pacing in the browser, needs new
  worklet messages for hold/seek/position, and the promised "keeps
  working unexamined" payoff is mostly illusory: the output taps work
  for any source, the beat tracker feeds on the push not the ring,
  and slice-based capture beats ring history when the whole buffer is
  known. Rejected for buying complexity with benefits the buffer
  source already has.
- **A separate per-deck mode toggle with a pane swap** (the roadmap's
  generation-pane sketch) — couples generation to a deck before a
  track exists, can't represent "generate while both decks play", and
  invites a Playback deck with nothing loaded. Rejected for the
  loading-implies-mode invariant.
- **Release the parked worker to reclaim ~2 GB** — buys RAM during
  playback but costs a model reload (tens of seconds) on every return
  to the live stream, plus restart-failure handling on a path users
  will treat as instant. Deferred, not refused: the machinery exists
  (M3) and this ADR records it as the response if memory pressure is
  measured, per-deck.
- **A second render worker for Magenta tracks** — avoids queueing pad
  renders behind a track render at the price of another ~2 GB
  resident model on a machine already hosting three. Rejected;
  honest pending state is cheaper than RAM.
- **A user-selected track-library database** — rejected in favor of the later,
  simpler shell-owned generated-songs folder plus its small provenance registry.
  Folder tracks still re-decode from their selected source; generated songs
  re-decode from the fixed app library.
