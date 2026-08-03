# 0002. Browser app with Python model workers, deferring Tauri

- **Status:** Superseded by [ADR-0018](0018-native-macos-shell-tauri-with-python-sidecars.md) and [ADR-0019](0019-pcm-transport-from-python-sidecars-to-the-rust-engine.md) (2026-06-15) — the Python-model-workers core is preserved; see ADR-0018
- **Date:** 2026-06-09
- **Deciders:** Daniel Peter

## Context

LSDJ is a DJ interface over Magenta RealTime 2 (MRT2): two independently
steerable "decks" of generated audio, blended with a crossfader. The model
dictates most of the constraints:

- MRT2's inference surfaces are a Python library (`magenta-rt`, MLX backend on
  Apple Silicon), a C++ engine (`magentart::core`), and a batch-only CLI. The
  steering features this project depends on — embedding text prompts, blending
  style embeddings with weights, changing them between chunks — are exposed in
  the Python library. The C++ engine is the least documented path and requires
  a CMake build plus our own bindings. The CLI renders fixed-duration clips and
  cannot stream.
- Generation is chunked (~2 s of 48 kHz PCM at a time), so the UI ↔ model
  interface is a stream of PCM buffers plus control messages — a WebSocket,
  regardless of what shell the UI lives in.
- Target machines are Apple Silicon Macs with headroom for two concurrent
  model instances (Pro/Max-class for two `mrt2_base` decks). Each deck should
  be able to use a different model size (`mrt2_small` / `mrt2_base`).

A desktop shell (Tauri) was considered because the project may eventually want
a double-clickable app. But Tauri's Rust backend cannot call the model any more
directly than a browser can — the bridge to Python is a WebSocket either way —
so the shell choice is independent of the model integration.

## Decision

We will build v1 as a plain browser app served by a local Python backend:

- A FastAPI controller process serves the React build, exposes one WebSocket
  per deck (binary PCM frames + JSON control messages), and supervises the
  workers.
- Each deck is its own Python worker process running `magenta_rt` on MLX. One
  process per deck gives memory isolation, avoids GIL contention between two
  inference loops, and makes switching a deck's model a worker restart that
  does not touch the other deck.
- No Tauri (or other desktop shell) in v1.

## Consequences

- Easier: fastest path to audible results; one backend runtime and dependency
  tree (uv-managed Python); the model's full Python steering API is available
  for prompt morphing; iteration is browser-refresh fast.
- Easier later: the React app and the WebSocket protocol are shell-agnostic.
  Wrapping in Tauri later means moving the frontend into a webview and managing
  the Python processes as sidecars — no protocol or UI rework.
- Harder: no single distributable binary; the user starts a local server.
  Acceptable for a personal tool.
- Accepted risk: Python sits in the audio path, but only hands ~2 s PCM
  buffers to a WebSocket per deck; the hot loop is MLX's compiled Metal
  kernels, so interpreter overhead is noise.
- Revisit trigger: if the project grows toward a self-contained distributable
  app, binding `magentart::core` into a native shell becomes the right path —
  record that change as a new ADR superseding this one.

## Alternatives considered

- **Tauri from day one (Python sidecars)** - adds Rust scaffolding, sidecar
  lifecycle, and packaging work before any audio is heard, while the actual
  model bridge (WebSocket to Python) is identical. Deferred, not rejected.
- **Tauri + C++ engine (`magentart::core`) via FFI, no Python** - lowest
  latency and a single binary, but the least documented integration path,
  needs a CMake build plus Rust bindings, and the style-embedding blending we
  need is not a documented C++ surface. Too much risk and work for v1.
- **Node/TypeScript controller with Python workers** - the inference workers
  must be Python regardless, so this adds a second backend runtime to replace
  ~150 lines of FastAPI. No benefit.
- **Shelling out to the `mrt` CLI** - batch-only; no continuous streaming or
  mid-stream prompt changes. Unusable for decks.

<!-- Status values: Proposed | Accepted | Rejected | Deprecated |
     Superseded by ADR-NNNN -->
