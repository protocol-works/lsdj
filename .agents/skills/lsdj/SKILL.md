---
name: lsdj
description: Drive LSDJ over its MCP server (the `lsdj` tools) — solo or alongside the human DJ. Observe with get_state, mix and glide the faders, steer the realtime decks, and generate tracks/samples including LoRA styles. Use when playing or supporting a set on LSDJ, prepping material, or testing the MCP surface live.
---

# Driving LSDJ

LSDJ is a two-deck generative DJ instrument. Every tool mutates the same
store the UI and MIDI controller use, so your moves show on screen and the
human's moves show in `get_state` — trust it, and re-read it after the human
touches anything.

## Who's driving?

The surface is identical whether you're the DJ or the second pair of hands —
what changes is how boldly you move and how much you coordinate. Read the
mode from what the human asked for, and expect it to shift mid-set:

- **You drive** ("play a set", "take over"): own the arc. Keep music
  continuous, prep the idle deck while one plays, generate ahead of need,
  and narrate your plan in chat as you go — the human can grab any fader at
  any time, and your narration is what makes that a handoff instead of a
  collision.
- **Driving together** (the human is at the controller or UI): the human
  owns whatever they're touching. Observe first, prefer additive moves —
  prep the idle deck, cue, generate, stage FX — over overriding, and say
  what you're about to do before changing anything audible. If a control
  moves under you, that's the human: back off and re-read `get_state`.
- **Unsure?** Act like the second operator — the least intrusive reading —
  and ask.

## First moves

1. Call `get_state` — decks, mixer, FX, style pads, cues, transport, models.
2. Check what's audible before touching it: `crossfade`, per-deck `gain`,
   and `onAir`. Glide (`ramp_ms`), don't jump.
3. If tools seem missing or schemas look stale, the client is holding an old
   tool list — reconnect the `lsdj` MCP server (new tools only appear after
   a client re-list).

## The instrument model

Deck 0 = A, deck 1 = B. Each deck is in one of two modes (`get_state`
`mode`):

- **realtime** — a Magenta RT model streams endlessly, steered by the style
  pad (`set_style`, `set_style_cursor`, `set_prompt`), held notes
  (`set_notes`), drum conditioning (`set_drums`), and `set_model`. Bring it
  to the master with `set_on_air` (off-air = prep: it keeps generating,
  audible only in the headphone cue). Steering changes are heard once the
  buffered audio drains (a few seconds); note steering resets on
  play/stop/model switch — start the deck first, then steer.
- **playback** — a loaded file plays: `load_track` (from `list_songs`) or a
  finished `generate_track` flips the deck to playback; `eject` hands it
  back to the realtime stream. `deck_play`/`deck_stop` are mode-aware. A
  deck's top-level `playing` covers the realtime stream only; on a playback
  deck read `transport.playing`.

## Mixing

- `set_crossfade(position, ramp_ms?)` and `set_volume(deck, gain, ramp_ms?)`
  take an optional glide (0–60 000 ms, engine-side, click-free) — use it for
  every musical move; instant jumps are for corrections.
- `set_eq(deck, band, value)`: low/mid/high, 0.5 = flat. `set_trim` (dB),
  `set_cue` (headphone tap), `set_cue_mix` (cue↔master blend).
- Color FX: `set_fx(deck, kind)` with kinds `filter`, `dubEcho`, `space`,
  `crush`, `noise`, `sweep`; `set_fx_amount(deck, amount)` 0..1 — for
  `filter`, 0.5 is bypass (below = low-pass, above = high-pass). `clear_fx`
  bypasses bit-exact. `set_fx_amount` still steps (no ramp) — move it in
  small increments if you want a sweep.

## Playback-deck performance

`set_tempo` (BPM varispeed — needs a known BPM; library tracks can carry
`bpm: null`, generated tracks are analysed), `sync_deck` (beat-match to the
other deck), `beat_loop(deck, beats)`, `seek_track` (releases an active beat
loop), hot cues (`set_hot_cue`/`clear_hot_cue`/`jump_to_hot_cue`), and
`toggle_pad(deck, slot)` — a filled loop/sample pad plays or stops (loops
layer, one-shots fire once); an EMPTY slot captures a freeze loop from the
deck's live stream. Slot contents are in `get_state` `loopLabels`.

## Generation

- `generate_sample(prompt, seconds, kind, one_shot?, loras?)` — kinds `sfx` /
  `music` (Stable Audio 3) / `magenta`; lands in the samples library
  (`list_samples`, `load_sample`).
- `generate_track(deck, prompt, seconds, loras?)` — long-form SA3, **async**:
  it returns immediately with a job id and an ETA (generation runs at ~2.3 s
  of audio per second; a LoRA stack adds a ~130 s one-off merge). Keep
  mixing; poll `generation_status` (running/done/failed, newest first) or
  watch `get_state` for the deck to flip to playback when the track
  auto-loads. The human sees the pending row in the Media Explorer, so
  announce what you're generating and why.
- **LoRA styles:** `list_loras` → pass `loras: [{"name": "<base>/<slug>",
  "strength": 1.0}]` (max 4, strength 0–4, ~1 = as trained). Adapters are
  per-base-model: `medium/…` adapters ride `generate_track` only, `small/…`
  ride `generate_sample` sfx/music; the `magenta` engine takes none.

## A clean transition (A playing, B incoming)

1. Prep B off-air (realtime) or paused (playback); cue it (`set_cue`).
2. Beat-match: `sync_deck(1)` — or `set_tempo` if sync can't (no BPM).
3. Kill B's bass: `set_eq(1, low, 0.0)`; start B; bring it on air.
4. Swap bass mid-blend: B's low up as A's low goes down.
5. `set_crossfade(1.0, ramp_ms: 4000–16000)` — long glides hold constant
   power the whole way.

## Getting the most out of the models

Prompting the two engines well is its own craft — read these before writing
prompts:

- [references/magenta-realtime-prompting.md](references/magenta-realtime-prompting.md)
  — steering the realtime decks: how the weighted style blend works, what
  prompts land, and the measured limits (tempo is NOT text-steerable).
- [references/stable-audio-prompting.md](references/stable-audio-prompting.md)
  — prompt structure for generated tracks and samples, tempo/key that
  actually sticks, LoRA interplay, and timing budgets.

## Ops notes

- The first call after an idle stretch can time out (-32001); retry once
  immediately.
- Generation errors surface in `generation_status` `detail` and as the
  Media Explorer row failing — report them, don't retry blind.
