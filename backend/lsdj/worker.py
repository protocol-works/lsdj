"""Deck worker process: runs the generation loop around a DeckEngine.

Commands arrive on cmd_queue as dicts ({"type": "play" | "stop" |
"set_prompt" | "shutdown", ...}). Output goes to out_queue as
("audio", bytes) and ("status", dict) tuples. Clip renders (M18) answer
on the dedicated clip_queue as (id, {"pcm": ...} | {"error": ...}) so
multi-second results never sit behind the audio stream.

Pacing: the model generates faster than real time, so the worker throttles
itself to stay TARGET_AHEAD_SECONDS ahead of wall-clock playback — enough
cushion to absorb a slow chunk, small enough that a prompt change is heard
within a few seconds. The bounded out_queue is only a safety net for a
stalled client.
"""

import logging
import queue
import time

from .engine import DeckEngine
from .mrt2 import public_startup_error

logger = logging.getLogger(__name__)

IDLE_POLL_SECONDS = 0.2
TARGET_AHEAD_SECONDS = 3.0


def run_deck_worker(
    deck_id: str,
    model: str,
    cmd_queue,
    out_queue,
    engine_factory=DeckEngine,
    clip_queue=None,
    perform_warmup=True,
) -> None:
    logging.basicConfig(level=logging.INFO)
    logger.info("deck %s: loading %s", deck_id, model)
    try:
        engine = engine_factory(model=model)
        warm_up = getattr(engine, "warm_up", None)
        if perform_warmup and callable(warm_up):
            out_queue.put(
                ("status", {"event": "warming", "deck": deck_id, "model": model})
            )
            warm_up()
        diagnostics = getattr(engine, "diagnostics", None)
        runtime = diagnostics() if callable(diagnostics) else {}
    except Exception as error:
        logger.exception("deck %s: startup failed", deck_id)
        out_queue.put(
            (
                "status",
                {
                    "event": "startup_failed",
                    "deck": deck_id,
                    "model": model,
                    "error": public_startup_error(error),
                },
            )
        )
        return
    out_queue.put(
        (
            "status",
            {
                "event": "ready",
                "deck": deck_id,
                "model": model,
                "runtime": runtime,
            },
        )
    )

    playing = False
    style: dict | None = None
    chunk_index = 0
    # Pacing clock: seconds of audio emitted since play, vs. wall time since
    # play. Reset on every stop→play transition.
    pace_epoch = 0.0
    pace_seconds = 0.0

    while True:
        # Apply every pending command. Blocks while idle, and while playing
        # blocks for exactly the throttle wait, so commands are handled
        # immediately either way.
        while True:
            try:
                if playing:
                    ahead = pace_seconds - (time.monotonic() - pace_epoch)
                    wait = ahead - TARGET_AHEAD_SECONDS
                    if wait > 0:
                        cmd = cmd_queue.get(timeout=wait)
                    else:
                        cmd = cmd_queue.get_nowait()
                else:
                    cmd = cmd_queue.get(timeout=IDLE_POLL_SECONDS)
            except queue.Empty:
                if playing:
                    break  # the next chunk is due
                continue
            kind = cmd["type"]
            if kind == "shutdown":
                logger.info("deck %s: shutting down", deck_id)
                return
            if kind == "play":
                if not playing:
                    playing = True
                    pace_epoch = time.monotonic()
                    pace_seconds = 0.0
                    # A fresh stream starts unsteered: note/drum conditioning
                    # resets on every discontinuity (ADR-0023), like the beat
                    # gate and freeze captures (ADR-0009/0010). The frontend
                    # clears its own mirror on the same transitions.
                    engine.set_notes(None)
                    engine.set_drums(None)
            elif kind == "stop":
                playing = False
                engine.set_notes(None)
                engine.set_drums(None)
            elif kind == "render_clip":
                # Worker-side gate, where the truth lives: rendering on a
                # playing deck would stall the pacing loop for seconds.
                if clip_queue is None:
                    # Deck workers ship without a clip queue — only the
                    # render worker gets one. A misrouted render has nowhere
                    # to answer; drop it rather than crash the stream.
                    logger.warning(
                        "deck %s: render_clip with no clip queue; dropped", deck_id
                    )
                elif playing:
                    clip_queue.put((cmd["id"], {"error": "deck is playing"}))
                else:
                    try:
                        pcm = engine.render_clip(cmd["prompt"], cmd["seconds"])
                    except Exception:
                        logger.exception("deck %s: render_clip failed", deck_id)
                        clip_queue.put((cmd["id"], {"error": "render failed"}))
                    else:
                        clip_queue.put((cmd["id"], {"pcm": pcm}))
            elif kind == "embed_sample":
                started = time.monotonic()
                try:
                    engine.embed_sample(cmd["id"], cmd["pcm"])
                except Exception:
                    logger.exception("deck %s: embed_sample failed", deck_id)
                    out_queue.put(
                        (
                            "status",
                            {"event": "error", "error": "sample embed failed"},
                        )
                    )
                else:
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": "sample_embedded",
                                "id": cmd["id"],
                                "embed_seconds": round(time.monotonic() - started, 3),
                            },
                        )
                    )
            elif kind == "set_chunk_frames":
                # The ADR-0023 performance knob (issue #48): an armed deck
                # runs small chunks for playable steering latency. Applied
                # between chunks like every command; unlike note/drum
                # steering it is a mode, so it survives play/stop.
                try:
                    engine.set_chunk_frames(cmd["frames"])
                except Exception:
                    logger.exception("deck %s: set_chunk_frames failed", deck_id)
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": "error",
                                "error": "set_chunk_frames failed; chunk unchanged",
                            },
                        )
                    )
                else:
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": "chunk_frames_applied",
                                "frames": cmd["frames"],
                                "effective_from_chunk": chunk_index,
                            },
                        )
                    )
            elif kind == "set_generation":
                # Live generation params (issue #84): the deck's sampling /
                # guidance operating point, applied between chunks like every
                # command. A mode, not held conditioning — it survives play/stop
                # (the authored value lives shell-side and is re-sent on ready).
                try:
                    engine.set_generation(
                        cmd["temperature"],
                        cmd["top_k"],
                        cmd["cfg_musiccoca"],
                        cmd["cfg_notes"],
                    )
                except Exception:
                    logger.exception("deck %s: set_generation failed", deck_id)
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": "error",
                                "error": "set_generation failed; params unchanged",
                            },
                        )
                    )
                else:
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": "generation_applied",
                                "effective_from_chunk": chunk_index,
                            },
                        )
                    )
            elif kind == "reset":
                reset = getattr(engine, "reset", None)
                if not callable(reset):
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": "error",
                                "error": "reset is unsupported by this runtime",
                            },
                        )
                    )
                else:
                    try:
                        reset(seed=cmd.get("seed"))
                    except Exception:
                        logger.exception("deck %s: reset failed", deck_id)
                        out_queue.put(
                            (
                                "status",
                                {"event": "error", "error": "reset failed"},
                            )
                        )
                    else:
                        playing = False
                        pace_seconds = 0.0
                        out_queue.put(
                            (
                                "status",
                                {
                                    "event": "reset",
                                    "seed": cmd.get("seed"),
                                    "effective_from_chunk": chunk_index,
                                },
                            )
                        )
            elif kind in ("set_notes", "set_drums"):
                # Idempotent full-state conditioning (ADR-0023): the payload
                # replaces the held state wholesale; None returns to masked.
                try:
                    if kind == "set_notes":
                        engine.set_notes(cmd["notes"])
                    else:
                        # `cfg` (issue #50) is optional on the wire — a missing
                        # one falls back to the library default in the engine.
                        engine.set_drums(cmd["drums"], cmd.get("cfg"))
                except Exception:
                    # The deck must survive a bad payload; the Rust deck command
                    # validates shape at the trust boundary (the sidecar shell is
                    # a pass-through), and the engine's range check is the truth.
                    logger.exception("deck %s: %s failed", deck_id, kind)
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": "error",
                                "error": f"{kind} failed; conditioning unchanged",
                            },
                        )
                    )
                else:
                    field = "notes" if kind == "set_notes" else "drums"
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": f"{field}_applied",
                                field: cmd[field],
                                "effective_from_chunk": chunk_index,
                            },
                        )
                    )
            elif kind in ("set_prompt", "set_style"):
                started = time.monotonic()
                if kind == "set_prompt":
                    entries = [{"text": cmd["prompt"], "weight": 1.0}]
                else:
                    entries = cmd["prompts"]
                try:
                    # Sampled entries (M15) carry their cache id alongside
                    # the display label; the id is the blend key. Keys
                    # share one namespace: a TEXT prompt typed to exactly
                    # match a live sample id would resolve as that sample.
                    # Accepted — ids are machine-shaped ("sample:a:1") and
                    # the collision needs another entry holding the id in
                    # the same style.
                    engine.set_style(
                        [
                            (entry.get("sample") or entry["text"], entry["weight"])
                            for entry in entries
                        ],
                        sample_keys=frozenset(
                            entry["sample"] for entry in entries if entry.get("sample")
                        ),
                    )
                except Exception:
                    # The deck must survive a bad prompt; the controller
                    # validates shape, but embedding can still fail.
                    logger.exception("deck %s: set_style failed", deck_id)
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": "error",
                                "error": "set_style failed; style unchanged",
                            },
                        )
                    )
                else:
                    style = {"prompts": entries}
                    out_queue.put(
                        (
                            "status",
                            {
                                "event": "style_applied",
                                **style,
                                "effective_from_chunk": chunk_index,
                                "embed_seconds": round(time.monotonic() - started, 3),
                            },
                        )
                    )

        started = time.monotonic()
        try:
            pcm = engine.generate_chunk()
        except Exception:
            logger.exception("deck %s: generation failed; deck stopped", deck_id)
            playing = False
            out_queue.put(
                (
                    "status",
                    {"event": "error", "error": "generation failed; deck stopped"},
                )
            )
            # A worker that stops ITSELF must end the transport upstream too:
            # the error above feeds the UI banner, and this structured event
            # feeds the shell's transport relay. Without it the store kept
            # claiming `playing` after a generation failure, so the next
            # deck_play round-tripped as a value-equal no-op — no snapshot,
            # a wedged in-flight guard, and a play button that swallowed
            # presses until a stop (found on the device).
            out_queue.put(
                ("status", {"event": "stopped", "reason": "generation failed"})
            )
            continue
        elapsed = time.monotonic() - started
        out_queue.put(("audio", pcm))
        try:
            queue_depth = cmd_queue.qsize()
        except (AttributeError, NotImplementedError, OSError):
            queue_depth = None
        out_queue.put(
            (
                "status",
                {
                    "event": "chunk",
                    "index": chunk_index,
                    "generation_latency_ms": round(elapsed * 1000, 2),
                    "queue_depth": queue_depth,
                    "rtf": (
                        round(engine.chunk_seconds / elapsed, 2)
                        if elapsed > 0
                        else None
                    ),
                    "style": style,
                },
            )
        )
        chunk_index += 1
        # Pace on the engine's CURRENT chunk length — the performance knob
        # changes it mid-stream, and pacing on a constant would let an armed
        # deck run 5× ahead (or starve) of real time.
        pace_seconds += engine.chunk_seconds
