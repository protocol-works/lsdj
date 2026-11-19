# Loom checklist — the branching playback surface on the FLX4

Manual verification of M26 ([ADR-0038](adr/0038-the-loom-a-branching-playback-surface-as-a-theme.md))
with the device and ears. The loom reducer, nominal-grid timing, layout,
scheduler and render queue are unit-tested, and a Rust test proves the engine
swap sample-continuous; this checklist covers what they cannot: the real
window, the FLX4, the phones, real render times, and a recorded set. The
expected behaviour is the shuttle rules in
[`mockups/loom/README.md`](../mockups/loom/README.md).

Where things are in Loom theme: the **loom bar** (name, `Saved`, the ⋯ Loom
menu, the status line) sits in the top bar right of the LSDJ logo; the
**canvas** fills the booth, with `+ thread`, `−`, `Fit` and `+` at its top
right; the **mixer bar** runs along the bottom — deck A's strip on the left,
the crossfader, scope, beat phase, Cue mix and master meter in the middle,
deck B's strip on the right. A node's tools (`✓ keep`, `⨯`, `⌾ cue`, `→ A`,
`→ B`, `✱`, `⤓`) sit on the node; a deck's tag (`A · 12 → HOLD`) rides above
its playhead on the canvas.

## Setup

- [ ] `just tauri-dev` (or a `just tauri-build` app), FLX4 connected (green
      LED), phones on the FLX4 jack, master to speakers or an interface.
- [ ] Settings → Model library: the Stable Audio 3 model and at least one
      LoRA adapter installed. Settings → Appearance: `Performance visuals` on.
- [ ] Settings → Recording: note the recordings folder.

## Theme switch

- [ ] Settings → Appearance → `Theme` offers `DJ | Loom`, with `DJ` selected
      on a fresh profile; the booth is today's two deck columns.
- [ ] Select `Loom`: the deck columns give way to the canvas with the mixer
      bar below it. Statusbar, explorer and settings drawer are unchanged; the
      top bar gains the loom bar (name field, `Saved`, ⋯ Loom menu), and
      Settings → Appearance gains the `Render ahead` switch.
- [ ] Switch theme while a track plays on deck A: the audio does not stop,
      click or reload.
- [ ] In Loom, tie a ready child to deck A (`→ A`), then Theme → `DJ` before
      the seam: at the seam deck A does **not** swap into the child; it loops
      the node's last four bars (the hold stays in the engine) until you exit
      the loop on the DJ deck.
- [ ] Quit and relaunch: `Loom` is still selected. Back to `DJ`, relaunch:
      `DJ`.

## Weave & takes

- [ ] On an empty loom (the canvas reads "Weave a thread to begin…"), press
      `+ thread`: the popover "Weave a new thread" opens. Prompt, `Takes` 3,
      `Bars` 16, `Tempo (BPM)` 124, no LoRA, `Weave ×3`. Three ghosts appear in
      one bracket labelled `3 takes · 0/3 rendered` and render one after
      another, each ghost reading `queued · m:ss`, then `⟳ m:ss` with a
      progress bar.
- [ ] Each finished take shows its waveform under the bar ruler and its meta
      line (`16b · take 2`) picks up a key (`8A`) and a key tint once analysis
      lands; the bracket reads `3 takes · ⌾ cue each · ✓ keep one`. Each take
      is in the explorer's Songs tab, titled from the start of the prompt
      with ` · take k`.
- [ ] Weave with `SA3 settings` open (cfg, apg, a negative prompt, a fixed
      seed): the render completes; ⋯ → `Reuse settings` on that song in the
      explorer's Songs tab shows the same values.
- [ ] Weave a root with one LoRA adapter (the popover's adapter list): the
      popover warns "⟐ The adapter merge alone takes 2:08, every render"; the
      ghost reads `⟐ merging · m:ss`, then `⟳ m:ss`; it completes and its meta
      shows `⟐`.
- [ ] Continue: `✱` (Weave what comes next) on a ready node, or the
      `+ What next?` slot right of a leaf, opens "Weave after “…”"; leave
      `Prompt` empty ("Empty = continue in the same style") and weave. The
      child starts at the parent's end on the canvas. `⌾ cue` it (its label
      reads `⌾ cue · from the join (−4 bars)`): the parent's last bars run
      into the child with no jump in tempo or key.
