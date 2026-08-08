# MCP co-DJ — findings & next steps

Working notes from testing the LSDJ MCP server (`src-tauri/src/mcp.rs`) by
driving it as a co-DJ from Claude Code. Branch: `mcp-improvements`. Uncommitted
working notes — commit or fold into issues as you see fit.

## Status

- **DONE (2026-08-07 session 2) — fixes landed AND live-verified** (schemas
  inspected on the wire; eject round-trip, structured `set_style`, 6-way
  concurrent `list_songs` in 13 ms, and a "Velvet Echo.wav" generation all
  exercised against the rebuilt app). Uncommitted on `mcp-improvements`:
  - **#12** `$defs`/`$ref` inlined into tool schemas at router construction
    (`inline_local_refs` in `mcp.rs`; unit-tested) — `set_style` and the enum
    params become client-usable.
  - **#2** `SongLibrary::list`/`SampleLibrary::list` no longer write on read
    (save only when the reconcile changed something) and the MCP tools run
    library scans under `spawn_blocking` (`scan_library`).
  - **#3** new `eject` tool (routes the webview's `leavePlayback`);
    `deck_play`/`deck_stop` are mode-aware — on a playback deck they route to
    the track transport (webview `play()`/`stop()` via new `play`/`stop`/`eject`
    deck-commands).
  - **#4/#6** `mcp://generation` start/done/error events (job-keyed); the Media
    Explorer mirrors the in-app pending row (spinner) for agent generations.
  - **#7** MCP generations now get a two-word `pleasant_title()` (the Rust
    counterpart of `randomSongTitle()`), fixing raw-prompt titles AND filenames
    for both tracks and samples.
  - **#11** `set_on_air` description documents the off-air auto-prime.

- **DONE — #1 `get_state` observability tool.** Added a read-only `get_state`
  MCP tool that returns the `InterfaceStore` snapshot as JSON (both decks,
  mixer, EQ, FX, style pads, cues, transport, models) — the same data as the
  `lsdj://interface-state` resource, but reachable by a **tools-only** MCP
  client (Claude Code surfaces tools, not resources, to the agent loop). Server
  `instructions` updated to point agents at it. `cargo check` clean; live
  rebuilt binary verified to contain it.
  - Caveat: a newly-added MCP tool only appears after the **client re-lists**
    tools. Reconnect the `lsdj` server (CLI: `/mcp` → lsdj → reconnect) or start
    a fresh Claude Code session. Port `51639` and the bearer token persist, so
    nothing else needs redoing.

- **DONE (2026-08-07 session 3) — playback mode + generation exercised live.**
  Full pass: `load_track` (library track onto A), mode-aware `deck_play`/
  `deck_stop` ("resuming/pausing its loaded track"), `generate_track` (45 s
  "Cosmic Cascade" onto B, auto-load + pleasant title + registered in the
  library), `set_tempo` varispeed (130→120 BPM = rate 0.923, exact),
  `set_hot_cue`/`jump_to_hot_cue`, `beat_loop`, `seek_track`, and `eject` from
  both a playing and a paused deck (both hand back to the realtime stream).
  New findings #13–#14 below; #8 got a timing data point.

