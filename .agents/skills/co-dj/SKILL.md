---
name: co-dj
description: Drive LSDJ as a co-DJ over its MCP server (the `lsdj` tools) — observe with get_state, mix and glide the faders, steer the realtime decks, and generate tracks/samples including LoRA styles. Use when DJing with LSDJ, prepping a set, or testing the MCP surface live.
---

# Driving LSDJ as a co-DJ

You are a second operator on a two-deck generative DJ instrument. Every tool
mutates the same store the UI and MIDI controller use, so your moves show on
screen and the human's moves show in `get_state` — trust it, and re-read it
after the human touches anything.

## First moves

1. Call `get_state` — decks, mixer, FX, style pads, cues, transport, models.
2. Check what's audible before touching it: `crossfade`, per-deck `gain`, and
   `onAir`. Never yank a fader the human may be riding — glide (`ramp_ms`).
3. If tools seem missing or schemas look stale, the client is holding an old
   tool list — reconnect the `lsdj` MCP server (new tools only appear after a
   client re-list).

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
  finished `generate_track` flips the deck to playback; `eject` hands it back
  to the realtime stream. `deck_play`/`deck_stop` are mode-aware. A deck's
  top-level `playing` covers the realtime stream only; on a playback deck
  read `transport.playing`.

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
  of audio per second). Keep mixing; poll `generation_status`
  (running/done/failed, newest first) or watch `get_state` for the deck to
  flip to playback when the track auto-loads. The human sees the pending row
  in the Media Explorer, so announce what you're generating and why.
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

## Ops notes

- The first call after an idle stretch can time out (-32001); retry once
  immediately.
- Generation errors surface in `generation_status` `detail` and as the
  Media Explorer row failing — report them, don't retry blind.
