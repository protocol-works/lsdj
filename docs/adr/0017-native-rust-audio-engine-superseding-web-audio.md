# 0017. Native Rust audio engine, superseding frontend Web Audio mixing

- **Status:** Accepted (2026-06-15) — spike PASS ([`spike-rust-audio.md`](../spike-rust-audio.md)); the ≥10-min endurance run remains a confirmation
- **Date:** 2026-06-15
- **Deciders:** Daniel Peter

## Context

ADR-0003 put all mixing in the browser's Web Audio API: a per-deck AudioWorklet
ring buffer → GainNode → equal-power crossfade, with recording tapped off the
master bus. That was the right call for a browser app and it still works. Two
things have since changed the forces:

- **We are moving to a native Tauri shell** — a double-clickable Mac app, with
  MIDI handled by `tauri-plugin-midi` over `midir`/CoreMIDI so the measured
  FLX4 byte map and the `control/` layer (ADR-0005) carry over unchanged. Tauri
  on macOS renders in WKWebView, not Chromium. WKWebView's `AudioWorklet` and
  `setSinkId` support is the one remaining risk to running LSDJ's audio
  there — which is precisely the engine ADR-0003 built. The shell decision puts
  Web Audio itself in question.
- **Web Audio has become the capability ceiling.** Varispeed (M20) shifts pitch
  with rate because `playbackRate` is resampling — a standing risk, and the
  reason keylock and time-stretch sit parked. The cue path is split across two
  workarounds: a stereo-capped second browser sink (ADR-0006) and a backend
  sounddevice sink for the FLX4 phones jack (ADR-0007), because the browser
  cannot reach USB channels 3/4. Neither improves inside Web Audio.

ADR-0003 anticipated this: its rejected "backend mixes and plays to a device"
alternative was deferred to "reconsider if device routing becomes a hard
requirement." The native shell makes it one — and a Rust engine in the shell,
not Python, is where that audio now belongs.

## Decision

We will move all audio mixing and DSP out of Web Audio into a native Rust
engine running in the Tauri process. Each deck's model worker streams PCM into
the engine over a local link; a wait-free ring buffer (`rtrb`) feeds the
CoreAudio callback (`cpal`); the mix graph — per-deck player, three-band EQ, the
Color FX insert, freeze/playback/loop playback, equal-power crossfade, and the
master limiter — is a `fundsp` `Net`, with live effect changes committed
click-free from a non-realtime frontend to the realtime backend. The JS
frontend becomes a thin control and visualization surface: it emits
`ControlIntent`s and renders meters, waveforms, and beat views from telemetry
the engine reports. This supersedes ADR-0003.

## Consequences

- The WKWebView audio risk **disappears** rather than being mitigated: with no
  Web Audio in the app, the webview only has to render UI, which it does well.
- Cue routing unifies. Main out, booth monitor, and the FLX4 phones jack
  (channels 3/4) are each just another `cpal` output stream or a CoreAudio
  aggregate device — collapsing ADR-0006 and ADR-0007 into one native path, and
  retiring ADR-0007's backend `sounddevice`/PortAudio dependency from the Python
  side.
- The capability ceiling lifts: a native engine puts pitch-independent
  time-stretch and keylock on the table (impossible in Web Audio), which would
  promote M25 harmonic mixing from advisory to corrective. The algorithm is
  available MIT (Signalsmith Stretch); only its Rust binding is immature today,
  so keylock is an upgrade tier — ship varispeed via mature resampling
  (`rubato`) first.
- Less from-scratch DSP than feared. `fundsp` ships biquad/shelf filters (EQ,
  Filter), a 32-channel FDN `reverb_stereo` (Space), `delay`/`fdn` (Dub Echo), a
  look-ahead `limiter` (M17), and `shape` with a crush mode (Crush); only Noise
  and Sweep are trivial hand-work. Its frontend/backend `commit()` split is a
  real-time-safe match for the existing `ControlIntent`→engine pattern, and
  `crossfade(Fade::Smooth, …)` gives click-free effect swaps for the Color FX
  bank and its bit-exact-bypass intent (ADR-0008).
