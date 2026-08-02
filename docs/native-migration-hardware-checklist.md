# Native migration — hardware & integration checklist

Behaviour the automated suite (cargo / vitest / pytest) cannot reach: the live
Tauri webview, real audio devices, the FLX4, the model sidecars, and packaging.
Per [`CLAUDE.md`](../CLAUDE.md), a human ticks these before the work is "done".

Run the native stack with `just tauri-dev` (Tauri app + sidecars; needs
`just setup` for backend deps — install model weights in-app from the settings
drawer).

## Part 2 — MIDI (`tauri-plugin-midi`)

- [ ] FLX4 plugged in: the app sees it within ~1 s (the plugin's 1 s poll).
- [ ] Moving a knob/fader sends CC that reaches the mixer (input path).
- [ ] On connect the position-query SysEx fires and the controls sync.
- [ ] Pad LEDs light (output path) — feedback from the app reaches the hardware.
- [ ] Unplug + replug mid-set recovers without a permission prompt.

## Part 3 — UI ↔ engine over IPC

- [ ] Channel meters, master meter, and limiter-GR readout move with audio.
- [ ] EQ kills, volume, crossfade, and the six Color FX audibly match the Web
      Audio build (the parity oracle: same gestures, `just dev-frontend`).
- [ ] EQ sweeps are click-free: dragging any band — or a fast FLX4 EQ-knob CC
      sweep — glides smoothly, no zipper/crackle. The band gain is `follow()`-
      smoothed in the Rust engine (`graph.rs` `EQ_SMOOTH_SECS`, ~15 ms) instead of
      rebuilding+resetting the filter per knob tick. Confirm the feel is tight
      enough for hard kills; retune `EQ_SMOOTH_SECS` by ear if it smears.
- [ ] Trim (gain staging) and the on-air gate behave: off-air mutes the master
      feed but the channel meter stays live.
- [ ] Selecting / clearing an effect (FX-none) engages / removes it.
- [ ] Load a track: waveform overview renders, transport (play/pause/seek), the
      varispeed tempo, and a track loop work; the playhead is exact.
- [ ] Media browser → Folder tab: the native OS picker opens (dialog plugin), the
      chosen folder's audio files list, and loading one onto a deck plays it. (The
      Chromium File System Access API is unavailable in WKWebView; this is the
      `list_audio_files` / `read_audio_file` path. A read is scoped to the chosen
      folder — picking a folder then loading a file outside it must fail.)
- [ ] Freeze pads, generated pads (one-shots + loops), and style-sample capture.
- [ ] Master recording works: record the master bus, stop, and the saved WAV
      plays back the set (16-bit PCM, streamed to disk — no length cap; ADR-0028).
      Record past 30 min and confirm the take is whole and RAM stays flat; the file
      lands in the settings-chosen folder (empty = Downloads).
- [ ] Jog the wheel while a track plays: the playhead phase-bends to catch up
      (push ahead / drag back for beatmatching) with no click — the engine's
      `nudge_track_phase` rate bend, not a seek. Paused, a jog is a fine seek.
- [ ] Known gaps (documented stubs, not bugs): synced dub echo (`setBeatPeriod`)
      shows its stub behaviour.

## Part 4 — Inference sidecars

- [ ] On launch, each deck spawns `python -m lsdj.sidecar` (started with the app,
      no flag); the Rust log shows the loopback port and the sidecar connecting.
- [ ] Audio generates: PCM streams sidecar → engine → speakers, no underruns
      (watch `engine_snapshot` deck-ring fill / underruns).
- [ ] Deck control reaches the worker: play/stop, set-prompt, set-style change
      the output within a few seconds.
- [ ] `sidecar://status` events surface in the webview (ready / chunk / errors).
- [ ] Killing a sidecar process emits `worker_died`; the deck goes silent without
      crashing the app. (In-process auto-restart / model-switch is a follow-up.)
- [ ] Quitting the app cleanly kills the sidecar processes (no orphans).

## Part 5 — Native cue routing

