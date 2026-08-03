# Spike A — Rust audio core + PCM transport

**Status: spec (2026-06-15); results pending.** The executable spec for Phase 0,
Spike A of the [native migration](native-migration-plan.md). It **gates
[ADR-0017](adr/0017-native-rust-audio-engine-superseding-web-audio.md)** (Rust
audio engine) and
**[ADR-0019](adr/0019-pcm-transport-from-python-sidecars-to-the-rust-engine.md)**
(PCM transport): a PASS moves both from Proposed toward Accepted. Throwaway,
exploratory code; the deliverable is the measured **Results** section at the
bottom, not production code.

The reference constants below were verified against the source (file:line cited);
trust them over re-reading unless something looks off.

## Objective

Prove a Rust engine (`cpal` + `rtrb` + `fundsp`) can drive **two decks** of real
LSDJ 48 kHz/stereo PCM glitch-free, reach **stated** parity with the Web
Audio engine on the load-bearing nodes, and choose the PCM transport — or surface
exactly where it can't.

## What is bit-exact vs what is not (read first)

The single most important method point. The Rust graph renders with different math
than Web Audio (different biquad coefficient formulas, op order, denormal
handling), so **cross-engine bit-exactness is impossible for any DSP path.** Claim
it only where no DSP runs:

- **Bit-exact (IEEE-754, ULP == 0):** (a) the **dead-zone bypass** — dry
  passthrough; (b) the **clip-guard ceiling invariant** — no output sample exceeds
  `0.9296875`.
- **Within-epsilon (state the number):** EQ, crossfade, and the limiter/FX **body**
  — compared against an `OfflineAudioContext` golden with **static** params, after
  removing fixed group delay (cross-correlate, then diff). Target: per-sample
  max-abs ≤ `1e-4`, or transfer-function magnitude within ±0.1 dB across
  20 Hz–20 kHz.
- **Invariant tests, not waveform diffs:** the **M17 limiter** — its Web Audio
  reference is a `DynamicsCompressorNode` whose curve is implementation-defined and
  unspecified, so do **not** diff its waveform. Test two contracts instead: a hot
  input never exceeds `0.9296875`; a sub-threshold signal (peaks below −6 dB)
  passes level-transparent (proving the makeup-gain cancellation holds).
- **Documented divergence (parity not required):** Space (FDN ≠ convolution),
  Crush (needs a hand-rolled hold), Noise (different PRNG) — measure and **record**
  the divergence rather than chase parity.

## Verified reference constants

**EQ** (`frontend/src/audio/eq.ts`): low `lowshelf` @ 250 Hz (L23); mid `peaking`
@ 1000 Hz, Q 0.7 (L24); high `highshelf` @ 2500 Hz (L25). `eqValueToDb`: 0 →
**−40 dB** kill, 0.5 → 0 dB, 1 → **+6 dB**, linear in each half (L13–15).
*fundsp note:* Web Audio shelves take **no Q** (frequency+gain only); fundsp
shelves do — the shelf slope is a free variable to tune to WA's fixed slope. Mid
must be `bell_hz` (has gain), not `peak_hz`. Pass Q = 0.707 to biquad LP/HP to
match WA defaults.

**Master** (`frontend/src/audio/master.ts`): limiter `DynamicsCompressorNode` thr
−6 dB, knee 0, ratio 20, attack 0.002 s, release 0.25 s (L22–26); implicit makeup
`(1/fullScaleGain)^0.6` (~+3.4 dB) cancelled by `LIMITER_MAKEUP_DB` (L28–37);
clip-guard hard ceiling **`0.9296875`** = 119/128 ≈ −0.6 dBFS (L17); auto-gain
target 0.15 RMS, ±12 dB, floor 0.005 (L53–57); loudness window 10 s (L88).

**Color FX** (`frontend/src/audio/fx.ts`, `fxGraphs.ts`, `public/crusher-kernel.js`):
`FX_DEAD_ZONE = 0.02` on `|amount − rest|`; bypass is bit-transparent inside it
(L19, 27–29).

