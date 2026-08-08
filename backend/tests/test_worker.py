"""Worker loop tests: the deck must survive engine failures.

The worker runs in a thread with a fake engine (run_deck_worker only needs
the queue interface, so plain queue.Queue works in place of mp.Queue).
"""

import queue
import threading
import time

import pytest

from lsdj.worker import run_deck_worker

FAKE_PCM = b"\x00" * 64


class FakeEngine:
    def __init__(self):
        self.styles = []
        self.style_sample_keys = []
        self.samples = []
        self.renders = []
        self.notes = []
        self.drums = []
        self.generation = []
        self.chunk_frames = []
        self.chunk_seconds = 1.0
        self.fail_set_style = False
        self.fail_embed_sample = False
        self.fail_generate = False
        self.fail_render = False
        self.fail_set_notes = False
        self.fail_set_chunk_frames = False
        self.fail_set_generation = False
        self.resets = []

    def diagnostics(self):
        return {"runtime": "fake", "hardware_qualified": False}

    def reset(self, *, seed=None):
        self.resets.append(seed)

    def render_clip(self, prompt, seconds):
        if self.fail_render:
            raise RuntimeError("render blew up")
        self.renders.append((prompt, seconds))
        return FAKE_PCM

    def set_style(self, prompts, sample_keys=frozenset()):
        if self.fail_set_style:
            raise RuntimeError("embed blew up")
        self.styles.append(prompts)
        self.style_sample_keys.append(sample_keys)

    def embed_sample(self, sample_id, pcm):
        if self.fail_embed_sample:
            raise RuntimeError("audio embed blew up")
        self.samples.append((sample_id, len(pcm)))

    def set_notes(self, notes):
        if self.fail_set_notes:
            raise ValueError("bad multihot")
        self.notes.append(notes)

    def set_drums(self, flag, cfg=None):
        self.drums.append((flag, cfg))

    def set_chunk_frames(self, frames):
        if self.fail_set_chunk_frames:
            raise ValueError("bad chunk frames")
        self.chunk_frames.append(frames)
        self.chunk_seconds = frames * 0.04

    def set_generation(self, temperature, top_k, cfg_musiccoca, cfg_notes):
        if self.fail_set_generation:
            raise ValueError("bad generation params")
        self.generation.append((temperature, top_k, cfg_musiccoca, cfg_notes))

    def generate_chunk(self):
        if self.fail_generate:
            raise RuntimeError("inference blew up")
        return FAKE_PCM


