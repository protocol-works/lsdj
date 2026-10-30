# Loom checklist — the branching playback surface on the FLX4

Manual verification of M26 ([ADR-0038](adr/0038-the-loom-a-branching-playback-surface-as-a-theme.md))
with the device and ears. The loom reducer, nominal-grid timing, layout,
scheduler and render queue are unit-tested, and a Rust test proves the engine
swap sample-continuous; this checklist covers what they cannot: the real
window, the FLX4, the phones, real render times, and a recorded set. The
expected behaviour is the shuttle rules in
[`mockups/loom/README.md`](../mockups/loom/README.md).

## Setup

- [ ] `just tauri-dev` (or a `just tauri-build` app), FLX4 connected (green
      LED), phones on the FLX4 jack, master to speakers or an interface.
- [ ] Settings → Model library: the Stable Audio 3 model and at least one
      LoRA adapter installed. Settings → Appearance: Performance visuals on.
- [ ] Settings → Recording: note the recordings folder.

## Theme switch

- [ ] Settings → Appearance → Theme offers `DJ | Loom`, with DJ selected on a
      fresh profile; the booth is today's two deck columns.
- [ ] Select Loom: the deck columns give way to the loom canvas with the mixer
      bar below it. Statusbar, explorer and settings drawer are unchanged; the
      top bar gains the loom name, a quiet saved mark and the loom menu.
- [ ] Switch theme while a track plays on deck A: the audio does not stop,
      click or reload.
- [ ] Quit and relaunch: Loom is still selected. Back to DJ, relaunch: DJ.

## Weave & takes

- [ ] On an empty loom, weave a root: a prompt, 3 takes, 16 bars, 124 BPM, no
      LoRA. Three ghosts appear in one take bracket and render one after
      another, each with a live ETA (`queued`, then `⟳ m:ss`).
- [ ] Each finished take shows its full-length waveform under the bar ruler
      and picks up a key tint once analysis lands. Each is in the explorer's
      Songs tab, titled from the prompt with `· take k`.
- [ ] Weave with Advanced open (cfg, apg, a negative prompt, a fixed seed):
      the render completes; ⋯ → Reuse settings on that song in the explorer
      shows the same values.
- [ ] Weave a root with one LoRA adapter: the ghost shows `⟐ merging` and an
      ETA of two minutes or more; it completes.
- [ ] Weave a child from a ready node with no prompt (continue): it starts at
      the parent's end on the canvas. Cue it from the join (⌾): the parent's
      last bars run into the child with no jump in tempo or key.
- [ ] ✓ Keep one take: the other takes leave the loom and are still in Songs.
- [ ] ⨯ Discard a single take: it leaves the loom; the file stays in Songs.
- [ ] Try to discard the node deck A is on, a node tied to deck A, and a node
      with children: each is refused with a status line saying why; nothing
      changes.
- [ ] Drag a song from the explorer onto the canvas: it becomes a root whose
      bar count fits inside the song (never past its end).

## Shuttle & seams

- [ ] →A on a node while deck A is stopped: the deck loads it at its in-point.
      The deck panel's status line reads now · next · bar.
- [ ] PLAY/PAUSE on the FLX4: the deck plays and the shuttle moves. The
      countdown tag shows bars to the seam and `→ HOLD`, turning amber at 8
      bars.
- [ ] **Hold is never silent.** Reach an untied seam: the deck loops the
      node's last four bars on the beat, heard as a loop, not a gap, until
      something is tied.
- [ ] While holding, →A on a ready child: the hand-over happens at the next
      loop boundary, on the bar, with no click and no silence.
- [ ] **Tied child hands over with no gap or click.** Tie a ready child
      before the seam: at the seam the deck hands over on the bar with no gap,
      click or doubled beat; the deck panel names the child; the BPM readout
      and zoom bands follow within a quarter second.
- [ ] Two ties queued (a child, then its child) play in order. ⨯ on the second
      unties it: the deck holds at the end of the first.
- [ ] Return: →A on a node back up the tree. At the seam the deck jumps back
      on the bar; the loom draws a return arc.
- [ ] **Cut lands on the bar.** Shift-click →A (or drop a node on deck A's
      shuttle): the deck moves to the node on the first bar line at least 2 s
      out (`CUT_LEAD_SECONDS`, room for the clip to decode — usually the next
      bar or the one after) — not at once, never off the beat, and not later
      than the seam. Note any cut that lands off the beat.
- [ ] Weave & tie A from the weave popover: the takes render and the first is
      tied to deck A without a further press.
