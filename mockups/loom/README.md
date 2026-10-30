# Loom mockup

A static, dependency-free mockup of the **looming** UI for LSDJ: an alternative
front end on the same Rust engine and Stable Audio 3 generation. Nothing here
makes sound. Open `index.html` in a browser (or
`python3 -m http.server --directory mockups/loom`; the `loom-mockup` entry in
`.claude/launch.json` does the same).

## The idea

A DJ set is a path through a space of possible music. The loom shows that
space: left to right is musical time in bars, down is alternatives. Each pill
is a **thread** — one rendered clip, continued from the tail of its parent
(SA3 *continue* from ADR-0037, the continuation conditioning of ADR-0034).
You audition branches in the headphones, tie a deck through the one you want,
and the deck's playhead — the **shuttle** — follows the tied thread.

## Vocabulary

| Word | Meaning |
| ---- | ------- |
| loom | The canvas. x = bars, y = branches. Several roots can coexist (a forest). Zoom like a DAW: − / + / fit (keys − = Z), ⌘-wheel anchored at the cursor, or drag the ruler up and down (sideways scrolls). No scale readout; the ruler's bar numbers are the scale. |
| thread | A node: one clip (prompt, key, bars, seed, LoRA stack), rendered from its parent's last bars. A root is seeded from a prompt or a library song. The waveform runs the full length of the clip (peaks from the decoded audio in the app; the mockup synthesises an intro / build / drop / break envelope). |
| trim | Drag a node's left or right edge to skip an intro or end at the drop, snapping to bars. DAW-style: the trimmed audio is hidden, the node gets shorter, the tree closes up behind it. The shuttle enters at the in-point and seams at the out-point. |
| weave | "What next?" after any node: a prompt (or none = continue), how many **takes**, how many bars, a LoRA stack, and the SA3 knobs behind a disclosure. Renders shimmer as ghosts with a progress bar until the audio exists. |
| take | One of N sibling renders of the same prompt with different seeds, drawn in a bracket. Cue each from the join, then ✓ **keep** one; the others are discarded (⨯ discards a single take). A deck can't lose the take it is on or has tied. |
| shuttle | A deck's playhead inside the loom. It follows the threads you tied; where none is tied it **holds** the last four bars (amber slot, counting tag). With **auto** on for that deck it follows the first rendered child instead. It never goes silent. |
| →A / →B | Tie a node as the deck's *next*: through the tree if it is downstream, a jump or a return at the seam if not. Ties queue; ⨯ (the same button, or the rail chip) unties. Shift-click, or drop the node on the shuttle, **cuts** to it on the next bar. |
| cue | ⌾ auditions a node in the headphones, by default from the *join* (four bars before it) so you hear the transition. Click inside a waveform to cue from that bar instead (a lime marker shows where). Independent of the on-air decks. |
| scrub | Clicking inside the waveform of the node a deck is playing moves that deck's playhead to that bar (on the beat, in the real thing). |
| panel | Each deck's strip in the mixer: what's playing, bars to the seam and what's there, then transport, fader with meter, EQ and FX. Drop a node on it to tie. |
| bounce | Drop a node on the explorer: its root-to-node path is stitched to one song in the library. ⤓ on a deck panel bounces what that deck actually played. |
| loop | 4 / 8 / 16 on the deck loops from the current bar, quantised (the FLX4's auto-loop); press again to carry on from the playhead. Drawn as a bracketed region on the node with the trail cycling inside and `⟲ 8 · ×3` in the deck status. A hold at an untied seam is the same region in amber. A tie back up the tree is a loop at song scale. |
| render ahead | A setting, on by default: every on-air leaf grows one *auto* continuation (same prompt and stack) as soon as a deck lands on it, so a hold is a choice, not an accident. Ghosts show a live ETA (`⟳ 0:06`, `queued`, `⟐ merging`); a tied ghost that will miss the seam turns the shuttle tag amber with how late it is. |
| save | There is none. The loom autosaves under the name in the top bar (click to rename; a quiet "saved" mark). The explorer's **looms** tab lists looms to open, duplicate or delete; deleting a loom keeps its renders in songs. A *set* is what left the master: ● rec. |
| theme | A new setting in the settings drawer: **DJ** (the two deck columns the app has today) or **Loom** (this). It switches the deck surface only; the mixer, explorer, engine, generation and FLX4 mapping are shared. |

## What it keeps from the current app

The mixer (trim, EQ, Color FX, volume with a meter beside the fader, crossfader,
cue mix, master, record), kept to one strip per deck, the media explorer as a drawer (now with star
ratings, tags, sorts, a ★4+ filter and an *in key with deck A/B* filter), and
the FLX4 mapping. Only the deck surface changes: the loom replaces the two deck
columns.

## Settings drawer

The settings button opens a right-hand drawer with the sections the app has
today — Appearance, Audio (output, headphones, tempo range), Models, Recording,
AI co-DJ (MCP) — plus two additions the loom needs: the **Theme** switch at the
top of Appearance, and the Stable Audio 3 model and LoRA adapters listed under
Models next to the Magenta RT models.

## Visual language (the microdose)

Everything decorative is tied to something musical, and it stays behind the
work: an **aurora** of blurred colour that breathes once a bar at the master
tempo and glows under each deck's playhead; node colour by **key** on the
circle of fifths, waveforms with peaks in that hue over a white-hot body;
deck badges and playheads that **pulse on their own deck's beat**, so two
unsynced decks visibly disagree and SYNC visibly locks them; a small
**Lissajous scope** in its own window beside the master controls, drawn from
the two decks' levels in their colours; a slow sap of light along every thread; a **ripple** from a node
when you tie, cut or keep; a conic ring around whatever is cued; crawling
grain; two slow counter-rotating spirals. Settings → Visuals: **off /
microdose / full** (full adds a sheen sweep over nodes and brighter layers).
Microdose is the default.

## Generation settings exposed (what the backend already takes)

The weave popover maps 1:1 onto `/api/generate` for `kind: track`:
prompt, `seconds` (from bars at the root's tempo), `loras` (up to 4 adapters,
each with a strength; the real adapters on disk are listed), `cfg` (1.1–4,
default 3), `apg` (0–1, default 1), `init_noise_level` shown as **drift** from
the parent's tail (0.01–5, default 0.6), `negative_prompt`, and `seed` (one per
take: a chosen seed plus the take index, or random). Steps are fixed by the
sidecar. Nothing in the popover needs a new endpoint.