- Harder: this reimplements the tested heart — but only the realtime mix graph:
  the player ring, three-band EQ, all six Color FX (ADR-0008), the
  freeze/playback/loop buffer sources (ADR-0009/0013), varispeed and loops
  (M20/M21/M23), the crossfade, and the limiter (M17) — i.e. the
  Web-Audio-node-bound code in `engine.ts`, `fxGraphs.ts`, and the worklets — and
  it gives up Web Audio's optimized native nodes. The M14/M20/M22 **analysis
  stays in TypeScript**: the beat tracker, loudness, band scroller, and the
  offline beatgrid are pure `Float32Array` math fed off the wire *before* the
  audio graph (`useDeck.ts` pushes PCM to analysis, then to the player), so the
  death of Web Audio does not force them to move — they read from the wire or
  engine telemetry instead. That roughly halves the rewrite surface.
- Harder: we lose the "open it in Chrome" dev loop and the browser verify corpus
  (`verify-worklets` and the `verify_m*.mjs` Playwright/Node scripts that are the
  acceptance evidence for the audio-path milestones); the app becomes
  native-only to run. The Rust process becomes both real-time-critical and the
  supervisor of the Python sidecars. Required follow-up: a native verify story —
  Rust integration tests that measure underruns, output parity against the Web
  Audio reference, and bit-exact bypass — re-homing the exit criteria of
  ADR-0008/0009/0013/0014 onto the native engine; the UI keeps the browser dev
  loop, but audio-path work is verified natively.
- New dependencies to justify (per `.claude/rules/security.md`): `cpal`
  (CoreAudio I/O), `rtrb` (wait-free SPSC ring), `fundsp` (the DSP graph), and
  `rubato` (resampling/varispeed) are reputable, maintained, and would be pinned;
  `fundsp` carries the DSP premise, so its coverage is *verified in the spike*,
  not assumed. Keylock's binding (`ssstretch` v0.1, over MIT Signalsmith Stretch)
  is immature and explicitly gated on the binding reaching maturity — varispeed
  ships on `rubato` until then.
- Accepted risk and gate: glitch-free real-time output under the Python PCM feed
  must be proven before acceptance. **Acceptance is gated on a spike** that
  stands up `cpal` + `fundsp` against two live decks, **selects the PCM transport
  channel per ADR-0019** (jitter and throughput across the candidate channels),
  and measures underruns, latency, a click-free FX swap,
  **FX parity and the ADR-0008 bit-exact bypass, and the M17 limiter ceiling**
  (the measured 0.9297 ceiling and makeup-gain cancellation) — not just timing.
  Migration is **not** "deck by deck": one shared master bus and clock domain
  make that impossible. It is sliced by capability, each slice parity-checked
  against the Web Audio reference — (1) two-deck silence-to-output over the
  transport; (2) bare mix: player rings + EQ + crossfade + limiter; (3) the Color
  FX insert with the bypass parity test; (4) freeze/loops/track sources +
  varispeed; (5) native cue routing.
- Depends on the native-shell decision (ADR-0018) and the Python↔Rust PCM
  transport (ADR-0019); this one assumes both. On acceptance it supersedes
  ADR-0003 (Web Audio mixing) and also ADR-0006 and ADR-0007, whose second-sink
  and backend cue workarounds the native `cpal` routing replaces — until then all
  three stay Accepted. ADR-0018's first ship is gated on this engine: the native
  app does not ship on Web Audio.

## Alternatives considered

- **Keep Web Audio, ship in Electron** - Electron bundles Chromium, so the whole
  ADR-0003 engine keeps working untouched and the app still ships native. The
  conservative path, and the right one if packaging is the only goal. Not chosen
  here because it leaves LSDJ on Web Audio's ceiling — no keylock or
  time-stretch, the two-workaround cue path stays — and locks in a heavier
  Chromium runtime.
- **Keep Web Audio in WKWebView, mitigate with a spike** - cheapest if it
  passes, but it preserves the capability ceiling and the cue split and bets the
  core feature on WKWebView's weakest web APIs. Rejected: it spends risk to keep
  the thing we want to move past.
- **Mix in the Python backend** - ADR-0003's original rejected alternative;
  still pays a control round-trip on every fader gesture and adds an audio stack
  to Python. A native Rust engine in the shell gets device routing without the
  round-trip. Still rejected.
- **Hand-rolled Rust DSP instead of `fundsp`** - maximal control and no graph
  abstraction to fight, but reimplements filters, FDN reverb, and a look-ahead
  limiter that `fundsp` already ships under MIT/Apache-2.0. Deferred; drop to
  hand-rolled DSP only where a specific effect outgrows the library.

<!-- Status values: Proposed | Accepted | Rejected | Deprecated |
     Superseded by ADR-NNNN -->