- **DONE (2026-08-07 session 3, part 2) — fixes #5/#10/#13/#14 + MCP LoRA
  support, all landed and live-verified except the LoRA generation itself:**
  - **#10** `ramp_ms` (optional, 0–60 000) on `set_crossfade` / `set_volume`:
    a linear glide ticked per frame on the RT path (`Ramp` in `graph.rs`); the
    crossfade ramps the POSITION and re-applies the equal-power law each frame,
    so a glide holds constant power throughout. Plumbed Host → Command →
    Engine; UI/MIDI paths unchanged (instant). Unit-tested (monotonic glide +
    exact landing); heard live (3 s crossfades, 2 s volume fades — no steps).
    `set_eq` already glides (`follow()`); `set_fx_amount` still steps — a ramp
    there needs the FX params made audio-rate (bigger refactor, not attempted).
  - **#13** `TransportSnap.playing` (serde-default false): the webview mirrors
    the playback transport's play/pause up immediately (bypasses the 250 ms
    throttle like rate/loop changes); `get_state` documents top-level `playing`
    as realtime-only. Verified live: True at playhead 2.9 s, False on stop.
  - **#5** new `toggle_pad(deck, slot)` tool — the webview runs the exact
    pad-press gesture (`toggleLoopPad` via a new `pad` deck-command): filled
    slot plays/stops (loops layer, one-shots fire), EMPTY slot captures a
    freeze. Slot count validated against the store's `loopLabels`. Verified
    live (fired the "Jungle" one-shot; slot 9 rejected with a clear message).
  - **#14** descriptions updated: seek releases the beat loop (`seek_track` +
    `beat_loop`), `sync_deck` notes the `bpm: null` library-track caveat.
  - **LoRA support (user request):** new `list_loras` tool (installed SA3
    adapters via `loras::discover`, under `spawn_blocking` per #2) and an
    optional `loras: [{name, strength}]` stack on `generate_track` /
    `generate_sample`, forwarded to `/api/generate` (magenta engine rejects it
    with a pointer). Wire contract unit-tested. NOT yet fired end-to-end
    against the backend — see "What's left".

- **DONE (2026-08-08 session 5) — the two reasons the show-night agent had to
  script around MCP, fixed. Live-verified same day via raw JSON-RPC (see the
  session-5 verification entry below):**
  - **#15 Option params schema fix** — session 4's live failure: schemars 1.x
    emits `Option<…>` params as draft-2020-12 nullable shapes
    (`"type": ["number", "null"]` for `ramp_ms`, `["array", "null"]` for
    `loras`, an `anyOf`+null for `set_notes.mode`), and the Claude Code
    harness drops array-valued `type`s/`anyOf` wrappers — the param surfaces
    untyped, the model sends a JSON *string*, server serde rejects it.
    `inline_local_refs` became `normalize_tool_schemas`: refs inline as
    before, then `strip_null_variants` removes the null variants (optionality
    already lives in `required`; serde accepts null regardless). Router-wide
    regression test asserts no client-hostile shape survives on any tool.
  - **#8 async generate_track** — `generate_track` now returns immediately
    with a job id (+ ETA at ~2.3 s audio/s) and spawns the
    generate→save→load work; results land in a Tauri-managed `GenerationJobs`
    registry (app-wide, survives MCP session reconnects; running jobs never
    evicted). New no-arg `generation_status` tool reports every job newest
    first (running/done/failed, elapsed seconds, result message). The
    `mcp://generation` UI events fire exactly as before. The 380 s server cap
    is now reachable through any MCP client.

- **Session-5 verification (2026-08-08, same day, raw JSON-RPC against the
  rebuilt app):**
  - Wire schemas: all 36 tools clean — no `$ref`/`$defs`, no nullable type
    arrays, no Option `anyOf` anywhere; `ramp_ms` serves as `"number"`,
    `loras` as `"array"`, `set_notes.mode` as a typed enum.
  - `set_crossfade` with a *typed numeric* `ramp_ms` accepted ("crossfade
    gliding to 0.5 over 2000 ms").
  - `generate_track` returned in <1 s with the job id; `generation_status`
    tracked running (elapsed counting) and kept answering while the job ran
    past the old 60 s client-timeout mark; a backend failure surfaced as
    `status: "failed"` with the server's detail string.
  - **#16 (found by the LoRA live-fire): the backend's SA3 deadline didn't
    budget the LoRA merge.** `sa3.timeout_for` was `120 + seconds`; the CLI
    merges adapter deltas into the DiT at load — measured **127.7 s flat**
    for one adapter on the medium DiT (12.6 s sample, 148 s total for a 60 s
    track, peak 5.68 GB) — so any LoRA track died at the deadline ("502:
    generation timed out after 180s"). Fixed: `LORA_TIMEOUT_SECONDS = 300`
    flat allowance when a stack rides (the merge cost is dominated by the
    base-DiT dequant, not per-adapter), and the MCP ETA adds a matching
    ~130 s so the agent isn't promised 26 s. The direct CLI run confirmed the
    adapter itself works — a clean 60 s chiptune WAV. Re-fired in-app after
    the fix: job `done` in 215 s wall (memory pressure beside the loaded
    Magenta models; inside the new 480 s deadline), "Velvet Reverie"
    auto-loaded onto deck 1 in playback mode, analysed at **140.0 BPM** — the
    prompt's requested tempo. The LoRA live-fire Jake asked for is done.
  - The remaining session-4 leftovers are now tracked as issues: #124
    (`set_fx_amount` ramp), #125 (idle -32001), #126 (`mainDevice: ""` —
    reproduced live this session).
  - Agent skill added at `.agents/skills/lsdj/SKILL.md` (served to Claude
    Code via the `.claude/skills` symlink). Named `lsdj`, not `co-dj` — the
    skill covers both solo driving and working beside the human ("Who's
    driving?" section). Bundled prompting references distill the measured
    model knowledge: `references/magenta-realtime-prompting.md` (weighted
    style blends, ADR-0004's tempo-is-not-steerable evidence) and
    `references/stable-audio-prompting.md` (descriptor stacks, BPM/key
    adherence, LoRA interplay, timing budgets).

## What's left (next session)

1. **Harness-side proof:** a *fresh Claude Code session* (so the client
   re-lists tools) calling `set_crossfade{ramp_ms}` and
   `generate_track{loras}` through the MCP tools — the session-4 failure was
   harness stringification, which raw JSON-RPC can't exercise. Expected to
   pass now that the schemas carry plain types; this is the PR checklist item.
2. **#10 remainder** — now issue #124 (`ramp_ms` on `set_fx_amount`).

### Workflow gotchas (learned the hard way, session 3)

- **The webview serves `frontend/dist`, not a vite dev server** (no `devUrl`
  in `tauri.conf.json`): frontend changes need `npm run build` in `frontend/`
  before an app restart picks them up. Symptom otherwise: Rust-side changes
  live, webview behaviour stale (#13 looked unfixed until the bundle rebuild).
- **Don't run `cargo check`/`test` while `cargo tauri dev` is rebuilding** —
  overlapping builds in the shared `target/` corrupted incremental state twice
  (undefined `lsdj_app_lib` hash symbols at link). Fix: `cargo clean -p
  lsdj-app`, `rm -rf target/debug/incremental`, touch a source file.

## How to resume in a new session

1. A fresh session lists the current tools (incl. `eject`, typed `set_style`);
   in a *continuing* session the client holds a stale list until the `lsdj`
   server reconnects (finding #9). Raw curl to the loopback port works around
   it (token/port under `~/Library/Application Support/works.protocol.lsdj/`).
2. Call `get_state` first to observe; verify the read-back matches the UI.
3. Remaining open items: see "What's left (next session)" above — the LoRA
   live-fire, **#8** (async generation job), the `set_fx_amount` ramp, and the
   co-DJ SKILL.md. The sections that follow keep the *fixed* items too (for the
   mechanism/why); each is marked.

## Prioritized next steps

### #2 [FIXED, session 2] `load_track` / `list_songs` stall & time out (-32001) under concurrency — HIGH
- **Evidence:** a batch of `load_track` + `set_volume` + `set_eq` fired together
  → `load_track` timed out while the store-backed calls returned instantly;
  retried alone → instant. `list_songs` also timed out once right after the app
  restart (disk busy).
- **Root cause:** `SongLibrary::list()` (`src-tauri/src/songs.rs`) — called by
  `load_track` AND `list_songs` — takes a `std::sync::Mutex`, does blocking fs
  I/O, and **writes the registry on every read** (`save_registry`), all on the
  tokio runtime with no `spawn_blocking`. Concurrent callers (two loads, or the
  folder watcher's own reconcile) serialize on the mutex and block a tokio
  worker past the MCP client timeout. `SampleLibrary::list()` likely shares the
  pattern.
- **Fix:** don't write-on-read (reconcile/save only when the dir changed, or
  cache); and/or move the blocking work to `spawn_blocking`; and/or an
  async-aware lock. Then re-test concurrent `load_track`.

### #3 [FIXED, session 2] No path back to realtime (Magenta live) mode; `deck_play` is mode-blind — MED/HIGH
- The store has per-deck `mode` (`realtime`|`playback`); the webview's
  `unloadTrack()` (`frontend/src/deck/useDeck.ts`) flips a deck back to
  `realtime`. But MCP exposes `load_track` (→ playback) with **no `eject`/
  go-live tool**, so once a track is loaded the agent can't reach the headline
  live-generation feature. And MCP `deck_play`/`deck_stop` drive Host + sidecar
  + store transport **without checking/setting `mode`** — `deck_play` on a
  playback deck is incoherent.
- **Fix:** add `eject`/`deck_go_live(deck)` that routes through the webview's
  `unloadTrack()` (emit, like `load_track`); make `deck_play`/`deck_stop`
  mode-aware (refuse on a playback deck with a clear message, or route to the
  track transport).

### #12 [FIXED, session 2] `set_style` unusable from Claude Code — nested `$ref` schemas stripped client-side — HIGH
- **Evidence (live):** `set_style` with a structured `cursor` failed:
  `invalid type: string "{\"x\": 0.45, \"y\": 0.3}", expected struct PadPointSnap`.
  The client sent the object as a *string*.
- **Root cause:** the server's schema is correct — schemars emits nested param
  types (`PadPointSnap`, `StyleTargetSnap`, `EqBandArg`…) as `$ref` →
  `$defs`, verified via raw `tools/list` on the wire. But **Claude Code strips
  `$defs`/`$ref` when surfacing tool schemas to the model**, leaving `cursor`
  with only a description (no type) and `targets.items` as `{}`. The model
  can't know the shape, sends a JSON string, rmcp rejects it. Same stripping
  hides the `EqBandArg`/FX-kind/note-mode **enum values** — `set_eq` etc. only
  work because agents guess valid strings blind.
- **Impact:** the style pad — a headline co-DJ surface — can't be re-arranged
  over MCP (workaround: `set_prompt`, single target only). Enum params are
  undiscoverable.
- **Fix (server-side, robust):** rmcp 1.8 hardcodes its schemars settings (no
  `inline_subschemas` hook), but `ToolRouter.map` / `ToolRoute.attr` /
  `Tool.input_schema` are public. At router construction, walk each
  `input_schema`, inline local `#/$defs/*` refs (merging `$ref`-sibling keys
  like `description`), drop `$defs`. No refs → nothing for any client to strip.
  Also worth an upstream report to Claude Code.

### #4 / #6 [FIXED, session 2] Agent actions are invisible in the UI (esp. generation) — MED
- `generate_sample`/`generate_track` proxy the loopback gen server directly and
  only `emit("mcp://load-track")` at the END (tracks) or nothing (samples).
  No "generating…" signal reaches the webview, so a multi-second/minute
  generation looks like nothing is happening. (User-reported.)
- **Fix:** emit `mcp://generation` start/finish (deck, prompt, kind) so the UI
  shows a spinner/toast on the target deck. Consider a general "agent is doing X"
  activity indicator for ALL agent-driven actions (the co-DJ is a second
  operator the human should be able to watch).

## Other findings (lower priority)

- **#5 [FIXED, session 3 — `toggle_pad`] No MCP way to trigger a loaded sample pad.** `load_sample` installs a
  loop/one-shot onto a deck pad slot, but firing a pad is MIDI/UI-only. Consider
  `trigger_pad(deck, index)`.
- **#7 [FIXED, session 2 — two-word `pleasant_title()`] MCP-generated tracks are titled with the raw prompt** (`generate_track`
  sets `title = prompt`), so the songs library shows a long string instead of a
  friendly name like the in-app generator. Cosmetic; consider a short auto-title.
  **Extends to samples:** `generate_sample` filenames are the raw prompt
  truncated mid-word (e.g. `…club techno transiti.wav`), punctuation swapped to
  `-`. Same fix: short auto-title, keep the prompt as metadata.
- **#8 [FIXED, session 5 — async job + `generation_status`] Long `generate_track` may trip the ~60s MCP client timeout.** A 30s track
  returned fine; the server allows up to 380s. If confirmed, make generation
  async (job id / progress) instead of one blocking request. Ties to #4/#6.
  **Timing data (session 3):** a 45 s track generated in ~19.7 s wall clock
  (~2.3× realtime), so the ~60 s client timeout bites around the ~2-minute-track
  mark; the 380 s server cap is unreachable without the async job.
  **Confirmed live (session 4):** a 240 s track through the Claude Code client
  died at 60 s (the raw JSON-RPC workaround completed it in ~2 min). Fixed by
  spawning the work and answering with the job id — see the session-5 status
  entry.
- **#13 [FIXED, session 3 part 2 — `transport.playing`] `get_state` deck `playing` is false while a playback
  transport rolls — MED.** Loaded "Burning Spire" on A, transport playing
  (playhead 1.9 s → 17.6 s, audible), but the deck's `playing` flag stayed
  `false` — it reflects only the realtime stream. An agent trusting `playing`
  thinks the deck is idle; the workaround is sampling `transport.playheadSeconds`
  twice. Fix: fold transport state into `playing` (or add `transport.playing`).
- **#14 [FIXED, session 3 part 2 — descriptions updated] `seek_track` clears an active `beat_loop`
  region.** Engaged a 4-beat loop, then `seek_track` past it → `loopRegion:
  null`. Plausibly intended (seek = leave the loop) but undocumented; mention it
  in the `beat_loop`/`seek_track` descriptions. Also noted: library tracks
  loaded from disk have `bpm: null` (no analysis), while generated tracks carry
  detected BPM (129.99 for a 130 request) — `sync_deck`/`set_tempo` on a
  no-BPM playback deck is untested and probably can't beat-match.
- **#9 (workflow note) New MCP tools need a client re-list to appear.** Expected
  MCP behaviour; documented here so anyone iterating on these tools reconnects
  the client after a rebuild.
- **#10 [FIXED, session 3 — `ramp_ms` on crossfade/volume] Mixer moves are stepwise — no ramp.** `set_crossfade`/`set_volume`/
  `set_eq`/`set_fx_amount` jump instantly; an agent "walking the fader" is a
  series of audible steps, not a blend. For show-quality transitions consider an
  optional `ramp_ms` (engine-side interpolation) on the continuous controls —
  the Rust engine already owns the audio thread, so a linear ramp there would be
  cheap and click-free.
- **#11 [FIXED, session 2 — description updated] `set_on_air(off)` auto-primes a stopped deck** — it starts the deck
  generating in prep (a later `deck_play` returns "already playing"). Handy, but
  undocumented in the tool description; agents should know off-air = start
  prepping, not just mute. Mention it in the `set_on_air` description.

## Verified working during the session
`get_state` (server), `list_songs`, `list_samples`, `load_track`, `set_volume`,
`set_crossfade`, `set_eq`, `set_on_air` (on/off → play/prime), `sync_deck`,
`set_fx` (incl. `dubEcho`), `set_fx_amount`, `clear_fx`, `set_hot_cue`,
`generate_sample` (music, 8s), `generate_track` (30s, end-to-end). Ran a full
A→B→A set with beat-matched bass-swap transitions.

## Nice-to-have: a co-DJ SKILL.md
A `SKILL.md` for driving LSDJ as a co-DJ would help future agents: call
`get_state` first; the deck-mode model (realtime vs playback) and how to switch;
FX wire values (`filter`, `dubEcho`, `space`, `crush`, `noise`, `sweep`) and
curve semantics (filter 0.5 = bypass); transition recipe (beat-match → bass swap
→ crossfade); and the "reconnect after adding a tool" note.