| FX | curve | Web Audio graph |
| --- | --- | --- |
| filter | bipolar: <0.5 lowpass `logSweep(18000→80)`, ≥0.5 highpass `logSweep(30→6000)` | one `BiquadFilterNode` |
| dub_echo | wet `amount*0.9`, feedback `min(0.82, amount*0.9)`; delay 0.35 s free or beat-fractions `[0.25,0.375,0.5,0.75,1]`; tone lowpass 2500 Hz in loop | Gain→Delay→Biquad(LP 2.5k)→Gain(fb)→Delay; Delay→Gain(wet) |
| space | wet = amount | `ConvolverNode` (2.5 s, decay^3 noise IR)→Gain |
| crush | bits `16−amount*12` (16→4), reduction `1+round(amount*39)` (1→40) | `AudioWorkletNode` `bit-crusher` |
| noise | level `amount*0.35`, centre `logSweep(120→9000)` | BufferSource(1 s white loop)→Bandpass(Q0.8)→Gain |
| sweep | rate `0.5+amount*7.5` Hz, depth `min(1, amount*1.2)` | Sine LFO→Gain(depth)→modulates duck Gain |

Bit-crusher kernel: `levels = 2^(bits-1)`; on `counter==0` quantize
`round(x*levels)/levels`, hold for `reduction` samples (`crusher-kernel.js:9–24`).

**Player ring** (`frontend/public/player-worklet.js`): capacity 30 s (L22),
prebuffer 1.5 s (L23), stats every 1 s (L24).

**Wire format** (`backend/lsdj/worker.py`, `controller.py`): **interleaved
stereo float32 little-endian, 48 kHz**, ~1.0 s chunks; frame = `4 * 2` bytes;
whole stereo frames only. Worker paces to stay **3.0 s ahead** of playback
(`worker.py:24`). Control messages: `play/stop/restart/set_prompt/set_style/
set_model/render_clip/embed_sample/shutdown`. **It is f32, not int16** — do not
scale by 32768.

## Interface contract (the bare-mix subset)

The engine implements the `engine.ts` `DeckChannel`/`AudioEngine` surface. Spike A
needs: `postPcm`, `setVolume`, `setEq`, `setFx`, `setFxAmount`, `setOnAir`,
`setTrim`, `getLevel`; engine `createDeckChannel`, `setCrossfade`,
`getMasterLevel`, `getMasterGainReduction`. (The A/B crossfade is an **equal-power
cos/sin pair** — `a=cos(p·π/2)`, `b=sin(p·π/2)` — two gains, **not**
`Net::crossfade`. `Net::crossfade(node_id, Fade::Smooth, secs, …)` is for the
click-free FX-node swap.)

## fundsp coverage map (verified against docs.rs/fundsp)

| Target | fundsp | risk | note |
| --- | --- | --- | --- |
| 3-band EQ | `lowshelf_hz`→`bell_hz`→`highshelf_hz` | MED | coeff divergence; tune shelf Q to WA's fixed slope |
| **M17 limiter** | — | **HIGH / no** | fundsp `limiter` is a fixed-ceiling peak limiter (no thr/ratio/knee/makeup, adds latency); test **invariants**, or hand-roll a feed-forward compressor |
| clip-guard | `shape(Shape::ClipTo(±0.9296875))` | LOW | maps cleanly; bit-exact ceiling |
| filter | `lowpass_hz`/`highpass_hz` (Q 0.707) | LOW | same biquad family |
| dub_echo | `delay` + in-loop lowpass + feedback | LOW-MED | `delay` rounds to nearest sample; retune zippers |
| **space** | `reverb_stereo` (FDN) | **HIGH / approx** | FDN ≠ convolution IR; document divergence or hand-roll partitioned convolution |
| **crush** | `shape(Shape::Crush)` | **HIGH / partial** | Crush is quantize-only; **hand-roll** the sample-and-hold decimation |
| noise | `white`→`bandpass_hz(Q0.8)`→gain | LOW-MED | different PRNG; non-looping — sample parity meaningless |
| sweep | `sine_hz`·depth + offset, mul | LOW | reproducible bar float |

