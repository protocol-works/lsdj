# Architecture Decision Records

This directory holds **Architecture Decision Records (ADRs)** - short documents
that capture a significant decision, the context that forced it, and the
consequences we accepted.

Record a decision here when it is hard to reverse, affects more than one team or
component, or a future reader would reasonably ask "why was it done this way?".
Routine, easily-reversible choices do not need an ADR.

## Conventions

- One file per decision, named `NNNN-short-title.md` (zero-padded, sequential).
- `0001` onward; never renumber or delete an ADR.
- An ADR is immutable once **Accepted**. To change a decision, write a new ADR
  and set the old one's status to `Superseded by ADR-NNNN`.
- Use [`template.md`](template.md) as the starting point.

## Creating one

Use the `write-adr` skill - it picks the next number, fills in the date, and
scaffolds the file from the template.

## Index

| ADR | Title | Status |
| --- | ----- | ------ |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-browser-app-with-python-model-workers.md) | Browser app with Python model workers, deferring Tauri | Superseded by 0018, 0019 |
| [0003](0003-frontend-audio-mixing-via-web-audio.md) | Frontend audio mixing via Web Audio | Superseded by 0017 |
| [0004](0004-style-is-a-weighted-prompt-blend-tempo-is-not-a-parameter.md) | Style is a weighted prompt blend; tempo is not a parameter | Accepted |
| [0005](0005-hardware-control-via-web-midi-in-the-frontend.md) | Hardware control via Web MIDI in the frontend | Accepted |
| [0006](0006-cue-output-via-a-second-audio-sink.md) | Cue output via a second audio sink | Superseded by 0017 |
| [0007](0007-flx4-phones-jack-via-a-backend-cue-sink.md) | FLX4 phones jack via a backend cue sink | Superseded by 0017 |
| [0008](0008-color-fx-as-one-knob-curves-at-a-pre-fader-insert.md) | Color FX as one-knob curves at a pre-fader insert | Accepted |
| [0009](0009-freeze-pads-loop-played-audio-at-the-channel-head.md) | Freeze pads loop played audio at the channel head | Accepted |
| [0010](0010-beat-detection-on-the-output-behind-an-honesty-gate.md) | Beat detection on the output, behind an honesty gate | Superseded by 0025 |
| [0011](0011-deck-to-deck-style-sampling-via-audio-embeddings.md) | Deck-to-deck style sampling via audio embeddings | Accepted |
| [0012](0012-generated-pads-via-a-spawned-sa3-mlx-subprocess.md) | Generated pads via a spawned sa3_mlx subprocess | Accepted |
| [0013](0013-playback-decks-play-decoded-tracks-loading-decides-the-mode.md) | Playback decks play decoded tracks; loading decides the mode | Accepted |
| [0014](0014-beat-matching-via-varispeed-tracks-against-the-measured-stream.md) | Beat-matching via varispeed tracks against the measured stream | Accepted |
| [0015](0015-hot-cues-in-deck-state-loops-on-the-buffer-source.md) | Hot cues in deck state, loops on the buffer source | Proposed |
| [0016](0016-beat-loops-length-scaling-over-the-loop-region.md) | Beat loops: length-scaling over the M21 loop region | Proposed |
| [0017](0017-native-rust-audio-engine-superseding-web-audio.md) | Native Rust audio engine, superseding frontend Web Audio mixing | Accepted |
| [0018](0018-native-macos-shell-tauri-with-python-sidecars.md) | Native macOS shell: Tauri with Python sidecars | Accepted |
| [0019](0019-pcm-transport-from-python-sidecars-to-the-rust-engine.md) | PCM transport from the Python sidecars to the Rust audio engine | Accepted |
| [0020](0020-rust-interface-state-store-and-mcp-server.md) | Rust as the single interface-state store, exposed via a native MCP server | Proposed |
| [0021](0021-split-master-and-cue-to-independent-output-devices.md) | Split master and cue to independent output devices (dual-mode) | Accepted |
| [0022](0022-persist-generated-samples-and-loops.md) | Persist generated samples and loops in a samples library | Accepted |
| [0023](0023-midi-note-and-drum-conditioning-over-the-deck-control-channel.md) | MIDI note and drum conditioning over the deck control channel | Accepted |
| [0024](0024-key-detection-in-the-rust-audio-engine-behind-an-honesty-gate.md) | Key detection in the Rust audio engine, behind an honesty gate | Accepted |
| [0025](0025-beat-detection-in-the-rust-audio-engine.md) | Beat detection in the Rust audio engine | Accepted |
| [0026](0026-lane-driven-variable-chunk-segmentation.md) | Lane-driven variable chunk segmentation for the modulation timeline | Accepted |
| [0027](0027-headphone-preview-for-library-items.md) | Headphone preview ("audition") for library items | Accepted |
| [0028](0028-sa3-lora-via-the-mlx-path.md) | Stable Audio 3 LoRA finetunes via the MLX path | Accepted |
| [0029](0029-output-resampling-for-non-48k-devices.md) | Output resampling for devices with no 48000/f32 config | Accepted |
| [0030](0030-track-decode-and-offline-track-analysis-in-the-rust-shell.md) | Track decode and offline track analysis in the Rust shell | Accepted |
| [0031](0031-native-midi-in-the-rust-core-superseding-web-midi.md) | Native MIDI in the Rust core, superseding Web MIDI | Accepted |
| [0032](0032-standalone-midi-keyboard-window.md) | Standalone MIDI keyboard window, routing decoupled from steering | Accepted |
| [0035](0035-dual-envelope-beat-detection-with-fast-change-invalidation.md) | Dual-envelope beat detection with fast change invalidation | Accepted |
| [0037](0037-platform-mrt2-runtime-contract.md) | Platform MRT2 runtimes behind one worker contract | Accepted for implementation |
