# 0014. Beat-matching via varispeed tracks against the measured stream

- **Status:** Accepted (2026-06-13, after the device pass — see
  `docs/m20-hardware-checklist.md`)
- **Date:** 2026-06-12
- **Deciders:** Daniel Peter

## Context

M20 wants real beat-matching. The booth has two kinds of deck since
M19: a live Magenta stream whose tempo cannot be steered (ADR-0004,
measured) but can be *measured* (M14's gated estimator), and a
playback deck holding its entire decoded buffer. That asymmetry
decides the sync model before any code is written: only the track can
change speed, so sync is one-directional — **the track follows the
booth**.

Three facts shape the design:

- A decoded buffer permits an *offline* beatgrid — BPM plus
  first-beat phase — far more accurately than any live estimate, and
  `AudioBufferSourceNode.playbackRate` gives varispeed for free
  (pitch shifts with rate; time-stretch is a separate, harder
  problem).
- The live deck's beat tracker (M14) runs on the **wire feed**, which
  leads the speakers by the player ring's buffer (~1.5–3 s). For BPM
  that lead is irrelevant; for *phase* it is fatal — a beat indicator
  derived from wire time would flash seconds before the audible beat.
  M14 dropped phase precisely because no consumer needed it; the
  phase meter is that consumer, and it needs phase **at the
  speakers**.
- The FLX4's tempo sliders have been deliberately unmapped since M7
  because generation tempo is not a parameter. Track playback rate is
  not generation tempo.

## Decision

- **Offline beatgrid per track, behind the M14 honesty rule.** At
  load, alongside the existing BPM pass, a pure analysis derives the
  grid under a constant-tempo model — and the period is *refined* in
  the same computation: detected onsets regress onto a grid (anchor
  and period jointly), because folding a whole track with the
  estimator's ±1–2 % coarse BPM smears a beat of phase every ~50 s.
  The regression residual *is* the drift check: a folder track that
  drifts past tolerance gets *no grid*, not a wrong one. No confident
  grid → no ticks, no meter, no quantise — varispeed and SYNC still
  work.
- **Varispeed via `playbackRate`, ±8 %.** A tempo control per playback
  deck sets the source's rate; the pure transport math gains a rate
  term (position advances at `rate` seconds per second) and the
  playhead re-anchors on every rate change so position stays exact
  across changes. Pitch shifts with rate — the classic varispeed
  trade-off, accepted; time-stretch is recorded in Later. The FLX4
  tempo sliders map to this (reversing M7's "deliberately unmapped"):
  ADR-0004 is untouched because playback rate is not a generation
  parameter — a realtime deck still ignores the slider.
- **SYNC matches tempo; the jog rides phase; the grid is not SYNC's
  gate.** SYNC needs only the two tempi — the track's gated BPM (times
  its current rate) and the other deck's gated BPM — refusing honestly
  when either gate is blank or the required rate falls outside the
  varispeed range. It deliberately does *not* require the grid, so the
  kill criterion (grid parks) cannot take tempo-matching down with it.
  Phase is the performer's: the jog becomes mode-aware like a real
  deck — **paused = seek** (M19's behaviour), **playing = phase
  nudge**, implemented as a stepped rate bend (±5 % for exactly as
  long as it takes to slip the requested milliseconds) managed by a
  pure slip accumulator: one bend at a time, new ticks extend it, and
  pause/seek/rate-set/unload all cancel it.
- **Live phase is measured at the speakers, not the wire.** The beat
  tracker grows a phase output: the pushed-sample index of the most
  recent beat, from a recency-weighted fold of its onset envelope by
  the detected period, shown only while consecutive anchors agree
  (anchor continuity — the meter must not flicker per estimate). The
  player worklet reports its **cumulative consumed frames plus its own
  `currentTime`** in the stats message it already posts, so the played
  index extrapolates in the audio clock the track transport also uses
  — one counter, owned by the thing that consumes the frames, reset
  exactly where the ring already resets. The phase meter compares the
  track's grid clock against this played-beat clock and goes blank the
  moment any leg fails: gate blank, no grid, track paused, worklet not
  playing, or stats stale — never a confident-looking lie.

### Amendment (2026-10-15)

- **The envelope is the performer's: ±8 % default, ±16 % or ±50 %
  selectable** (Settings → Audio → Tempo range, the Serato ranges). A
  library of generated tracks spans 96–150 BPM in practice, so ±8 %
  refused most cross-cluster pairs. The webview clamps the knob, the
  FLX4 slider, and SYNC to the chosen range; the engine boundary now
  holds the widest (±50 %) as its garbage guard. Pitch still shifts
  with rate — the wider ranges are opt-in for that reason.
- **SYNC folds metrical levels.** The required rate is read at
  whichever of half, same, or double lands nearest unity (the
  detector's own octave folding, ADR-0035), so a 70 BPM half-time
  reading syncs to a 140 BPM stream at ×1 instead of refusing at ×2.
- **A refusal says how far**: "needs +15 %, tempo range is ±8 %", so
  the performer can tell a near miss from a different-tempo track.

## Consequences

- Easier: track-against-track sync (both decks in playback) is exact —
  two grids, two known playheads. The hard case, track against stream,
  is honest: tempo locks by measurement, phase locks by jog and meter,
  exactly the two-step a DJ performs.
- The transport math stays pure and testable; rate-aware position and
  the bend arithmetic live next to the existing seek/clamp math with
  the same unit-test treatment, against click tracks and drifting
  fixtures.
- Varispeed changes the track's effective BPM readout; the UI must
  show grid-BPM × rate, not the stored number, or SYNC would look like
  it did nothing.
- A stepped bend momentarily detunes the track (~±5 % during a nudge)
  — audible if held, exactly like dragging a platter; accepted as the
  honest physical metaphor.
- The phase-at-speakers mapping inherits the worklet stats cadence; a
  fresh stats message can step the estimate by a few tens of
  milliseconds. Accepted for a meter; the audible lock comes from the
  performer's ears, the meter is the instrument-panel confirmation.
- Beat phase on arbitrary folder material is the standing risk: the
  grid pass must prefer silence over a wrong grid, and the milestone's
  kill criterion (grid features park, varispeed ships) stays live
  until the corpus says otherwise.

## Alternatives considered

- **Time-stretch (tempo without pitch)** — needs a granular/PSOLA
  worklet on the playback path; real engineering for v1 when ±8 %
  varispeed is the established DJ envelope and pitch shift at that
  range is mild. Recorded as the upgrade path in Later.
- **Steering the stream's tempo instead** — re-litigates ADR-0004
  against measurement; rejected. The stream is the tempo authority
  precisely because it cannot follow.
- **Auto phase-jump on SYNC** (snap the track to the nearest beat) —
  a position jump mid-performance is audible and surprising; the
  meter-plus-jog two-step keeps the performer in charge. Revisit once
  the meter has proven trustworthy on the device.
- **Phase from the wire feed** (ignore the buffer lead) — off by
  seconds; a meter that flashes before the beat is worse than no
  meter. Rejected on arithmetic.
- **Micro-seek nudges** (jump the playhead a few ms per jog tick) —
  every tick risks a click and fights the buffer-source restart cost;
  the stepped rate bend is seamless and matches the platter metaphor.