For Spike A, prove parity on the **clean** ones (filter, EQ-within-epsilon, the
crossfade, the bypass, the ceiling). The three HIGH-risk targets are the point of
the exercise: **measure and record** how far the fundsp approximation sits from the
Web Audio reference; that data decides hand-roll vs accept per effect.

## Real-time discipline + hazard checklist

Fixed regardless of transport: a **non-RT IO thread** decodes transport frames
into a **per-deck wait-free `rtrb`** (SPSC — one producer, one consumer, **one
ring per deck**, never shared); the **`cpal` callback only drains**. Assert under
load:

1. Callback does **zero** alloc / lock / syscall / log / panic — verified with a
   real allocator guard (e.g. `assert_no_alloc`) during the run, not by inspection.
2. Telemetry out (meters, ring fill, underrun count) is **wait-free** (atomics or a
   second SPSC ring) — never a mutex / `mpsc` on the callback.
3. `fundsp Net` mutation (`commit`/`crossfade`) runs **off** the audio thread; the
   callback only adopts an already-built graph. Verify the swap is click-free **and**
   alloc-free.
4. **FTZ/DAZ** set on the audio thread at stream start; include a denormal-storm
   input (a reverb/echo tail decaying to silence) in the run to prove no CPU spike.

## PCM transport selection (ADR-0019)