## Decisions so far

- **2026-10-30, shuttle rules** — below.
- **2026-10-30, Stable Audio 3 only, for now.** Realtime (Magenta RT) decks are
  a different instrument: an endless thread with no seam. The first loom is
  playback-only; live threads return as a later layer once the playback loom
  is right.
- **2026-10-30, no chord row.** We don't have chord data; the node shows a
  waveform, key and length instead.
- **2026-10-30, render time is shown, not hidden.** Measured on this Mac: SA3
  medium composes 2:00 in 14.9 s (≈0.125× realtime, ROADMAP M19), so a plain
  16-bar clip renders in ≈7 s and the shuttle is rarely the bottleneck. The
  expensive thing is a **LoRA stack: ≈128 s flat per render** for the adapter
  merge (docs/mcp-findings.md #16). The mockup models exactly that (`RENDER`),
  queues takes one after another, and the weave popover compares render time
  with the time until the deck reaches the seam. **Implementation
  prerequisite:** the sidecar must keep the merged DiT warm per stack, or
  LoRA threads cannot be woven live.
- **2026-10-30, autosave, no save button.** Every render is already a file in
  the library; the loom is a small graph over them, so it saves itself. Naming
  and listing (the looms tab) are the whole UI. ● rec stays the way to save a
  *set*.
- **2026-10-30, one loop visual.** Beat loops, the hold at a seam and return
  ties are three mechanisms the DJ experiences as one idea, so the node shows
  them the same way. Beat loops win over ties: a tied next waits until the
  loop is released.
- **2026-10-30, Loom is a theme, not a fork.** The loom ships behind a
  `Theme: DJ | Loom` setting rather than replacing the deck UI outright, so
  both surfaces share one engine, mixer, explorer and MIDI layer and either
  can be dropped later without a migration.
- **2026-10-30, no rail.** The mixer panel had a row of past · now · next
  pills per deck. The loom already draws all three (trail, glowing node, dashed
  tied node), so the panel keeps one line of text instead and the mixer gives
  the height back to the loom.
- **2026-10-30, trim hides, it does not dim.** Left-to-right is real time, so a
  trimmed intro cannot stay on screen without lying about when the next node
  starts. Nodes shrink like DAW clips and the handles stay reachable.
- **2026-10-30, takes, not flavours.** "Give me three of this prompt, I'll pick"
  beats three auto-varied prompts. Variation in *prompt* is the user's job (the
  chips help); variation in *seed* is the model's.
