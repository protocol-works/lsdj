<p align="center">
  <img src="docs/img/lsdj-mark.svg" alt="LSDJ" width="128">
</p>

<h1 align="center">Latent Space DJ</h1>

<p align="center"><strong>An instrument for real-time AI music.</p>

<p align="center">
  <img src="docs/img/lsdj-demo.gif" alt="LSDJ — latent space disc jockey" width="640">
</p>

<p align="center">
  <a href="https://buymeacoffee.com/brxs"><img src="https://cdn.buymeacoffee.com/buttons/v2/default-yellow.png" alt="Buy me a coffee" height="42"></a>
</p>

Two locally-running model decks, steered by text prompts and mixed like vinyl:
three-band EQ, one-knob Color FX, a crossfader, headphone cue, and full
Pioneer DDJ-FLX4 control. The live decks run on
[Magenta RealTime 2](https://github.com/magenta/magenta-realtime); generated
pads and finished tracks come from Stable Audio 3. See
[`docs/ROADMAP.md`](docs/ROADMAP.md) for how it got here and
[`docs/adr/`](docs/adr/) for the architecture decisions.

## Requirements

- Apple Silicon Mac (MLX backend)
- [uv](https://docs.astral.sh/uv/)
- ~13 GB disk for model weights (downloaded in-app on demand: Magenta
  ~4.5 GB for both deck models, Stable Audio 3 ~8 GB including the
  medium track model)
- macOS 11+ — LSDJ ships as a native app (Tauri + a Rust audio engine +
  Python inference sidecars; run with `just tauri-dev`, build with
  `just tauri-build`)
- Optional: a Pioneer DDJ-FLX4 for hardware control and its headphone jack

All common tasks live in the [`justfile`](justfile) — run `just` to list them.

Linux x86_64 AppImage support is under active qualification. Packaging and
hosted CI are available, but a public Linux release remains gated on the
portable MRT2/Stable Audio backends, licensing, and real NVIDIA/audio/FLX4
evidence. See [the Linux support status](docs/linux.md); do not infer hardware
support from a green hosted build.

## Setup

```sh
just setup   # backend deps, frontend deps + build (no model weights)
```

**Models install in the app**, not the terminal. Launch with `just tauri-dev`,
open the settings drawer, and install what you need from the model manager — both
families download with live progress. Magenta deck models (`mrt2_small` and the
heavier, higher-quality `mrt2_base`, selectable per deck — the app warns when the
combined selection looks tight for your RAM) and Stable Audio 3 (generated pads
and tracks) land in the app-owned `~/Library/Application Support/LSDJ`
(`MAGENTA_HOME` / `SA3_MLX_HOME` override the locations). `just migrate-models`
relocates an existing install—including one under the previous app name—into
that folder without re-downloading. The Rust host owns the cross-platform
config/data/cache/assets/staging mapping and passes it explicitly to Python; see
[the platform path contract](docs/platform-paths.md).

## Run

```sh
just tauri-dev
```

This launches the native app — add style targets to a deck's pad, hit
play, blend targets by dragging the cursor (or the dots themselves, to
cluster them), and ride the crossfader between decks.

- **Mixer** — per-deck volume and Hi/Mid/Low EQ, crossfader, and **Record**,
  which captures the master bus to a downloadable WAV. The health row shows
  the stream buffer, underrun count, and generation speed.
- **Color FX** — one knob per deck over a chosen effect: Filter (bipolar
  LPF/HPF), Dub Echo, Space, Crush, Noise, Sweep. The knob's centre/zero is a
  bit-exact bypass ([ADR-0008](docs/adr/0008-color-fx-as-one-knob-curves-at-a-pre-fader-insert.md)).
- **Freeze loops** — capture the last bars of a deck into one of four
  loop slots and hold the moment on air while you re-steer the model
  underneath
  ([ADR-0009](docs/adr/0009-freeze-pads-loop-played-audio-at-the-channel-head.md)).
  Captures and generated pads now auto-save to the samples library so a
  good one survives the session (see **Samples** below).
- **Beat detection** — each deck shows its detected BPM behind an
  honesty gate (a dash rather than a wrong number); with a confident
  tempo the Dub Echo syncs to the beat and freeze captures quantise to
  whole beats
  ([ADR-0010](docs/adr/0010-beat-detection-on-the-output-behind-an-honesty-gate.md)).
- **Deck-to-deck style sampling** — one press puts "the sound of the
  other deck, right now" on a deck's style pad as a blendable target;
  sampled targets are session-only by design
  ([ADR-0011](docs/adr/0011-deck-to-deck-style-sampling-via-audio-embeddings.md)).
- **Crates** — save a deck's pad + Color FX as a named preset, browse
  the crate from the FLX4's rotary, and load onto either deck mid-set;
  export/import as JSON for backup and sharing.
- **Samples** — frozen loops, generated pads, and short SFX/Music
  compositions persist to `~/Documents/LSDJ/generated_samples`; the Media
  Explorer's **Samples** tab browses them and loads one back into a deck
  loop slot. A loaded sample **layers** over the deck — it's summed on top
  of the live stream and several stack at once, so you can build a jam over
  whatever the model is playing (freezes still *replace*, to hold a moment)
  ([ADR-0022](docs/adr/0022-persist-generated-samples-and-loops.md)).
- **Master housekeeping** — a limiter on the master (the meter, the
  recording, and the phones all hear the limited signal; its gain
  reduction shows in the mixer) and per-channel auto-gain Trim that
  levels decks of different loudness, with a manual override.
- **Headphone cue** — hit a channel's **Cue**, ride the **Cue mix** knob
  between cue and master, and route **Main output** and **Cue output** to
  independent devices: the cue can ride the FLX4's own headphone jack
  (channels 3/4 of the one device) or play out any second output — the laptop
  jack, Bluetooth headphones, a second interface
  ([ADR-0021](docs/adr/0021-split-master-and-cue-to-independent-output-devices.md)).

Settings (pad arrangements, volumes, crossfade) persist across reloads.
Shortcuts: `A`/`B` focus a deck's style-target input, `X` focuses the
crossfader.

For development, `just tauri-dev` runs the native shell with a hot-reloading
webview against the Rust engine and the Python sidecars.

## Hardware control (Pioneer DDJ-FLX4)

Plug in the FLX4 and click **Connect MIDI** (Chrome asks for MIDI with SysEx;
plain MIDI works too, minus position sync). Mapped controls:

- Play/pause, channel faders, three-band EQ, crossfader
- Channel **CUE** buttons (headphone cue) and the transport **CUE** button
  (deck prep: prime a stopped deck off-air, stop a playing one)
- **SMART CFX** knob — Color FX amount; hold **SHIFT** to sweep the style pad
  instead
- **PAD FX** pad bank — select the deck's effect (re-press toggles it off);
  **HOT CUE** pads pick style targets; **SAMPLER** pads freeze loops
  (SHIFT + pad clears a slot)
- **HEADPHONES MIX** knob — cue mix
- **Browse rotary + LOAD buttons** — highlight a crate preset, load it
  onto deck 1 or 2

Knob and fader positions sync from the hardware on connect, and the LEDs
mirror app state. The measured byte map lives in
[`docs/midi-ddj-flx4.md`](docs/midi-ddj-flx4.md).

## Verify

- `just test` — backend pytest + frontend vitest
- `just lint` — format check, ruff, eslint, tsc
- `just check` — both of the above; what a PR must pass
- Hardware behaviour is verified by a human against the checklists in
  `docs/` (`m7-`, `m9-m10-`, `m12-hardware-checklist.md`)