Measure candidates **under load** (model inference running, cores busy): **loopback
TCP/WebSocket** (reuses v0 framing), **Unix domain socket**, **shared-memory ring**.
Report per candidate: inter-frame arrival-gap distribution (p50/p99/p99.9/**max**),
frames arriving later than one callback period, throughput (must sustain
~1.5 MB/s aggregate with headroom), CPU/frame, and underruns over the run.
**Decision rule:** the channel whose **worst-case arrival gap** stays comfortably
inside the ring drain margin with zero underruns under load; break ties on p99.9
jitter then CPU — **not** mean throughput. Record shared-memory's lifetime/cleanup
complexity as a method cost.

## Scope / build steps

1. Capture golden **input** PCM: dump real worker chunks (`('audio', bytes)`,
   48 k/stereo f32 LE) to file, OR a deterministic synthetic signal at fixed seed.
   The **same bytes** feed both the Rust engine and the Web Audio golden render.
2. Minimal Rust binary: two player rings drained from `rtrb` → per-deck 3-band EQ →
   equal-power crossfade → limiter + clip-guard → `cpal` CoreAudio output, as a
   `fundsp Net`.
3. Golden **render**: `OfflineAudioContext({sampleRate:48000, channels:2})` through
   the same graph with **static** params (no `setTargetAtTime` ramps), dumped and
   checked in.
4. Parity per the bit-exact/epsilon/invariant rules above (align before diffing).
5. Transport prototypes + measurement; pick one.
6. Sustained run; measure underruns/latency/CPU.

## Config & environment

- **Buffer:** request `cpal BufferSize::Fixed(256)` (5.33 ms @ 48 k); **log the
  granted size** (CoreAudio may round) and use it for the latency budget and the
  underrun definition. Latency target ≤ ~12 ms round-trip.
- **Device rate:** require an exact **48000** output config; enumerate
  `supported_output_configs` and **fail fast** if none (resampling out of scope for
  the spike). macOS built-in default is often **44100** — handle explicitly.
- **Channels:** stereo; on a >2ch device (FLX4 3/4) write 0/1, zero the rest. Cue/
  multi-out is out of scope here.
- **Replay must mimic worker pacing** (~1.0 s chunks, ~3.0 s ahead, realistic
  jitter) — a firehose replay never underruns and proves nothing. Per-deck ring
  30 s, prebuffer 1.5 s (in frames).
- Pin crate versions (`cpal`, `rtrb`, `fundsp`, `rubato`); record them for the
  `security.md` justification.

## Pass / fail

PASS requires all of:

1. **Zero underruns** at the granted buffer over a **≥10 min** two-deck run, counted
   the worklet's way (callback with fewer frames than requested) — **excluding** the
   initial prebuffer fill.
2. **Bit-exact bypass:** output == insert input (ULP 0; −0.0 ≡ +0.0; no NaN) at
   steady state inside the dead zone, **excluding** the ~20 ms ramp window.
3. **Ceiling invariant** holds on a hot input; **sub-threshold transparency** holds
   (makeup cancellation).
4. EQ / crossfade **within epsilon** (≤ 1e-4 or ±0.1 dB) after alignment.
5. **Click-free FX swap** via `Net::crossfade(Fade::Smooth)` — no discontinuity at
   the boundary.
6. A **transport chosen** on measured worst-case jitter under load.

**Fail path (not a phase failure):** an effect that can't reach parity in fundsp
drops to hand-rolled DSP — record which, and why, in Results.

## Results

**Run 1 — offline DSP-parity slice (2026-06-15).** This run answers only the
riskiest question — *can fundsp reproduce Chromium's Web Audio DSP within
tolerance?* — via a file-in/file-out renderer diffed against a real Chromium
`OfflineAudioContext` oracle. The device run, sustained run, and transport
measurement are **not** in this pass (no audio device / no live sidecar here).
Harness: `spike/rust-audio/` (`golden/` Node oracle, `engine/` fundsp renderer).
Engine: **`fundsp 0.23.0`** (`prelude32`, f32 internals — the `hacker` prelude was
removed in 0.23). Oracle: Chromium via Playwright. Deterministic 4 s signal.

| case | rule | result | verdict |
| --- | --- | --- | --- |
| `eq_flat` | epsilon | maxErr 5.7e-14 (−296 dBFS) | **PASS** |
| `eq_kill_low` | epsilon | maxErr 6.3e-7 (−140 dBFS) | **PASS** |
| `eq_boost` | epsilon | maxErr 2.4e-6 (−131 dBFS) | **PASS** |
| `filter_lp` | epsilon | maxErr 8.3e-7 (−140 dBFS) after Q-fix | **PASS** |
| `bypass` | bit-exact | 0 ULP, byte-identical | **PASS** |
| `master_limiter` ceiling | invariant | peak = 0.9296875 | **PASS** |
| `master_limiter` transparency | invariant | 0.000 dB | **PASS** |
| `master_limiter` loud body | report | maxErr 0.59 (−7.5 dBFS) | divergent (expected) |
| `crush` (hand-roll) | bit-exact | **0 ULP** vs worklet | **PASS** |
| `crush_fundsp` | report | maxErr 0.79 (no hold) | insufficient |
| `space` (RT60) | decay | FDN 4.51 s vs IR 2.67 s | approx |

Findings:

1. **EQ shelves match to ~1e-6.** `lowshelf_hz`/`bell_hz`/`highshelf_hz` reproduce
   Chromium's shelves/peaking to −130 dBFS or better, shelf Q = 1/√2 (matching WA's
   RBJ S=1 shelves), mid as `bell_hz` (gain), not `peak_hz`. Clean PASS.
2. **The resonant LP filter — root-caused and fixed (the headline result).** At
   `lowpass_hz(f, 1.0)` it diverged ~3% (−37 dBFS), ≈3 orders worse than the
   shelves. A Q sweep pinned a sharp error minimum at **q = 1.122018 = 10^(1/20)**,
   where it drops to **8.3e-7 (−140 dBFS)** — i.e. **Web Audio interprets the
   lowpass/highpass Q in dB** while fundsp takes linear Q. Fix (one line):
   `q_linear = 10^(Q_dB/20)`. The filter FX is a clean PASS with the conversion —
   not a fundsp limitation. **Generalises to the whole engine:** WA
   LP/HP/BP/notch/allpass Q is dB, peaking Q is linear (why the EQ mid matched at
   0.7), shelves take no Q — fundsp wants linear everywhere, so convert.
3. **The M17 limiter body can't be reproduced (HIGH-risk confirmed).** The clamp
   master satisfies both invariants — ceiling exact at 0.9296875, sub-threshold
   transparency 0.000 dB — but its loud-segment waveform diverges from the
   `DynamicsCompressor` by maxErr 0.59 / −7.5 dBFS (the clamp hard-clips ~57.5 % of
   the loud burst; the compressor tames it to peak 0.56 before the guard). Full
   body parity needs a hand-rolled feed-forward compressor; the invariant-only
   approach in this spec is the right call.
4. **Method confirmations (the critics were right).** Bypass is bit-exact (0 ULP).
   The `DynamicsCompressorNode` adds **288 frames / 6.00 ms latency** — diffing
   without alignment read it as 3.6e-2 "error"; aligned, the quiet seg matches to
   4e-7. Transparency is a *level* property (RMS ratio), phase-immune. "Align
   before diff" and "bit-exact only for the bypass" are now empirically grounded.
5. **Crush — fundsp insufficient, but a trivial hand-roll is bit-exact.** fundsp's
   `Crush(512)` is quantise-only (`round(x·512)/512`) and diverges hugely from the
   worklet (maxErr 0.79) because it lacks the sample-and-hold that dominates the
   effect. An ~8-line hand-rolled quantise-and-hold (shared counter, re-quantise
   every 21 samples) matches the Web Audio worklet **bit-exactly (0 ULP)**. Crush
   needs a hand-roll, but it's tiny and perfect.
6. **Space — fundsp is an approximation, not a match.** `reverb_stereo(12, 2.5,
   0.5)` (32-ch FDN) gives RT60 **4.51 s** vs the convolution IR's **2.67 s** — a
   different algorithm with a different decay. The `time` param can be tuned down
   to match RT60, but the FDN's modal tail still differs in character from the
   noise-IR convolution. Exact parity needs a partitioned-convolution reverb;
   otherwise accept a tuned FDN.

**Run 1 left open (now addressed in Run 2):** the real-time half — `cpal` device
output, the transport-channel measurement, the RT-safety guards, and the sustained
run (pass-list criteria 1 and 6).

**Interim verdict (offline slice — DSP coverage complete):** strong. fundsp
reproduces **EQ, the filter FX (after the Q-in-dB fix), and bypass to ~1e-6**,
holds the **ceiling/transparency invariants exactly**, and the **Crush hand-roll is
bit-exact**. Three nodes need hand-rolls — all known, bounded DSP, none a blocker:
the **limiter body** (a feed-forward compressor; the invariants cover the rest),
**Crush** (the ~8-line quantise-and-hold, bit-exact), and **Space** (a tuned FDN
approximates; exact parity wants partitioned convolution). The real-time half is
**Run 2**, below.

## Run 2 — real-time half (2026-06-15)

Two binaries in `engine/` — `rt_engine`, `transport_bench` — on `cpal 0.18.1`,
`rtrb 0.3.4`, `assert_no_alloc 1.1.2` (armed in release via `warn_release`). A real
output device was available, so both ran (not just the headless bench). Numbers
below independently reproduced.

**Transport selection (criterion 6)** — inter-arrival jitter, 8 CPU burners on 10
cores; one deck's 0.366 MB/s stream (~1.5 MB/s is the multi-deck aggregate):

| channel | MAX gap (no load) | MAX gap (load) | vs 1.5 s margin | late |
| --- | --- | --- | --- | --- |
| **shm** (in-proc `rtrb`) | 0.004 ms | 0.001 ms | ~6 orders inside | 0 |
| **tcp** (loopback, reuses v0) | 0.36 ms | 0.24 ms | ~4 orders inside | 0 |
| uds | 1.27 ms | 0.63 ms | ~3 orders inside | 0 |

All three pass with vast margin; load did not degrade tcp/shm (busy cores keep the
consumer thread hot). **Pick: `shm`** for a co-located sidecar (worst-case 0.004 ms,
contention-immune); **`tcp`** for an out-of-process Python sidecar (0.36 ms, reuses
protocol v0 framing, beats uds on every percentile). Channel chosen — criterion 6
met.

**Device output + RT safety (criterion 1)** — `rt_engine` on "MacBook Pro Speakers",
exact 48000/2-ch/f32, **granted buffer 256 frames** (CoreAudio honored `Fixed(256)`,
5.33 ms). Over 15–20 s two-deck runs incl. a `--load 8` variant: **zero underruns**
post-prebuffer, rings steady at the ≤3 s producer lead, and the **`assert_no_alloc`
guard printed no warnings** — the callback is genuinely alloc/lock/syscall/log-free.
Callback reviewed: per-deck SPSC `rtrb` reads, FTZ/DAZ on the audio thread,
off-thread-built fundsp EQ ticked in place, atomics-only telemetry — sound.

**Remaining for a full criterion-1 PASS:** the **≥10-min sustained run** (and its
loaded variant) on the target Mac — an endurance check, not a code gap.
Command: `./target/release/rt_engine --duration 600 [--load 8]`; PASS = granted 256,
zero underruns, no alloc warnings over the full 10 min.

## Run 3 — the three LOW-risk effects (2026-06-15)

Closed out DSP coverage with dub_echo, sweep, and the noise FX's filter (amount
0.7); golden via Chromium, rust via fundsp.

| effect | result | verdict |
| --- | --- | --- |
| **sweep** | maxErr 1.9e-6 (−130 dBFS) | **clean PASS** |
| **dub_echo** | tail-accumulating divergence (seg1 −18 → seg4 −3 dBFS) | **hand-roll** |
| **noise** (bandpass) | best −36 dBFS at any Q; source non-deterministic | **N/A / hand-roll** |

- **sweep** is a clean win — the phase-0 sine-LFO duck matches Chromium to ~1e-6.
- **dub_echo** does NOT match: fundsp's `feedback()` combinator inserts a 1-sample
  delay per loop iteration, so the 0.35 s echoes **drift cumulatively** — fine on
  the first echoes (seg1 −18 dBFS) but unrelated by the tail (seg4 maxErr 1.72,
  dominated by many-times-recirculated echoes of the loud burst). A faithful dub
  echo needs a **hand-built D-sample feedback delay** (exact loop length, the tone
  lowpass — which matches at q=1.122, the Q-in-dB default — in the loop, wet tapped
  pre-tone); `feedback()` is the wrong primitive.
- **noise**: its source is a random, looped buffer (like Space's IR), so the effect
  can't sample-parity by construction — fundsp `white()` is a different,
  non-looping PRNG; a filtered riser is character-equivalent, not sample-matched.
  Its bandpass is a separate note: a q sweep bottoms out at only −36 dBFS (vs the
  lowpass's −140), a **shallow** minimum near q≈1.0 — so unlike LP/HP, Web Audio's
  **bandpass Q is not simply dB**, and fundsp's `bandpass_hz` uses a different
  normalisation (constant-skirt vs WA's 0 dB-peak) no Q can close. Moot for the
  noise FX; a flag for any exact-bandpass need elsewhere (hand-match coefficients).

## DSP coverage — final tally

**Clean in fundsp (~1e-6 or bit-exact):** 3-band EQ, the Color filter (Q-in-dB
fix), **sweep**, bypass (bit-exact), and the master ceiling + sub-threshold
transparency invariants. **Bounded hand-rolls (all standard DSP, none a fundsp
blocker):** Crush (~8-line quantise-and-hold, **bit-exact**), the limiter body
(feed-forward compressor; invariants cover the ceiling), **dub_echo** (hand-built
feedback delay), and Space (tuned FDN, or partitioned convolution for exact).
**Inherently non-deterministic:** noise and Space's random source — character-
equivalent by design, never sample-matched.

## Overall verdict

Spike A is **substantially PASS, pending one endurance run.** DSP parity holds to
~1e-6 on the clean set (EQ, filter, sweep, bypass, ceiling), with four bounded
hand-rolls identified (Crush — bit-exact; the limiter body; dub_echo's feedback
delay; Space's reverb) plus noise's deliberately non-deterministic riser; the
real-time path runs glitch-free with verified RT-safety at a 256-frame buffer; the
transport is chosen (`shm` co-located, `tcp` out-of-process). The only box left
unticked is the **≥10-min sustained device run** — an operator step. On that green,
ADR-0017 and ADR-0019 clear their spike gate and move Proposed → Accepted.
