# 0020. Rust as the single interface-state store, exposed via a native MCP server

- **Status:** Accepted (2026-06-28)
- **Date:** 2026-06-20
- **Deciders:** Daniel Peter

> **Amended on acceptance (2026-06-28, while planning issue #37 Phase 1).**
> Narrowed from the Proposed draft: the store owns all **semantic/identity +
> audio-param** interface state, but **ephemeral view state stays in React**
> (active tab, scroll/highlight, expanded row, in-progress form fields, the
> loaded-but-not-confirmed selection). An agent does not need to drive scroll
> position, and the per-keystroke IPC churn the draft flagged buys nothing for the
> MCP goal. The Decision and Consequences below reflect this narrowing.

> **Amended (2026-06-28, issue #37 Phase 2 follow-up): the MCP server is now
> always on — no flag, no opt-out.** It originally shipped gated behind a
> `LSDJ_SIDECARS`-style `LSDJ_MCP` flag (see the Dependency decision) while the
> `rmcp` dependency was unproven. With Phase 2 in place — the server is
> loopback-bound only and every request needs the bearer token (now persisted and
> rotatable) — the flag bought nothing but a way to forget to turn the feature on.
> It starts with the app like the generation server already does; a failed
> loopback bind is the only path that leaves the endpoint unadvertised
> (`port() == None`).

## Context

LSDJ is a generative DJ instrument. We want an external AI agent
(Claude Desktop / Claude Code) to act as a co-DJ: generate audio *and* drive
the decks, mixer, and FX live — alongside the human at the hardware and the
on-screen UI. MCP is the protocol those clients speak.

Two facts about the current architecture force the decision:

1. **Interface state is fragmented and mostly held in the React frontend.** The
   Rust audio engine (`lsdj-engine`) is authoritative only for the audio
   itself — playback transport, loop regions, buffers, meters — plus the
   filesystem library. Everything semantic lives in React
   (`useDeck`/`deckReducer`/`App.tsx`): realtime-deck prompt/style/model/playing,
   each track's identity (title, file, bpm, grid), hot-cue points (ADR-0015
   keeps these as TS-only deck state), loop-slot labels, mixer/FX positions (the
   engine has only setters in `graph.rs`, no getters), crossfade and cue-mix
   (App.tsx React state), and all browser/selection/tab/scroll state. The engine
   control surface is effectively write-only: it cannot report where the
   crossfader is, which track is loaded, or what prompt a deck is on.
2. **The only external-reachable control today is generation.** The Python
   FastAPI controller (`controller.py`) is on a loopback port (`generation.rs`).
   The engine `Host` (`lsdj-engine::host::Host`) is reachable only via Tauri
   `invoke` from the webview, validated/clamped at the `commands.rs` trust
   boundary. No external process can drive the decks/mixer/FX.

For an agent to be a real co-DJ it must both *act on* and *observe* the full
interface state. Bolting read-back onto the write-only surface — getters here, a
now-playing registry there, a last-set shadow elsewhere — would scatter the
state across the engine, the sidecar, and React, leaving two copies that drift.
The deeper problem is ownership: with three peer controllers now in view (the
on-screen UI, the hardware via MIDI, and an agent via MCP), there is no single
place that holds "the current state of the instrument."

Forces in tension:

- An external agent and the human (UI + MIDI) must act on the *same* state;
  divergent copies would let the agent and the screen disagree.
- The engine `Host` is reachable only from the webview today; an external client
  cannot `invoke` it.
- A live audio instrument under external control is a real trust boundary.
- No external programmatic-control interface exists, largely by design
  (ADR-0003's "remote control" aside). Introducing one is deliberate.
- A general MCP client attaches to a long-running server; it cannot usefully
  spawn a fresh GUI app, which already owns the audio device, per session.

## Decision

We will make **Rust the single source of truth for all interface state**, turn
the webview into a **unidirectional projection** of it, and **expose that store
to external agents through a native, in-process MCP server**. The on-screen UI,
the hardware (MIDI), and the agent (MCP) become symmetric **peer controllers**:
each emits intents that mutate the one store; the store emits change events; the
webview re-renders from the snapshot.

**The state store (the inversion).**

- A **shell-level state store** in the Tauri/`Host` layer becomes authoritative
  for all **semantic/identity + audio-param** interface state: realtime-deck
  prompt/style/model/playing, loaded-track identity (title/file/bpm/grid), mixer
  (volume/EQ/trim/crossfade/cue/cue-mix), FX kind + amount, hot-cue points,
  loop-slot labels. **Ephemeral view state stays in React** (active browser tab,
  scroll/highlight, expanded row, in-progress form fields, the
  loaded-but-not-confirmed selection): an agent does not drive scroll position,
  so the store does not carry it (the narrowing recorded above).
- **Layering.** The real-time audio core (`lsdj-engine`, headless/RT-safe)
  keeps owning the audio params it already does (gains, EQ coefficients,
  crossfade, loop regions, buffers) inside the mix graph and stays out of the
  cpal callback path; the store holds the *semantic / identity / view* state
  alongside and mirrors the engine's audio read-backs. The engine remains the
  truth of "what the audio is doing"; the store is the truth of "what the
  instrument shows."
- **Data flow.** UI / MIDI / MCP → intent → validated mutation of the store
  (reusing the `commands.rs` trust-boundary validation) → audio-affecting changes
  forwarded to the engine / sidecar as today → store emits a change event → the
  webview projects it. The existing `ControlIntent` bus is re-pointed so intents
  resolve against the Rust store rather than React-local state.

**The MCP server.**

- Hosted **inside the Tauri/Rust process**. Tools mutate the store (the same path
  UI and MIDI take); resources read it. Generation tools proxy to the existing
  loopback FastAPI server, reusing its prompt/length limits.
- **Transport.** A streamable-HTTP MCP endpoint bound to `127.0.0.1`, advertised
  via `app_info` the way the generation port already is. A **stdio shim is
  deferred** to a later, optional phase — a thin proxy to the HTTP endpoint.
- **Trust.** Loopback-bound only; a per-session bearer token generated at startup
  and surfaced for the client config; the server runs only while the app runs;
  the store's validation stays the authoritative guard regardless of caller.
- **Dependency.** The official Rust MCP SDK (`rmcp`,
  modelcontextprotocol/rust-sdk), pinned. (Originally gated behind a
  `LSDJ_SIDECARS`-style `LSDJ_MCP` flag; now **always on** — see the amendment
  note above.)

This supersedes **ADR-0015 on the location of hot-cue state** (cue points move
into the store; 0015's loops-on-the-buffer-source decision stands). It does not
change the audio path (ADR-0017) or how generation is hosted (ADR-0018/0019).

## Consequences

- Easier: one authoritative copy of the instrument state — the agent, the screen,
  and the hardware can never disagree, because they read and write the same store.
- Easier: the MCP server needs no bespoke read-back — no getters to bolt on, no
  now-playing registry, no last-set shadow that drifts. Observation is just
  reading the store.
- Easier: actuation reuses the `commands.rs` validation/clamping as the single
  mutation guard for every controller (UI, MIDI, MCP), with no second copy of the
  rules.
- Easier: the store is serialisable — session save/restore, undo, and richer
  telemetry fall out of having one place that holds the state.
- Easier: concurrent human + agent control reduces to well-defined writes against
  one store (last-write-wins, or an explicit policy) instead of reconciling
  divergent copies.
- Harder (the big cost): this **inverts current frontend ownership**. React
  (`useDeck`/`deckReducer`/`App.tsx`) is authoritative today; it becomes a
  projection that renders from a Rust snapshot and emits intents. That is a
  substantial frontend refactor and the larger half of this work — the MCP server
  is the easy half once it lands.
- Scoped out (the acceptance narrowing): *view* state (tab/scroll/highlight/
  in-progress form fields) is **kept in React**, not the store. Putting it in the
  store would push ephemeral UI churn across the IPC boundary on every keystroke
  and scroll for no agent benefit — an agent drives the instrument, not the scroll
  position. The store stays the single source of truth for the semantic/audio
  state an agent and the human share; view state remains a private React concern.
- Harder: strict unidirectional flow means a fader / jog / crossfade drag
  round-trips through Rust before the knob re-renders. Sub-millisecond on
  loopback, but high-rate performance gestures need **optimistic local rendering**
  during the drag with the store as the reconciliation truth.
- Harder: a new dependency to vet and pin (`rmcp`, the younger Rust MCP SDK).
  Revisit trigger: if it proves unworkable, fall back to a thin Rust HTTP server
  speaking MCP directly, or a Python-host with a new control channel.
- Harder: every exposed action needs an MCP tool definition kept in sync with the
  store's mutation surface as it evolves.
- Security surface: a token-gated loopback HTTP server is now part of the app;
  token handling, the bind address, and "is the app running?" failures need care
  and a hardware/integration checklist (live audio under external control cannot
  be fully unit-tested).
- Follow-up: this reverses ADR-0015's hot-cue-location decision; once this ADR is
  accepted, 0015's status must be annotated to point here.

## Alternatives considered

- **Keep React authoritative; bolt read-back onto the engine for MCP** — add
  getters for mixer/FX positions, a now-playing registry, and a last-set
  prompt/model shadow, leaving the rest of the interface state in React. A smaller
  change, but it scatters state across engine / sidecar / React with copies that
  drift, and leaves the agent blind to anything not explicitly mirrored. Rejected:
  it patches observation for MCP without fixing the ownership problem, and the
  half-measures sum to more long-term complexity than the inversion.
- **Two ADRs (state-ownership + MCP) rather than one** — cleaner to cite and
  supersede later, since the data-flow inversion has value independent of MCP.
  Folded into this single record by choice: the inversion and the MCP surface are
  being decided and built together, so they are recorded together.
- **MCP server in the Python loopback server + a new Rust control channel** — the
  Python MCP SDK (FastMCP) is more mature and generation is already local, but
  store/engine control would need a reverse Python→Rust channel that does not
  exist (sidecars are Rust→Python only), splitting control across processes.
  Rejected; retained as the fallback if `rmcp` fails.
- **Bridge through the webview** — let MCP drive the existing React-authoritative
  state via a native→JS hop. Rejected: control would depend on the webview being
  alive, and it entrenches the fragmented ownership this decision removes.
- **stdio transport as primary** — Claude Desktop spawns stdio servers as
  subprocesses, which cannot work for a GUI app already running and owning the
  audio device. Rejected as primary; retained as an optional later shim.
- **Do nothing / keep external control out of scope** — preserves the prior
  implicit stance and avoids the security surface, but forecloses the AI-co-DJ
  direction that motivates this work. Rejected deliberately — which is why this
  ADR exists.

<!-- Status values: Proposed | Accepted | Rejected | Deprecated |
     Superseded by ADR-NNNN -->