- **2026-10-30, a node is a song.** Every render is saved to the songs library,
  so nodes are rated, tagged and analysed like any song and a loom is a small
  graph over song files. Discarding a take removes it from the loom and leaves
  the file in songs: the library is the trash, and there is no undo UI in v1.
- **2026-10-30, a nominal grid, clamped to the clip.** A loom has one tempo per
  root (124 BPM unless a dropped song brings its own) and a bar is `240 / bpm`
  seconds from the node's own bar 0; the detected grid never moves a seam.
  Every derived end is clamped to the clip's measured duration and a dropped
  song's bars are floored, so a nominal end never runs past the audio. A
  render that drifts from its tempo seams off the beat; trim fixes it.
- **2026-10-30, no drift in v1.** `init_noise_level` is remix mode and its
  interaction with continue mode is unmeasured, so continuations do not offer
  it. It comes back as its own follow-up.
- **2026-10-30, render ahead skips LoRA nodes.** A ≈2 min auto render is not a
  safety net, so behind a node with a LoRA stack render ahead weaves nothing
  and the deck tag says `auto skipped (LoRA)`. The warm merge is a later
  phase.
- **2026-10-30, looms are files beside the songs.** Each loom is a JSON file
  in `generated_songs/looms/`, written by the shell; the webview autosaves it
  and validates it on read. Deck state is never saved.
- **2026-10-30, a bar menu, not an explorer tab.** The loom's name, a saved
  mark and a menu (new, open, duplicate, delete) sit in the top bar. The
  explorer "looms" tab is deferred; the menu is the lazy working version.
- **2026-10-30, needle drops are immediate.** A click on a playing node's
  waveform seeks at once; quantised drops are a follow-up. A trim on the node
  a deck is playing takes effect at the seam: the out-point moves the seam,
  the in-point only matters on entry.
- **2026-10-30, bounce joins with a 5 ms crossfade.** Bounce stitches a deck's
  played path, or a node's path from its root, into one new song. Each join is
  a 5 ms crossfade over the last 5 ms of the outgoing part and the incoming
  clip's 5 ms just before its start, the gains summing to one; a part that
  starts at its clip's first frame has nothing before it and joins hard.
- **2026-10-30, key hue from the Camelot number.** Each Camelot number is a 30°
  step on the circle of fifths, and a relative pair shares a hue: 8A/8B
  (A minor / C major) sits at 0°, as in the mockup's `hue()`.

## Shuttle rules (settled 2026-10-30)

1. **A tie is a promise, a hold is the default.** The shuttle plays what you
   tied. With nothing tied it loops the last four bars of the node,
   beat-aligned, until you tie something. It never picks a branch for you.
2. **→A means next, never now.** Downstream nodes are reached through the
   tree; anything else is a jump at the end of the current node. Cut-now is a
   separate gesture (shift + →A, or dropping the node on the shuttle),
   quantised to the bar. On a stopped deck →A loads the node at the top.
3. **Ties are a queue and may go backwards.** Re-tying an upstream node is a
   return (ABAB is four ties); the loom draws a return arc.
4. **The countdown is first-class.** The shuttle tag and the rail show bars to
   the seam and what is at it (`12 → breakbeat`, `12 → HOLD`). HOLD turns
   amber at eight bars and blinks the deck's play button (FLX4 play LED).
5. **Seams are quantised.** Tie and untie take effect at the node end, or at
   the next loop boundary while holding. Arriving at a tied node that is still
   rendering holds on the current tail until it is ready, then goes.
6. **Weave can tie.** "Weave & tie A/B" renders the takes and ties the first at
   once, so the on-air deck keeps going in two presses; audition-first stays
   the default for the incoming deck.
7. **Autopilot is an explicit per-deck toggle, off by default.** When on, an
   untied seam takes the first *rendered* child instead of holding. The rail
   says `auto › …` so the choice is visible before it happens.

## Open questions

- **Tempo across a loom.** Threads inherit the root's tempo; crossing roots is a
  varispeed sync like today. Should a weave be allowed to change tempo
  (halftime / double time) and what does the ruler show then?
- **What is a fork, musically?** Children start where the parent ends. Do we
  also want branches that *overlap* (start 8 bars before the parent ends, for
  a blended transition rendered by the model)?
- **Live decks later.** When Magenta RT returns, is a live thread a node with
  no end (the earlier mockup) or a separate deck type beside the loom?
- **Hardware.** FLX4 jog = scrub the loom? Pads = the current node's takes or
  children (pad 1–4 = tie 1–4)? Browse knob = move the selection along threads?
  Loop in / out / auto-loop map straight onto the 4 / 8 / 16 control.
