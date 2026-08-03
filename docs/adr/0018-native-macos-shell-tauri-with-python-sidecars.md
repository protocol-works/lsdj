# 0018. Native macOS shell: Tauri with Python sidecars

- **Status:** Accepted (2026-06-15) — spikes PASS ([`spike-packaging.md`](../spike-packaging.md), [`spike-c-midi.md`](../spike-c-midi.md))
- **Date:** 2026-06-15
- **Deciders:** Daniel Peter

## Context

ADR-0002 built LSDJ as a browser app served by a local Python backend and
deferred a desktop shell, with an explicit revisit trigger: "if the project
grows toward a self-contained distributable app … record that change as a new
ADR superseding this one." We are now there — the goal is a double-clickable
macOS app, not a "start a server, open Chrome" tool.

ADR-0002 assumed a Tauri wrap would be "no protocol or UI rework — move the
frontend into a webview, run Python as sidecars." That was written before the
hardware-control decision (ADR-0005), and the gap it missed is the whole
problem:

- Tauri on macOS renders in **WKWebView (the Safari engine), not Chromium.**
  LSDJ's signature surface — FLX4 control — is Web MIDI (ADR-0005), and
  WebKit does not implement Web MIDI at all. A naive wrap would silently delete
  hardware control. The other two Chromium-leaning APIs (Web Audio worklets and
  `setSinkId`) are addressed separately by ADR-0017's move to a native audio
  engine.
- The model integration is unchanged: ADR-0002's core still holds — the bridge
  to the model is a stream of PCM from a Python process whatever the shell — so
  the model workers stay Python. Who *consumes* that stream changes (the Rust
  engine, not the browser); that transport is ADR-0019.
- A distributable must also carry a heavy, non-trivial-to-package backend
  (FastAPI + `magenta_rt` + MLX + the `sa3_mlx` checkout) and ~13 GB of model
  weights that must not live in the bundle.

## Decision

We will package LSDJ as a native macOS app with **Tauri v2**:

- Hardware MIDI runs through **`tauri-plugin-midi`**, which shims
  `navigator.requestMIDIAccess` to the W3C Web MIDI API over `midir`/CoreMIDI,
  SysEx and output included. The `control/` layer, the measured FLX4 byte map,
  and its unit tests carry over unchanged; MIDI no longer flows through the
  webview.
- Python is retained **only for model inference**. The MRT2 and `sa3_mlx`
  workers ship as **Tauri sidecars**, frozen with PyInstaller and supervised by
  the Tauri process; the Rust shell takes over the rest of today's FastAPI
  controller — serving the frontend, the per-deck WebSocket protocol, and worker
  supervision. Replacing the inference itself with native Rust is out of scope
  (see Consequences).
- **Model weights are not bundled.** The app preserves the existing first-run
  download (already in `just setup` since M19) behind a progress UI; the bundle
  carries only the shell and the frozen backend.
- Distribution builds are **code-signed and notarized** for Gatekeeper.

This supersedes ADR-0002's deferral of a desktop shell, while preserving its
Python-model-workers decision.

## Consequences

- A real, double-clickable, signed app — no terminal, no separate browser.
- Hardware control survives the move intact, and should improve: routing MIDI
  through `midir`/CoreMIDI instead of the webview is expected to remove the class
  of webview-MIDI fragility noted in CLAUDE.md (the Playwright renderer crash on
  MIDI output with the FLX4 attached) — but MIDI output (LED echo) and the
  position-query SysEx through the young `tauri-plugin-midi` are measured on the
  device, not assumed (see the follow-up checklist).
- The shell does **not** require the C++ engine (`magentart::core`) that
  ADR-0002 named as its expected supersession. Python sidecars plus a native
  Rust audio/shell layer is a lighter path; `magentart::core` stays a deeper
  future option, not a prerequisite.
- Python is reduced to an isolated inference RPC — the model forward pass and
  `embed_style` stay on the model authors' supported runtime, which keeps the
  "MRT2 streaming API shifts under us" standing risk cheap to absorb. Going fully
  Python-free would buy only packaging simplicity (no PyInstaller, a smaller
  bundle), not performance — the hot loop is already compiled MLX Metal kernels —
  at the cost of extending MRT2's C++ engine or reimplementing two research
  models. Deferred.
- Audio is handled by its own decision (ADR-0017), and **first ship is gated on
  it**: the native app never runs Web Audio in the shell. WKWebView only ever
  renders UI, so its weak `AudioWorklet`/`setSinkId` support is moot — there is no
  Web-Audio-in-WKWebView interim and no Electron fallback. The accepted cost:
  there is no shippable native app until ADR-0017's engine lands.
- New dependency to justify (per `.claude/rules/security.md`):
  `tauri-plugin-midi` is young (v0.2, the specta-rs org) but a thin shim over
  the mature `midir`; pin it and be ready to vendor or fork its small surface.
- New build complexity: a PyInstaller freeze of an MLX + native-extension stack,
  the `sa3_mlx` checkout vendored rather than pip-frozen (its
  checkout-not-a-package status is a standing risk), Tauri bundling, and an Apple
  Developer ID for signing/notarization; packaging is a release concern. The
  browser dev loop (`just dev-frontend`) stays useful for UI work, but once audio
  is native Rust it no longer exercises the audio path — that path is verified by
  the native engine's Rust tests (ADR-0017), not the `verify_m*.mjs` corpus.
- Follow-up: a hardware checklist for the packaged app — MIDI input, **MIDI
  output (LED echo) and the position-query SysEx through `tauri-plugin-midi`**,
  audio devices, first-run download, Gatekeeper launch — in the project's
  checklist tradition.

## Alternatives considered

- **Electron** - bundles Chromium, so Web MIDI, worklets, and `setSinkId` all
  work untouched and no audio rewrite is forced. The lowest-risk path to a native
  app *today*. Rejected: it locks in a heavier Chromium runtime and a Node main
  process, while the project is committing to a native Rust audio engine
  (ADR-0017) that makes WKWebView's web-API gaps moot — and with first ship gated
  on that engine (see Consequences), there is no interim that Electron would
  serve.
- **Thin Chrome launcher / PWA** - a tiny `.app` that boots the backend and
  opens `Chrome --app=…`, or a Chrome-installed PWA. Days of work, keeps every
  web feature because it is Chrome. Rejected as the end state: it needs Chrome
  installed and never feels first-class, though it is the fastest way to a dock
  icon if one is wanted before the real app exists.
- **Tauri default (WKWebView) without the MIDI plugin** - breaks Web MIDI and
  kills FLX4 control. Rejected outright.
- **Replace all Python with Rust (native inference)** - two routes: FFI to
  MRT2's own `magentart::core` C++ engine (weights compiled to a `.mlxfn`,
  streamed on MLX), or reimplement MRT2 and Stable Audio 3 in `candle`/`mlx-rs`.
  The first is the genuine single-binary path and ADR-0002's named supersession,
  but the `embed_style`/blend steering surface LSDJ depends on is not a
  documented C++ surface and SA3 has no C++ engine at all; the second
  reimplements a 2.4B research model and fights upstream drift. Deferred to a
  future ADR, gated on the C++ steering surface maturing.
- **Stay a browser app (ADR-0002 unchanged)** - zero packaging work, but no
  distributable; the user runs a server and opens a browser. The decision being
  superseded.

<!-- Status values: Proposed | Accepted | Rejected | Deprecated |
     Superseded by ADR-NNNN -->
