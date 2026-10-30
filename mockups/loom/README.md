# Loom mockup

A static, dependency-free mockup of the **looming** UI for LSDJ: an alternative
front end on the same Rust engine, Magenta RT decks, and Stable Audio 3
generation. Nothing here makes sound. Open `index.html` in a browser (or
`python3 -m http.server --directory mockups/loom`).

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
| loom | The canvas. x = bars, y = branches. Several roots can coexist (a forest). |
| thread | A node: one clip (prompt, key, chords, bars), rendered from its parent's last bars. A root is seeded from a prompt or a library song. |
| live thread | A Magenta RT stream as a node: open-ended, steered from the style pad, pulsing right edge. Freeze it to get a fixed thread to branch from. |
| weave | "What next?" after any node: render N candidate children (prompt + flavour variants). They shimmer as ghosts with a progress bar until the audio exists. |
| shuttle | A deck's playhead inside the loom. It follows the threads you tied; where none is tied it **holds** the last four bars (amber slot, counting tag). With **auto** on for that deck it follows the first rendered child instead. It never goes silent. |
| →A / →B | Tie a node as the deck's *next*: through the tree if it is downstream, a jump or a return at the seam if not. Ties queue; ⨯ (the same button, or the rail chip) unties. Shift-click, or drop the node on the shuttle, **cuts** to it on the next bar. |
| cue | ⌾ auditions a node in the headphones, by default from the *join* (four bars before it) so you hear the transition. Independent of the on-air decks. |
| rail | The deck's flattened path (past · now · next) in the mixer. Drop target. |
| bounce | Drop a node on the explorer: its root-to-node path is stitched to one song in the library. |

## What it keeps from the current app

The mixer (trim, EQ, Color FX, volume, crossfader, cue mix, master, record),
the media explorer as a drawer, and the FLX4 mapping. Only the deck surface
changes: the loom replaces the two deck columns.

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
6. **Weave can tie.** "Weave & tie A/B" renders one continuation and ties it at
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
- **Render-ahead budget.** SA3 takes minutes per clip; the shuttle moves in
  real time. Weaving must run well ahead of the playhead. Live threads cover
  the gap. Is that enough, or does the deck need an automatic "safe
  continuation" that renders in the background for every on-air node?
- **How many candidates per weave?** Three is the mockup default. Each is a
  full render.
- **Hardware.** FLX4 jog = scrub the loom? Pads = the current node's children
  (pad 1–4 = tie child 1–4)? Browse knob = move the selection along threads?