- [ ] **Autopilot takes the first rendered child.** AUTO on for deck A, an
      untied seam with two rendered children: the status shows `auto › …`
      before the seam and the deck takes the first rendered child. AUTO off
      (the default): the same seam holds.
- [ ] **Late render holds, then goes.** Tie a child that is still rendering
      (a LoRA weave) so the seam arrives first: the tag turns amber with how
      late it is, the deck holds, and when the render lands the deck goes at
      the next loop boundary.
- [ ] Both decks: deck B on another branch, crossfade between them on the
      FLX4 crossfader; each deck seams on its own.
- [ ] HOT CUE pad sets a cue on deck A's node; after the next seam the pad
      LEDs clear (no cue carried over from the previous node).

## Loops 4/8/16 and the FLX4 loop buttons in Loom theme

- [ ] Deck panel 4 / 8 / 16: the loop starts on the current bar, on the
      nominal grid, drawn as a bracketed region on the node with `⟲ n · ×k` in
      the status line. Pressing it again carries on from the playhead.
- [ ] A loop wins over a tie: with a child tied and a loop running, the deck
      keeps looping; released, it seams at the node's end.
- [ ] FLX4 **4 BEAT/EXIT** sets the loom loop (as the panel's 4) and a second
      press releases it. It works on a node whose detected grid is missing or
      wrong (the loom loop uses the nominal grid).
- [ ] FLX4 **CUE/LOOP CALL ◄ / ►** with a loom loop running: either the loop
      halves or doubles within 4 / 8 / 16 bars and the panel follows, or the
      buttons do nothing. Never a region off the bar grid, and the next seam
      still happens. Write down which.
- [ ] FLX4 **LOOP IN / LOOP OUT** in Loom theme: write down what they do.
      After releasing the loop the deck is back on its plan (hold or tie);
      it is never left silent.

## Trim

- [ ] Drag a node's left edge: it snaps to bars, the trimmed intro is hidden
      (not dimmed), the node gets shorter and the tree closes up behind it.
- [ ] A node cannot be trimmed below 4 bars.
- [ ] Trim the right edge of the node deck A is playing: the countdown moves
      and the seam happens at the new out-point.
- [ ] →A on a node with a trimmed in-point: the deck enters at the in-point,
      on the bar.

## Cue in phones

- [ ] ⌾ on a node plays it in the phones from the join (four bars before the
      node); the master meter and the speakers do not change.
- [ ] Click inside a waveform that no deck is playing: the cue starts from
      that bar and a lime marker shows where.
- [ ] Esc, or ⌾ again, stops the cue; cueing another node replaces it.
- [ ] HEADPHONES MIX blends cue and master, and the phones CUE buttons PFL
      the decks, exactly as in DJ theme.

## Render ahead

- [ ] Settings → Render ahead is on by default. When deck A lands on a leaf
      with no children, one auto child (same prompt, 16 bars) appears as a
      ghost behind it.
- [ ] On a node with a LoRA stack the deck tag says `auto skipped · LoRA` and
      nothing is woven.
- [ ] Render ahead off: no auto child appears.
- [ ] Render ahead and AUTO both on: deck A rides three seams in a row without
      a hold and without a touch.

## Autosave & reopen after restart

- [ ] After any change the saved mark goes saving → saved. There is no save
      button.
- [ ] Rename the loom in the top bar (Enter): the loom menu lists it under the
      new name.
- [ ] Loom menu → New, Open…, Duplicate, Delete (with a confirm step) each do
      what they say. A deleted loom's songs are still in the explorer.
- [ ] Explorer → Open songs folder: `looms/<id>.json` is there for each loom.
- [ ] Quit (⌘Q) with both decks playing and relaunch: the newest loom reopens
      with every node, take and trim; both decks are stopped; →A on any node
      and PLAY resumes the set.

## FLX4 in Loom theme

- [ ] PLAY/PAUSE and transport CUE act on the deck's current node as in DJ
      theme; after a transport CUE, play again: the next seam still happens.
- [ ] Channel faders, crossfader, TRIM, EQ HI / MID / LOW, SMART CFX with the
      PAD FX pads, phones CUE and HEADPHONES MIX all reach the mixer bar, and
      the on-screen controls follow.
- [ ] Jog: playing = nudge, paused = fine seek, SHIFT + jog = fast scrub.
      After any seek the next seam still lands on the bar; a seek past the
      seam hands over to the tied node at once.
- [ ] BEAT SYNC is unmapped on the FLX4 ([`midi-ddj-flx4.md`](midi-ddj-flx4.md)):
      SYNC on the deck panel locks the decks, and they stay locked across a
      seam.
- [ ] LEDs: phones CUE echoes PFL, transport CUE as in DJ theme, PAD FX shows
      the active effect, HOT CUE pads show the current node's cues, the
      pad-mode selector lights the active bank. Nothing stays lit wrongly
      after a seam or a theme switch.
- [ ] BEAT FX ON/OFF toggles REC, as in DJ theme.
- [ ] Browse rotary + LOAD on a song: in Loom theme it lands on the loom as a
      root tied to that deck.

## DJ theme unchanged

- [ ] Theme → DJ. Re-run the deck sections of
      [`m23-hardware-checklist.md`](m23-hardware-checklist.md) (4 BEAT/EXIT,
      ◄ / ► halve and double, the gridless refusal),
      [`show-prep-checklist.md`](show-prep-checklist.md) (deck layout,
      grabbable waveforms, the jog) and the Set flow section of
      [`media-curation-checklist.md`](media-curation-checklist.md): all hold.
- [ ] Play/cue, hot cues, loops, jog and LEDs on a playback deck behave as
      before; a realtime (Magenta) deck loads and plays.
- [ ] 4 BEAT/EXIT in DJ theme makes a grid beat loop, not a loom loop.

## Visuals toggles

- [ ] Loom theme, Performance visuals on: the aurora breathes under the
      playheads at the master tempo, nodes are tinted by key, and the
      Lissajous scope sits beside the master controls.
- [ ] Performance visuals off: all three are gone and the loom still works.
- [ ] DJ theme with visuals on looks as before; there is no scope.

## Failure drills

- [ ] Mid-render with deck A playing, kill the render
      (`pkill -f sa3_mlx`): deck A keeps playing and holds where it would
      have; the node shows failed with the reason, and retry / discard.
- [ ] Mid-render, kill the generation server (`pkill -f lsdj.controller` in
      dev, `pkill -f generation-server` in a release build): same result.
      Retry after the server is back (relaunch if needed): the node renders.
- [ ] Delete a node's song file by hand in Finder, relaunch: the node is
      greyed as missing and cannot be tied; the rest of the loom plays.
- [ ] Quit while a weave is rendering, relaunch: the node comes back failed
      (with retry) or renders again — never a ghost stuck rendering.

## Prepared loom (before a show)

- [ ] Weave a spine of 6–10 nodes including at least one LoRA thread (render
      ahead does not cover LoRA, so pre-render it), name the loom, quit.
- [ ] Relaunch: the loom reopens (or Loom menu → Open…) with every node ready,
      LoRA nodes included; →A on the first node and PLAY starts the set.

## Recorded set and the silence metric

- [ ] REC on before the first PLAY, off after the last note: one 48 kHz WAV in
      the recordings folder.
- [ ] Run `scripts/silence_gaps.py <wav>` (defaults `--threshold -60
      --min-ms 150`). Every span it prints is either an intentional silence
      (a planned break, the end) or a defect. Target: zero defects.
- [ ] **Rehearsal, 30 minutes:** zero defects on the metric, zero crashes,
      zero webview reloads, no "the deck ended" moment; the rehearsal log
      below filled in.
- [ ] **The show, 45 minutes**, from the prepared loom: at least 12 nodes
      woven live, 4 take picks, 2 returns and 1 cut; both decks on air with
      crossfades; the mixer driven from the FLX4 throughout. Zero defects on
      the metric. Afterwards the loom is on disk with everything woven and
      the recording is in the recordings folder.

## Measurements

Measured on the show machine; write the number in the last column.

| # | What | How | Target | Measured |
| - | ---- | --- | ------ | -------- |
| P1 | 16-bar plain weave at 124 BPM, both decks playing | press to ready, stopwatch; note the ETA shown at the press | ≤ 15 s; ETA within ±30 % | |
| P2 | Tied child ready ≥ 2 s before the seam | listen at the seam | on the beat, within one 250 ms poll | |
| P3 | Peak memory during a render, both decks playing | Activity Monitor, the app plus its sidecars | fits with headroom | |
| P4 | Webview frame rate with 40 nodes on screen | React profiler or an FPS meter | ≥ 50 fps | |

## Rehearsal log

One row per seam, hold, cut, loop or gap worth noting.

| Time | Deck | Event | From → to | Heard | Note |
| ---- | ---- | ----- | --------- | ----- | ---- |
| | | | | | |

When every box ticks, flip M26's status in [`ROADMAP.md`](ROADMAP.md) and
ADR-0038 to Accepted.