The engine derives a headphone-cue feed (PFL bus blended with master per the cue
mix) and the device routes it to channels 3/4 on a ≥4-channel output (the FLX4).
Replaces the second-sink cue (ADR-0006) and the backend `sounddevice` sink
(ADR-0007). (`sounddevice` + `cue.py` + `/ws/cue` are retired at cutover, part 7,
with the browser path that still depends on them.) The output device is chosen in
the mixer's Phones section (the Output picker → `list_output_devices` /
`set_output_device`); the engine opens it preferring its widest config so the cue
reaches 3/4, and switches live (re-creates the PCM rings, swaps the render
thread's producers). A device with < 4 channels shows "no cue" — master only.

- [ ] FLX4 selected as the output (mixer → Phones → Output): master plays on the
      main out (channels 1/2) and the cue plays on the phones jack (channels 3/4).
- [ ] The Output picker lists devices, marks the cue-capable (≥4ch) ones, and
      switching to the FLX4 takes effect live (no app restart); switching to a
      bad/unplugged device surfaces an error and leaves the current audio playing.
- [ ] Cue a deck (PFL): it is audible in the phones even when crossfaded fully
      out of the master.
- [ ] The cue-mix knob blends cue ↔ master in the phones (0 = cue, 1 = master).
- [ ] On a plain stereo output device the master is unaffected and the cue is
      simply absent (no crash, no glitch).
- [ ] Booth output (a third pair mirroring the master) — follow-up, not yet
      wired.

## Part 6 — Packaging (`docs/native-packaging.md`)

- [ ] `just freeze-backend` produces `src-tauri/sidecar-dist/lsdj_backend/`
      (~1.1 GB) and the frozen binary runs: `lsdj_backend --deck a --model
      mrt2_small --port <n>` connects to a listener and streams a chunk.
- [x] The frozen backend is added by the release-only Tauri `resources` overlay;
      the packaged app resolves it through `LSDJ_BACKEND_BIN` and fails startup
      if the resource is absent.
- [ ] `just tauri-build` with the `APPLE_*` env set produces a signed, notarized,
      stapled `.app` + `.dmg` (entitlements applied; the sidecar tree signed).
- [ ] First launch on a clean Mac: Gatekeeper passes (no "unidentified
      developer"); the one-time scan completes; subsequent launches are fast.
- [ ] First run with NO weights: the model-download screen appears, downloads to
      `$MAGENTA_HOME/magenta-rt-v2`, then reveals the decks (preserves
      `just setup`).

## Part 7 — Cutover

The native app is the product; ADR-0003/0006/0007 are superseded; the backend cue
sink + `sounddevice` are removed. Deck control routes to the sidecars over IPC and
status arrives as `sidecar://status` events (`useDeck` selects this with
`isTauri()`).

- [ ] In the native app, deck play/stop and set-style reach the sidecar (audio
      responds) — no `/ws/deck` socket is opened.
- [ ] Sidecar status (ready / chunk / errors) drives the deck UI state.
- [ ] The browser dev path (`just dev-frontend`) still works as the parity oracle
      (control + status over the WebSocket, Web Audio mixing).
- [ ] Remaining native integration (follow-ups, need the live stack):
  - [x] Beat/loudness analysis fed from a sidecar/engine feature tap (the model
        PCM no longer transits the UI). — done: the sidecar reader tees model PCM
        over a per-deck Tauri Channel (`subscribe_deck_pcm`); `useDeck` pushes it
        to beat/loudness/band. On the live stack, verify:
    - [ ] The deck BPM (M14) detects from a live generative deck, same as web.
    - [ ] The live beat-phase meter + the zoom beat lattice (M20) track correctly
          and stay BLANK (not wrong) when stale — the native per-stream
          played-frame origin fix is timing math only checkable with live audio.
    - [ ] The `Channel` raw-bytes path delivers an `ArrayBuffer` on WKWebView
          (the less-battle-tested raw-RESPONSE direction); if it degrades, fall
          back to a base64/`Vec<u8>` framed encoding. Confirm the CSP allows it.
    - [ ] Auto-gain (M17) tracks loudness on the native deck.
  - [x] In-process model switch / sidecar restart (the model is fixed at spawn).
        — done: `deck_set_model` restarts the sidecar reusing the deck ring. On
        the live stack, verify: switching a deck's model reloads + plays the new
        model; no `worker_died`; the other deck is uninterrupted; a bad/missing
        model leaves the old sidecar running (bind/launch fails) or surfaces
        `worker_died` recoverable by re-selecting. Known follow-up: the old
        model's already-buffered ~3 s of ring PCM is not flushed on switch (needs
        an engine-side realtime-ring reset), so a brief old-model tail can play.
  - [x] The sa3 pad/track HTTP generation path (`/api/render`, `/api/generate`)
        rehosted for the native app. — done: the Rust shell spawns the FastAPI
        controller (generation-only by construction — it spawns no deck workers)
        on a loopback port via `python -m lsdj.controller --port N` in dev or
        the shared frozen backend's `--generation-server` mode in a release
        (`generation.rs`), the
        webview fetches it via `getApiBaseUrl()` (CORS on; CSP left null/permissive
        — tightening is a security follow-up). On the live stack, verify:
    - [ ] Pad generation (Magenta `/api/render`) and track generation (sa3
          `/api/generate`) work in the native app.
    - [ ] Quitting the app kills the generation server (no orphaned uvicorn /
          loopback-port + model leak) — `RunEvent::Exit` handles this since macOS
          `process::exit` skips managed-state `Drop`. Same for the sidecars.
    - [x] Style sampling routed to the sidecar (the gen server has no deck
          workers) — done: a binary `FRAME_EMBED` carries the captured PCM to the
          target deck's sidecar (`deck_embed_sample`), which injects an
          `embed_sample` command `worker.py` handles. Verify on the live stack:
          sample deck A's tail, drop it on a deck B pad, hear B adopt the style.
    - [ ] Dev: `just tauri-dev` launches the gen server + sidecars (the
          default `uv run` uses the backend dir as CWD; override with
          `LSDJ_GENERATION_CMD` / `LSDJ_SIDECAR_CMD`, or point both at a freeze
          with `LSDJ_BACKEND_BIN`).
  - [x] Remove the now-inert browser cue UI (phones picker / `cueStream`) — done
        at cutover, and the native Output picker (Part 5) replaces it: a
        `list_output_devices` / `set_output_device` dropdown in the mixer's Phones
        section that opens the chosen device (cue on 3/4 of a ≥4-channel device)
        and switches live.

## Part 8 — In-app model manager (issue #43)

The model manager (settings-drawer panel) installs both model families with no
terminal. The download/install paths cannot be reached by cargo/vitest/pytest
(real multi-GB weights, the packaged frozen bundle, the SA3 uv/venv build), so a
human verifies them here. Run with `just tauri-dev` for dev paths; the packaged
checks need a `just tauri-build` bundle on a clean machine (no `~/Documents/
Magenta`, no `~/Repos/stable-audio-3`).

- [ ] Open the settings drawer from the status bar; both families list with
      installed-vs-missing status and on-disk size.
- [ ] Install a missing Magenta model from the drawer (no terminal): progress
      streams, and when done the deck model picker offers it AND a deck actually
      loads it — i.e. the shared `resources/` were fetched (`--init-resources`),
      not just the two model files.
- [ ] Packaged build only: the same install works from the frozen bundle
      (`lsdj_backend --init-resources` / `--download-model`) — confirms
      `huggingface_hub` / `fsspec` / `click` are collected (a missing fsspec
      registration fails only here, never in CI).
- [ ] Install the Stable Audio 3 stack from the drawer on a machine with no
      checkout: tarball fetch → extract → `install.sh -y --python 3.11` → warm,
      with staged progress and no git/tty prompt; SA3 generation then works.
- [ ] Cancel an in-flight install: the child is killed and the partial dir is
      cleaned (no half-written model folder, no spurious `models://changed`).
- [ ] Drop a valid model folder into the models dir: it appears in the manager
      and the picker once both files are present (the watcher), not before.
- [ ] "Open models folder" reveals the models dir in Finder; removing a model
      there is reflected live in the manager and the deck picker (the watcher).
      (In-app deletion is intentionally absent — moving multi-GB weights to the
      Trash fails on iCloud/dataless files, -8013.)
- [ ] Quit mid-install: no orphaned install process (the `InstallManager` is torn
      down from `RunEvent::Exit`, like the sidecars).
