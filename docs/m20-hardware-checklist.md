# M20 hardware checklist — beatgrids and sync

Manual verification of the M20 exit criteria with the device and
ears. The measurable half is `verify_m20.mjs` (in `just verify-ui`);
this covers the audible lock, the platter feel, and the slider
orientation no script can judge (ADR-0014).

**Passed 2026-06-13** across three device runs, each feeding fixes
back in (the run findings are recorded inline below: slider
orientation, scrub CC, jog sensitivity, bend responsiveness, mark
visibility). Final verdict: *"Ok, that works! Very different from
what I expected, but a truly novel take on beat matching. I like
it."*

## Setup

- [x] FLX4 connected (green LED), deck A streaming steady techno with
      a gated BPM showing, a composed techno track loaded on deck B
      and playing.

## Tempo and SYNC

- [x] The FLX4 **tempo slider** on deck 2 rides deck B's rate: the
      BPM readout follows, the pitch audibly shifts with it (the
      varispeed trade-off, ADR-0014). Orientation was measured
      inverted on the first device run and flipped in
      `tempoSliderToRate` — re-confirm it now feels right.
- [x] First touch of the slider jumps the rate to the slider's
      position (no soft-takeover — consistent with volume/EQ). Note
      if this is jarring in practice.
- [x] **SYNC** on screen matches deck B's readout to deck A's gated
      BPM in one press; with the slider parked at an extreme so the
      target is out of range, SYNC refuses with the message instead
      of landing close.

## The audible lock (the exit criterion)

- [x] With tempos matched, ride the **jog while playing**: a slow
      turn bends gently (~10 ms/tick), a faster turn bends audibly
      harder and settles within a beat or two of letting go — no
      clicks, no jumps. (The third device run found the old fixed 5 %
      bend consumed slip far slower than the platter piled it up —
      "the jog doesn't move anything"; the bend now steepens with the
      backlog and the backlog caps at 1 s.) Judge the feel; note
      better values for `JOG_NUDGE_SECONDS` / `BEND_CATCH_UP_SECONDS`
      if it drags or snatches.
- [x] **SHIFT + jog scrubs** while playing (the CDJ search
      convention; the second device run found the firmware sends
      SHIFT+jog on its own CC `0x29`, now mapped). Plain jog on a
      paused track seeks *finely* (0.05 s/tick — 0.5 read as "way too
      sensitive"); note better values for `JOG_SEEK_SECONDS` /
      `JOG_SCRUB_SECONDS` if the feel is off.
- [x] Nudge until the kicks coincide: the **phase meter** needle sits
      centre when your ears say locked — the meter must agree with
      the room, not the wire (the buffer-lead correction, ADR-0014).
- [x] The **beat view** (centre, switchable to top bar / off in the
      statusbar — M22, pulled forward) shows both close-ups scrolling
      with the audio, kicks blue-heavy and hats bright; when locked,
      the beat marks of both strips visually coincide and the live
      strip's marks land on its audible kicks. The marks are grid-red
      now (the muted gray of the first cut was "barely visible" on
      the device).
- [x] The lock holds for **a minute** by ear with the meter steady;
      small drift corrects with single jog ticks.
- [x] Pause deck B: the meter goes dark (no track clock); the jog
      reverts to seeking. Stop deck A's stream: dark again (no live
      clock). It must never show a confident needle without both.

## Grid honesty

- [x] The beat ticks on deck B's overview are *visible* (the first
      device run found per-beat ticks fusing into a solid band on a
      2-minute track — they now stride up to downbeats/bars) and the
      heavy marks line up with the audible kicks across the track.
      If no ticks at all, the take refused a grid — the console's
      `[beatgrid]` lines say why; compose another take.
- [x] Load a beatless track (generate an ambient drone): no ticks, no
      meter, BPM dash — and SYNC still works if the *coarse* verdict
      exists, refuses honestly if not.

When every box ticks, flip M20's status in [`ROADMAP.md`](ROADMAP.md)
to ✅ done and ADR-0014 to Accepted.

## Tempo range and octave sync (2026-10-15, ADR-0014 amendment)

- [ ] Settings → Audio → **Tempo range** at ±16 %: the FLX4 tempo
      slider's full throw now spans ±16 % (the BPM readout confirms
      at both ends) and the on-screen tempo knob matches; back at
      ±8 % the slider feels as before.
- [ ] With two tracks ~12 % apart, SYNC at ±8 % refuses with the
      percentage it needed; switch to ±16 % and the same press locks.
- [ ] A half-time reading (a 70-ish BPM readout against a 140-ish
      deck) syncs at unity instead of refusing — the readouts land
      an octave apart, the beats line up on the meter.
- [ ] At ±50 %, an extreme rate still plays clean (no clicks, the
      loop and jog bend behave) — it just sounds like a varispeed.

