# Show-prep checklist

Feel and hardware checks that tests can't prove. Tick each by hand in the
running app before the show.

## Grabbable BeatView waveforms

The mixer's two close-up strips (not the deck overviews). Playback decks only.

- [ ] Hovering a playback deck's strip shows the grab cursor; holding the
      button shows grabbing. A live (Magenta) deck's strip shows a plain cursor
      and ignores drags.
- [ ] Paused: dragging left pulls later audio under the playhead (the waveform
      follows the finger), dragging right goes earlier. Silent. Release leaves it
      where you dropped it. Works in the vertical layout too (drag up = later).
- [ ] Playing: dragging with the scroll (left / up) audibly speeds up a touch,
      against it slows down a touch. Never stops, never reverses, no scratch.
- [ ] Playing: holding still on the strip changes nothing.
- [ ] Playing: a hard fast drag stays a gentle bend (about 10 % at most), with
      no clicks or stutter.
- [ ] Release returns to the set tempo smoothly and keeps the new phase (no
      snap back). Beat marks of two decks can be lined up this way.
- [ ] Mouse and trackpad both work (click-drag and three-finger/press-drag).
- [ ] The FLX4 jog wheels still behave exactly as before (nudge while playing,
      fine seek paused, SHIFT fast scrub).

## Deck layout: transport on top

- [ ] With the Media Explorer open, both decks show PLAY/STOP and (on a
      track) the overview waveform without scrolling the deck.
- [ ] Playback deck, top to bottom: play + overview, title + position/BPM,
      tempo / SYNC / Back to live + hot cues, Color FX, Loops (loop in/out,
      beat loops, freeze pads), Generate to pad.
- [ ] Realtime deck, top to bottom: play + buffer/BPM/underruns/gen speed,
      style pad, Color FX, Loops (freeze pads), Generate to pad.
- [ ] Pressing PLAY on the beat while watching the overview playhead feels
      immediate; clicking the overview still seeks.
- [ ] A long track title ellipsises instead of pushing the clock off the row.
- [ ] Narrow window: the health readouts stay on one line beside PLAY; nothing
      overflows the deck column.

## Realtime deck samples a playback deck

- [ ] Load a track on deck B (paused is fine) and switch deck A to live:
      "Sample deck B" on deck A is enabled.
- [ ] Mid-track, press it: a ⏺ B·n chip lands on deck A's pad and blending
      toward it pulls deck A toward the ~10 s of the track just before the
      playhead.
- [ ] With deck B parked at the top (0:00), sampling still works (it takes the
      first 10 s).
- [ ] Sampling does not disturb deck B's playback (no click, no stall).
- [ ] With deck B back on live and stopped, the button is disabled again.

## Media Explorer: MCP recall, tag filter, crop

- [ ] Songs → ⋯ on a take the MCP agent generated (an odd length like 3:33) →
      Reuse settings: the form fills in (prompt, Length shows 3:33, the LoRAs,
      Advanced with the fixed seed) instead of "saved recipe is invalid".
      Compose from it and the take comes out the same length.
- [ ] The tag row above the Songs list shows every tag in the list; clicking
      two narrows to rows carrying both; "Clear tags" brings everything back.
      Same on Samples with its own tags. Clicking a #tag on a row toggles it too.
- [ ] With a tag filter on, the FLX4 browse knob walks only the shown rows and
      LOAD loads the highlighted one.
- [ ] ⋯ → Crop… on a song with a silent tail: the waveform appears; dragging
      either handle (mouse and trackpad) moves it smoothly and the fields
      follow; clicking the waveform moves the nearer handle.
- [ ] Auto-trim silence lands both handles just past the gaps (it leaves a
      sliver rather than cutting sound).
- [ ] Preview plays only the selection in the phones, never the master; Stop
      stops it; closing the panel mid-preview stops it.
- [ ] Save as new clip: a "<title> (crop)" row appears with the original's
      tags and prompt, unrated; it loads to a deck, starts and ends without a
      click, and the original file is unchanged. Same for a sample (one-shot
      and loop) — a cropped loop still loops cleanly in its slot.
- [ ] Replace original on a rated, tagged, playlisted song: the same row keeps
      its stars, tags and playlist slots, plays the cropped audio, BPM/key
      refill, and the old file is in the Trash.