- [ ] `✓ keep` on one take: the other takes leave the loom and are still in
      Songs; the bracket is gone.
- [ ] `⨯` (Discard) on one take of a group: it leaves the loom; the file stays
      in Songs.
- [ ] In an open take group, tie one take to deck A (`→ A`), `✱` a child from
      another, then `⨯` each of those two: each is refused with "A node a deck
      is on or has tied, or one with children, stays" in the loom bar;
      nothing changes. `✓ keep` on a third take: the two protected takes stay
      as ordinary nodes.
- [ ] Drag a song from the explorer's Songs tab onto the canvas: it becomes a
      root whose bar count (`NNb`) fits inside the song (never past its end).

## Shuttle & seams

- [ ] `→ A` on a ready node while deck A is stopped: the deck loads it at its
      in-point. Deck A's status row reads `A`, the node's prompt,
      `16 → HOLD` and `bar 1 / 16`.
- [ ] PLAY/PAUSE on the FLX4: the deck plays and the playhead moves along the
      node. The canvas tag reads `A · 12 → HOLD` and turns amber at 8 bars;
      the leaf's slot then reads `+ A is holding — what next?`.
- [ ] **Hold is never silent.** Reach an untied seam: the deck loops the
      node's last four bars on the beat (the node shows `⟲ hold · last 4`),
      heard as a loop, not a gap, until something is tied.
- [ ] While holding, `→ A` on a ready child: the hand-over happens at the next
      loop boundary, on the bar, with no click and no silence.
- [ ] **Tied child hands over with no gap or click.** `→ A` on a ready child
      before the seam (tag `A · 8 → <child>`): at the seam the deck hands over
      on the bar with no gap, click or doubled beat; deck A's status row names
      the child; the BPM readout and zoom bands follow within a quarter
      second.
- [ ] Two ties queued (a child, then its child) play in order. `⨯ A` on the
      second unties it: the deck holds at the end of the first.
- [ ] Return: `→ A` on a node back up the tree. At the seam the deck jumps
      back on the bar; the loom draws a return arc.