class DeckHarness:
    def __init__(self, with_clip_queue=True):
        self.engine = FakeEngine()
        self.cmd_queue = queue.Queue()
        self.out_queue = queue.Queue()
        # Production deck workers run without a clip queue — only the
        # render worker gets one (M18).
        self.clip_queue = queue.Queue() if with_clip_queue else None
        self.thread = threading.Thread(
            target=run_deck_worker,
            args=("test", "fake", self.cmd_queue, self.out_queue),
            kwargs={
                "engine_factory": lambda model: self.engine,
                "clip_queue": self.clip_queue,
            },
            daemon=True,
        )

    def send(self, **command):
        self.cmd_queue.put(command)

    def next_event(self, wanted_kind, timeout=3.0):
        """Return the next ('audio' | status-event-name) payload, skipping others."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                kind, payload = self.out_queue.get(timeout=0.1)
            except queue.Empty:
                continue
            if kind == "audio" and wanted_kind == "audio":
                return payload
            if kind == "status" and payload.get("event") == wanted_kind:
                return payload
        raise AssertionError(f"no {wanted_kind!r} event within {timeout}s")


@pytest.fixture
def deck():
    harness = DeckHarness()
    harness.thread.start()
    harness.next_event("ready")
    yield harness
    harness.send(type="shutdown")
    harness.thread.join(timeout=2)
    assert not harness.thread.is_alive()


def test_play_emits_audio(deck):
    deck.send(type="play")
    assert deck.next_event("audio") == FAKE_PCM
    chunk = deck.next_event("chunk")
    assert chunk["index"] == 0
    assert chunk["generation_latency_ms"] >= 0
    assert chunk["queue_depth"] is not None
    assert chunk["rtf"] is not None


def test_ready_carries_runtime_diagnostics():
    harness = DeckHarness()
    harness.thread.start()
    ready = harness.next_event("ready")
    assert ready["runtime"] == {
        "runtime": "fake",
        "hardware_qualified": False,
    }
    harness.send(type="shutdown")
    harness.thread.join(timeout=2)


def test_startup_failure_is_structured_and_worker_exits():
    out_queue = queue.Queue()

    def fail(**_kwargs):
        raise RuntimeError("CUDA unavailable")

    run_deck_worker("a", "mrt2_small", queue.Queue(), out_queue, engine_factory=fail)
    kind, status = out_queue.get_nowait()
    assert kind == "status"
    assert status == {
        "event": "startup_failed",
        "deck": "a",
        "model": "mrt2_small",
        "error": (
            "RuntimeError: MRT2 worker startup failed; "
            "inspect the local application log for details"
        ),
    }


def test_reset_to_reseed_stops_generation_and_reports_contract(deck):
    deck.send(type="reset", seed=42)
    status = deck.next_event("reset")
    assert status["seed"] == 42
    assert deck.engine.resets == [42]


def test_set_prompt_applies_as_single_prompt_style(deck):
    deck.send(type="set_prompt", prompt="warm disco funk")
    applied = deck.next_event("style_applied")
    assert applied["prompts"] == [{"text": "warm disco funk", "weight": 1.0}]
    assert deck.engine.styles[-1] == [("warm disco funk", 1.0)]


def test_set_style_blends_many_prompts(deck):
    prompts = [
        {"text": "warm disco funk", "weight": 0.5},
        {"text": "dark minimal techno", "weight": 0.3},
        {"text": "dub reggae", "weight": 0.2},
    ]
    deck.send(type="set_style", prompts=prompts)
    applied = deck.next_event("style_applied")
    assert applied["prompts"] == prompts
    assert deck.engine.styles[-1] == [
        ("warm disco funk", 0.5),
        ("dark minimal techno", 0.3),
        ("dub reggae", 0.2),
    ]


def test_set_style_failure_keeps_worker_alive(deck):
    deck.engine.fail_set_style = True
    deck.send(type="set_prompt", prompt="boom")
    assert "set_style failed" in deck.next_event("error")["error"]

    # The deck must still take commands and play afterwards.
    deck.engine.fail_set_style = False
    deck.send(type="set_prompt", prompt="recovered")
    assert deck.next_event("style_applied")["prompts"][0]["text"] == "recovered"
    deck.send(type="play")
    assert deck.next_event("audio") == FAKE_PCM


def test_embed_sample_caches_and_reports(deck):
    deck.send(type="embed_sample", id="sample:a:1", pcm=b"\x00" * 32)
    embedded = deck.next_event("sample_embedded")
    assert embedded["id"] == "sample:a:1"
    assert deck.engine.samples == [("sample:a:1", 32)]


def test_embed_sample_failure_keeps_worker_alive(deck):
    deck.engine.fail_embed_sample = True
    deck.send(type="embed_sample", id="sample:a:1", pcm=b"\x00" * 32)
    assert "sample embed failed" in deck.next_event("error")["error"]

    deck.send(type="play")
    assert deck.next_event("audio") == FAKE_PCM


def test_set_style_resolves_sampled_entries_by_id(deck):
    prompts = [
        {"text": "warm disco funk", "weight": 0.5},
        {"text": "⏺ A·1", "weight": 0.5, "sample": "sample:a:1"},
    ]
    deck.send(type="set_style", prompts=prompts)
    applied = deck.next_event("style_applied")
    # The echo keeps the display entries; the engine blends by id.
    assert applied["prompts"] == prompts
    assert deck.engine.styles[-1] == [
        ("warm disco funk", 0.5),
        ("sample:a:1", 0.5),
    ]
    assert deck.engine.style_sample_keys[-1] == frozenset({"sample:a:1"})


def test_set_notes_applies_and_reports(deck):
    multihot = [0] * 128
    multihot[60] = 3
    deck.send(type="set_notes", notes=multihot)
    applied = deck.next_event("notes_applied")
    assert applied["notes"] == multihot
    assert applied["effective_from_chunk"] == 0
    assert deck.engine.notes[-1] == multihot

    # Full-state messages are idempotent: a replay applies cleanly.
    deck.send(type="set_notes", notes=multihot)
    assert deck.next_event("notes_applied")["notes"] == multihot
    assert deck.engine.notes[-1] == multihot


def test_set_drums_applies_and_reports(deck):
    deck.send(type="set_drums", drums=0, cfg=5.0)
    applied = deck.next_event("drums_applied")
    assert applied["drums"] == 0
    assert applied["effective_from_chunk"] == 0
    # The strength (issue #50) rides the same message through to the engine.
    assert deck.engine.drums[-1] == (0, 5.0)

    # None returns the flag to masked — the model decides.
    deck.send(type="set_drums", drums=None)
    assert deck.next_event("drums_applied")["drums"] is None
    # A message without cfg falls back to the library default (None).
    assert deck.engine.drums[-1] == (None, None)


def test_set_chunk_frames_applies_and_reports(deck):
    deck.send(type="set_chunk_frames", frames=5)
    applied = deck.next_event("chunk_frames_applied")
    assert applied["frames"] == 5
    assert applied["effective_from_chunk"] == 0
    assert deck.engine.chunk_frames[-1] == 5

    # The knob is a mode, not steering: a play does not reset it (the worker
    # sends no counter-command), and the deck keeps generating.
    deck.send(type="play")
    assert deck.next_event("audio") == FAKE_PCM
    assert deck.engine.chunk_frames == [5]


def test_set_chunk_frames_failure_keeps_worker_alive(deck):
    deck.engine.fail_set_chunk_frames = True
    deck.send(type="set_chunk_frames", frames=0)
    assert "set_chunk_frames failed" in deck.next_event("error")["error"]
    # The deck must survive and keep taking commands.
    deck.engine.fail_set_chunk_frames = False
    deck.send(type="set_chunk_frames", frames=25)
    assert deck.next_event("chunk_frames_applied")["frames"] == 25


def test_set_generation_applies_and_reports(deck):
    deck.send(
        type="set_generation",
        temperature=0.7,
        top_k=20,
        cfg_musiccoca=3.0,
        cfg_notes=1.0,
    )
    applied = deck.next_event("generation_applied")
    assert applied["effective_from_chunk"] == 0
    assert deck.engine.generation[-1] == (0.7, 20, 3.0, 1.0)

    # The params are a mode, not steering: a play does not reset them (the
    # worker sends no counter-command), and the deck keeps generating.
    deck.send(type="play")
    assert deck.next_event("audio") == FAKE_PCM
    assert deck.engine.generation == [(0.7, 20, 3.0, 1.0)]


def test_set_generation_failure_keeps_worker_alive(deck):
    deck.engine.fail_set_generation = True
    deck.send(
        type="set_generation",
        temperature=0.0,
        top_k=1,
        cfg_musiccoca=1.6,
        cfg_notes=2.4,
    )
    assert "set_generation failed" in deck.next_event("error")["error"]
    # The deck must survive and keep taking commands.
    deck.engine.fail_set_generation = False
    deck.send(
        type="set_generation",
        temperature=1.1,
        top_k=50,
        cfg_musiccoca=1.6,
        cfg_notes=2.4,
    )
    assert deck.next_event("generation_applied")["effective_from_chunk"] == 0


def test_set_notes_failure_keeps_worker_alive(deck):
    deck.engine.fail_set_notes = True
    deck.send(type="set_notes", notes=[9])
    assert "set_notes failed" in deck.next_event("error")["error"]

    deck.engine.fail_set_notes = False
    deck.send(type="play")
    assert deck.next_event("audio") == FAKE_PCM


def test_play_resets_note_and_drum_conditioning(deck):
    multihot = [3] * 128
    deck.send(type="set_notes", notes=multihot)
    deck.send(type="set_drums", drums=1)
    deck.next_event("drums_applied")
    deck.send(type="play")
    deck.next_event("audio")
    # A fresh stream starts unsteered (ADR-0023's discontinuity rule); the
    # shell re-asserts drum-sit on the play edge (issue #50), not the worker.
    assert deck.engine.notes[-1] is None
    assert deck.engine.drums[-1] == (None, None)


def test_stop_resets_note_and_drum_conditioning(deck):
    deck.send(type="play")
    deck.next_event("audio")
    deck.send(type="set_notes", notes=[3] * 128)
    deck.next_event("notes_applied")
    deck.send(type="stop")
    # A follow-up command's echo proves the FIFO drained past the stop.
    deck.send(type="set_prompt", prompt="proof of drain")
    deck.next_event("style_applied")
    assert deck.engine.notes[-1] is None
    assert deck.engine.drums[-1] == (None, None)


def test_generation_failure_stops_deck_but_worker_survives(deck):
    deck.engine.fail_generate = True
    deck.send(type="play")
    assert "generation failed" in deck.next_event("error")["error"]
    # The self-stop is a TRANSPORT event, not just an error: the shell relay
    # must drop the store's `playing` or the next play round-trips as a
    # no-op and the UI's in-flight guard wedges (found on the device).
    assert deck.next_event("stopped")["reason"] == "generation failed"

    # The failure auto-stopped the deck; play must work again once the
    # engine recovers.
    deck.engine.fail_generate = False
    deck.send(type="play")
    assert deck.next_event("audio") == FAKE_PCM


def test_render_clip_answers_on_the_clip_queue(deck):
    deck.send(type="render_clip", id="clip-1", prompt="air horn", seconds=2.0)
    result_id, result = deck.clip_queue.get(timeout=3.0)
    assert result_id == "clip-1"
    assert result == {"pcm": FAKE_PCM}
    assert deck.engine.renders == [("air horn", 2.0)]


def test_render_clip_refuses_while_playing(deck):
    deck.send(type="play")
    deck.next_event("audio")
    deck.send(type="render_clip", id="clip-2", prompt="air horn", seconds=2.0)
    result_id, result = deck.clip_queue.get(timeout=3.0)
    assert result_id == "clip-2"
    assert result == {"error": "deck is playing"}
    assert deck.engine.renders == []


def test_render_failure_answers_an_error_and_worker_survives(deck):
    deck.engine.fail_render = True
    deck.send(type="render_clip", id="clip-3", prompt="air horn", seconds=2.0)
    _, result = deck.clip_queue.get(timeout=3.0)
    assert result == {"error": "render failed"}

    deck.send(type="play")
    assert deck.next_event("audio") == FAKE_PCM


def test_render_clip_with_no_clip_queue_is_dropped_not_fatal():
    # A misrouted render at a queue-less deck worker has nowhere to
    # answer; it must be dropped, not crash the stream (ADR-0012).
    harness = DeckHarness(with_clip_queue=False)
    harness.thread.start()
    harness.next_event("ready")
    harness.send(type="render_clip", id="clip-9", prompt="air horn", seconds=2.0)
    harness.send(type="set_prompt", prompt="proof of life")
    assert harness.next_event("style_applied")["prompts"] == [
        {"text": "proof of life", "weight": 1.0}
    ]
    assert harness.engine.renders == []
    harness.send(type="shutdown")
    harness.thread.join(timeout=2)
    assert not harness.thread.is_alive()