- [ ] **Cut lands on the bar.** Shift-click `→ A` (or drag a node onto deck
      A's tag above the playhead, "Drop a node here to cut to it on the next
      bar"): the deck moves to the node on the first bar line at least 2 s
      out (`CUT_LEAD_SECONDS`, room for the clip to decode — usually the next
      bar or the one after) — not at once, never off the beat, and not later
      than the seam. Note any cut that lands off the beat.
- [ ] In the popover opened from `✱` on the node deck A is on, `Weave & tie A`:
      the takes render and the first is tied to deck A without a further
      press. While deck A plays, the popover line reads "A reaches this seam
      in m:ss → in time ✓" or "… → late by m:ss, A will hold".
- [ ] **Autopilot takes the first rendered child.** `Auto` on deck A's strip
      lit, an untied seam with two rendered children: the tag reads
      `A · 8 → auto · <first child>` before the seam and the deck takes that
      child. `Auto` off (the default): the same seam holds.
- [ ] **Late render holds, then goes.** Tie a child that is still rendering
      (a LoRA weave) so the seam arrives first: the tag turns amber and reads
      `A · 3 → ⟳ late m:ss · <child>`, deck A's status row `3 → N s late`; the
      deck holds, and when the render lands the deck goes at the next loop
      boundary.
- [ ] Both decks: deck B on another branch, crossfade between them on the
      FLX4 crossfader; each deck seams on its own.
- [ ] HOT CUE pad 1 sets a cue on deck A's node; after the next seam the pad
      LEDs clear (no cue carried over from the previous node).

## Loops 4/8/16 and the FLX4 loop buttons in Loom theme

- [ ] Deck A's strip, `Loop` 4 / 8 / 16: the loop starts on the current bar,
      on the nominal grid, drawn as a bracketed region labelled `⟲ 8` on the
      node, and the tag reads `A · ⟲ 8`. Pressing another length while it runs
      keeps the start bar and changes the length; pressing the lit length
      releases it and the deck is back on its plan.
- [ ] Tab to deck A's `Loop` control with nothing lit: focus lands on `4`
      (arrow keys move it).
- [ ] A loop wins over a tie: with a child tied and a loop running, the deck
      keeps looping; released, it seams at the node's end.
- [ ] FLX4 **4 BEAT/EXIT** sets a four-bar loom loop (the strip's `4` lights)
      and a second press releases it. It works on a node whose detected grid
      is missing or wrong (the loom loop uses the nominal grid).
- [ ] FLX4 **CUE/LOOP CALL ◄ / ►** with a loom loop running: the loop halves
      or doubles within 4 → 8 → 16 bars (clamped at both ends), keeping its
      start bar, and the strip's `Loop` follows. With no loom loop running
      they do nothing. Never a region off the bar grid, and the next seam
      still happens.
- [ ] FLX4 **LOOP IN / LOOP OUT** in Loom theme: they set an engine loop as
      in DJ theme, drawn on the node as `⟲ n` and tagged `A · ⟲ n`; the loom
      places nothing while it runs (a tie waits). 4 BEAT/EXIT releases it,
      and the deck is back on its plan (hold or tie), never left silent.

## Trim

- [ ] Drag a node's left edge ("Drag to trim the start (snaps to bars)"): it
      snaps to bars, the trimmed intro is hidden (not dimmed), the meta reads
      `12/16b`, and the tree closes up behind it.
- [ ] A node cannot be trimmed below 4 bars.
- [ ] Drag the right edge ("Drag to trim the end (snaps to bars)") of the
      node deck A is playing: the tag's countdown moves and the seam happens
      at the new out-point.
- [ ] A node with children has no right-edge handle; its left edge still
      trims.
- [ ] `→ A` on a node with a trimmed in-point: the deck enters at the
      in-point, on the bar.

## Cue in phones

- [ ] `⌾ cue` on a node plays it in the phones from the join (four bars before
      the node; the label reads `⌾ cue · from the join (−4 bars)`, or
      `⌾ cue · from the top` on an untrimmed root); the master meter and the
      speakers do not change.
- [ ] Click inside a waveform that no deck is playing: the cue starts from
      that bar, a marker shows where, and the label reads
      `⌾ cue · from bar n`. Clicking inside the waveform deck A is playing
      moves deck A's playhead there at once instead.
- [ ] Esc, or `⌾ cue` again, stops the cue; cueing another node replaces it.
- [ ] HEADPHONES MIX blends cue and master, and the phones CUE buttons PFL
      the decks (the strip's `Cue` lights), exactly as in DJ theme.

## Render ahead

- [ ] Settings → Appearance → `Render ahead` is on by default. When a playing
      deck A lands on a leaf with no children, one auto child (same prompt,
      16 bars) appears behind it as a ghost reading `auto · queued · m:ss`.
- [ ] On a node with a LoRA stack the tag reads
      `A · n → HOLD · auto skipped (LoRA)` and nothing is woven.
- [ ] `Render ahead` off: no auto child appears.
- [ ] `Render ahead` on and `Auto` lit: deck A rides three seams in a row
      without a hold and without a touch.

## Bounce

- [ ] Let deck A play across at least one seam, then `⤓` ("Bounce what this
      deck played to songs") at the end of deck A's status row: the loom bar
      reads `Bouncing “<loom> · <node>”…`, then
      `Bounced to songs: “<loom> · <node>”`, and the song is in the explorer's
      Songs tab. It is each node's trimmed body in the order played, up to
      the playhead (loops and cuts are not in it). Play it: no gap or click
      at any join.
- [ ] `⤓` ("Bounce the path from its root to here to songs") on a node's
      tools: one song from the root through that node.
- [ ] A second `⤓` while `Bouncing…` shows does nothing: one new song, not
      two.
- [ ] `⤓` is absent on a deck that has played nothing.

## Autosave & reopen after restart

- [ ] After any change the loom bar's mark reads `Saving…`, then `Saved`.
      There is no save button.
- [ ] Rename the loom in the loom bar's name field (Enter): ⋯ Loom menu →
      `Open…` from another loom lists it under the new name.
- [ ] ⋯ Loom menu → `New loom`, `Open…` ("name · N nodes"), `Duplicate`
      ("name copy") and `Delete…` (confirm "Delete “name”? Its songs stay in
      the library." → `Delete`) each do what they say. A deleted loom's songs
      are still in the explorer.
- [ ] Explorer → Songs tab → `Open songs folder`: `looms/<id>.json` is there
      for each loom.
- [ ] Quit (⌘Q) with both decks playing and relaunch: the newest loom reopens
      with every node, take and trim; both decks are stopped; `→ A` on any
      node and PLAY resumes the set.

## FLX4 in Loom theme

- [ ] PLAY/PAUSE and transport CUE act on the deck's current node as in DJ
      theme; after a transport CUE, play again: the next seam still happens.
- [ ] Channel faders, crossfader, TRIM, EQ HI / MID / LOW, SMART CFX with the
      PAD FX pads, phones CUE and HEADPHONES MIX all reach the mixer bar, and
      the on-screen controls (the strip's fader, `Trim`, `EQ Hi` / `EQ Mid` /
      `EQ Low`, `Effect` and `FX amount`, `Cue`; the middle's `Crossfade` and
      `Cue mix`) follow.
- [ ] Jog: playing = nudge, paused = fine seek, SHIFT + jog = fast scrub.
      After any seek the next seam still lands on the bar; a seek past the
      seam hands over to the tied node at once.
- [ ] BEAT SYNC is unmapped on the FLX4 ([`midi-ddj-flx4.md`](midi-ddj-flx4.md)):
      `Sync` on deck B's strip locks deck B to deck A, and they stay locked
      across a seam.
- [ ] LEDs: phones CUE echoes PFL, transport CUE as in DJ theme, PAD FX shows
      the active effect, HOT CUE pads show the current node's cues, the
      pad-mode selector lights the active bank. Nothing stays lit wrongly
      after a seam or a theme switch. The play LED does not blink at HOLD
      (not in v1; HOLD warns on screen only).
- [ ] BEAT FX ON/OFF toggles REC, as in DJ theme.
- [ ] Browse rotary + LOAD on a song: in Loom theme it lands on the loom as a
      root tied to that deck.

## DJ theme unchanged

- [ ] Theme → `DJ`. Re-run the deck sections of
      [`m23-hardware-checklist.md`](m23-hardware-checklist.md) (4 BEAT/EXIT,
      ◄ / ► halve and double, the gridless refusal),
      [`show-prep-checklist.md`](show-prep-checklist.md) (deck layout,
      grabbable waveforms, the jog) and the Set flow section of
      [`media-curation-checklist.md`](media-curation-checklist.md): all hold.
- [ ] Play/cue, hot cues, loops, jog and LEDs on a playback deck behave as
      before; a realtime (Magenta) deck loads and plays.
- [ ] 4 BEAT/EXIT in DJ theme makes a grid beat loop, not a loom loop.
- [ ] The shared segmented controls (Settings → Appearance → `Theme`; the
      explorer Songs tab's `Generation mode` Basic | Advanced and a source's
      Continue | Replace | Remix) still tab to the selected option and move
      with the arrow keys.

## Visuals toggles

- [ ] Loom theme, `Performance visuals` on: the aurora breathes under the
      playheads at the master tempo, nodes are tinted by key (8A and 8B share
      a hue), and the Lissajous scope sits in the mixer bar's middle, above
      the beat-phase meter.
- [ ] Two unsynced decks playing: each deck badge, playhead and the dashed
      border of the node it is tied to pulse on that deck's own beat, so the
      two visibly disagree; `Sync` locks them together.
- [ ] Tie a node, and keep a take: the node ripples once (deck colour for a
      tie, cue green for a keep). A slow light runs along every rendered
      thread; a deck's tied path flows toward the next node.
- [ ] The scope draws one figure per sounding deck in its colour and grows
      and shrinks with the channel faders and the crossfader.
- [ ] `Performance visuals` off: the aurora, pulses, light, ripples and scope
      are gone (key tints stay) and the loom still works. With visuals on and
      macOS Accessibility → Display → Reduce motion on, nothing in the loom
      moves: no pulses, light, ripples or scope.
- [ ] DJ theme with visuals on looks as before; there is no scope.

## Failure drills

- [ ] Mid-render with deck A playing, kill the render
      (`pkill -f sa3_mlx`): deck A keeps playing and holds where it would
      have; the node reads `failed · <reason>` with `⟳ retry` and `⨯`.
- [ ] Mid-render, kill the generation server (`pkill -f lsdj.controller` in
      dev, `pkill -f generation-server` in a release build): same result.
      `⟳ retry` after the server is back (relaunch if needed): the node
      renders.
- [ ] Delete a node's song file by hand in Finder, relaunch: the node reads
      `file missing`, is greyed and offers only `⨯`; the rest of the loom
      plays.
- [ ] Quit while a weave is rendering, relaunch: the node comes back
      `failed · …` with `⟳ retry` — never a ghost stuck rendering.

## Prepared loom (before a show)

- [ ] The night before: weave a spine of 6–10 nodes including at least one
      LoRA thread (≈ 2 min per adapter, and render ahead skips LoRA, so it is
      pre-rendered here), name the loom.
- [ ] Every node is ready: no ghost, no failed or missing node on the canvas
      (retry or discard any that is), and each is in the explorer's Songs.
- [ ] Quit (⌘Q) and relaunch: the loom reopens (or ⋯ Loom menu → `Open…`)
      with every node ready, LoRA nodes included; both decks are stopped.
- [ ] Dry run, 5 minutes: REC on, `→ A` on the first node, PLAY, ride at
      least one tied seam and one hold, REC off. Then, from the repo root:
      `uv run --project backend python scripts/silence_gaps.py "<recordings
      folder>/lsdj-<time>.wav"; echo $?`. It prints one `start  end  length`
      line per gap and `N gaps, longest X ms`, and exits 1 when it found any.
      A span at `0:00.000` (REC before PLAY) or at the very end is
      intentional; any other span is a defect to fix before the show.

## Recorded set and the silence metric

- [ ] REC on before the first PLAY, off after the last note: one 48 kHz WAV in
      the recordings folder.
- [ ] Run `uv run --project backend python scripts/silence_gaps.py <wav>`
      (defaults `--threshold -60 --min-ms 150`). Every span it prints is
      either an intentional silence (a planned break, the end) or a defect.
      Target: zero defects.
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
| P3 | Peak memory during a render, both decks playing | the RSS loop below, plus Activity Monitor | fits with headroom | |
| P4 | Webview frame rate with 40 nodes on screen | the frame counter below | ≥ 50 fps | |

- **P1:** one take, 16 bars, 124 BPM, no LoRA, `SA3 settings` closed, both
  decks playing. Write down the ETA in the popover ("renders in m:ss") at the
  press, start a stopwatch on `Weave ×1`, stop it when the ghost becomes a
  waveform. ETA error is `(ETA − measured) / measured`.
- **P2:** tie a ready child (the tag is not amber) with at least 2 s to the
  seam, REC on. Listen at the seam; in the recording the hand-over sits on
  the bar, not a beat late. Note each seam in the rehearsal log.
- **P3:** in a terminal while the render runs, write down the peak it prints:

  ```sh
  sysctl vm.swapusage
  while sleep 1; do ps -axo rss=,command= | grep -E 'LSDJ|lsdj|sa3_mlx' | grep -v grep | awk '{kb += $1} END {printf "%.0f MB\n", kb / 1024}'; done
  ```

  Headroom: Activity Monitor → Memory shows the pressure green, and
  `sysctl vm.swapusage` after the render shows no more swap used than
  before.
- **P4:** a loom with 40 or more nodes on screen, both decks playing, in
  `just tauri-dev`: right-click → Inspect Element → Console, paste and wait
  ten seconds for the mean frame rate:

  ```js
  let n = 0; const t0 = performance.now(); requestAnimationFrame(function f(t) { n++; t - t0 < 10000 ? requestAnimationFrame(f) : console.log((n * 1000 / (t - t0)).toFixed(1), 'fps') })
  ```

## Rehearsal log

One row per seam, hold, cut, loop or gap worth noting.

| Time | Deck | Event | From → to | Heard | Note |
| ---- | ---- | ----- | --------- | ----- | ---- |
| | | | | | |

When every box ticks, flip M26's status in [`ROADMAP.md`](ROADMAP.md) and
ADR-0038 to Accepted.
