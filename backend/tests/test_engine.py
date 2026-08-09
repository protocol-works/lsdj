"""DeckEngine.set_style blending logic, with the model stubbed out (the
constructor loads MLX weights, so tests build the instance directly)."""

from types import SimpleNamespace

import numpy as np
import pytest

from lsdj.engine import (
    CFG_MUSICCOCA,
    CFG_NOTES,
    DeckEngine,
    FRAMES_PER_CHUNK,
    TEMPERATURE,
    TOP_K,
)


AUDIO_EMBEDDING = np.array([9.0, -9.0])


def make_engine(embeddings: dict[str, np.ndarray]):
    calls = []

    def embed_style(text_or_audio):
        calls.append(text_or_audio)
        if isinstance(text_or_audio, str):
            return embeddings[text_or_audio]
        return AUDIO_EMBEDDING  # a Waveform (M15 sample path)

    engine = DeckEngine.__new__(DeckEngine)
    engine._embed_cache = {}
    engine._samples = {}
    engine._system = SimpleNamespace(embed_style=embed_style)
    engine._style = None
    engine._state = None
    engine._notes = None
    engine._drums = None
    engine._drums_cfg = None
    engine._temperature = TEMPERATURE
    engine._top_k = TOP_K
    engine._cfg_musiccoca = CFG_MUSICCOCA
    engine._cfg_notes = CFG_NOTES
    engine._chunk_frames = FRAMES_PER_CHUNK
    return engine, calls


def sample_pcm(seconds: float) -> bytes:
    frames = int(seconds * 48_000)
    return np.zeros(frames * 2, dtype="<f4").tobytes()


def test_mlx_diagnostics_explicitly_advertise_upstream_negative_prompt_limit():
    engine, _ = make_engine({})
    engine._model = "mrt2_small"
    assert engine.diagnostics()["capabilities"]["negative_prompt"] is False


def test_constructor_uses_reference_sampling_defaults(monkeypatch):
    # The tuning knobs are set once on the system (not per generate) — adopting
    # the magenta-realtime app defaults, not the raw library floor. Lock them so
    # a silent revert to the library constructor defaults is caught. cfg_drums
    # is the baseline for an unsteered deck; the drum-sit control overrides it
    # per generate_chunk when a flag is active (issue #50).
    from magenta_rt.mlx import system

    captured = {}

    def fake_system(**kwargs):
        captured.update(kwargs)
        return SimpleNamespace()

    monkeypatch.setattr(system, "MagentaRT2SystemMlxfn", fake_system)
    DeckEngine(model="mrt2_small")
    assert captured["temperature"] == 1.1
    assert captured["top_k"] == 50
    assert captured["cfg_musiccoca"] == 1.6
    assert captured["cfg_notes"] == 2.4
    assert captured["cfg_drums"] == 4.0


def test_blends_weighted_embeddings_normalized():
    engine, _ = make_engine(
        {"funk": np.array([1.0, 0.0]), "techno": np.array([0.0, 1.0])}
    )
    engine.set_style([("funk", 3.0), ("techno", 1.0)])
    np.testing.assert_allclose(engine._style, [0.75, 0.25])


def test_embeddings_are_cached_across_morph_moves():
    engine, calls = make_engine(
        {"funk": np.array([1.0, 0.0]), "techno": np.array([0.0, 1.0])}
    )
    for mix in (0.2, 0.5, 0.8):
        engine.set_style([("funk", 1 - mix), ("techno", mix)])
    assert sorted(calls) == ["funk", "techno"]  # one embed per text, not per move


def test_cache_evicts_least_recently_used_not_oldest(monkeypatch):
    import lsdj.engine as engine_module

    monkeypatch.setattr(engine_module, "EMBED_CACHE_SIZE", 2)
    engine, calls = make_engine(
        {
            "funk": np.array([1.0]),
            "techno": np.array([2.0]),
            "dub": np.array([3.0]),
        }
    )
    engine.set_style([("funk", 1.0)])
    engine.set_style([("techno", 1.0)])
    engine.set_style([("funk", 1.0)])  # refresh funk's recency
    engine.set_style([("dub", 1.0)])  # evicts techno, not funk
    engine.set_style([("funk", 1.0)])
    assert calls == ["funk", "techno", "dub"]  # funk never re-embedded


def test_zero_weight_prompts_are_dropped():
    engine, calls = make_engine({"funk": np.array([1.0, 0.0])})
    engine.set_style([("funk", 1.0), ("techno", 0.0)])
    assert calls == ["funk"]
    np.testing.assert_allclose(engine._style, [1.0, 0.0])


def test_all_zero_weights_rejected():
    engine, _ = make_engine({})
    with pytest.raises(ValueError):
        engine.set_style([("funk", 0.0)])


def test_embed_sample_then_blend_with_text():
    engine, _ = make_engine({"funk": np.array([1.0, 1.0])})
    engine.embed_sample("sample:a:1", sample_pcm(4))
    engine.set_style(
        [("funk", 0.5), ("sample:a:1", 0.5)],
        sample_keys=frozenset({"sample:a:1"}),
    )
    np.testing.assert_allclose(engine._style, [5.0, -4.0])  # mean of the two


def test_sample_key_never_hits_the_text_embedder():
    engine, calls = make_engine({})
    engine.embed_sample("sample:a:1", sample_pcm(4))
    engine.set_style([("sample:a:1", 1.0)], sample_keys=frozenset({"sample:a:1"}))
    assert all(not isinstance(call, str) for call in calls)
    np.testing.assert_allclose(engine._style, AUDIO_EMBEDDING)


def test_unknown_sample_id_is_a_clear_error():
    engine, _ = make_engine({})
    with pytest.raises(ValueError, match="re-sample"):
        engine.set_style([("sample:gone", 1.0)], sample_keys=frozenset({"sample:gone"}))


def test_embed_sample_rejects_malformed_pcm():
    engine, _ = make_engine({})
    with pytest.raises(ValueError):
        engine.embed_sample("s", b"\x00" * 6)  # not whole stereo frames
    with pytest.raises(ValueError):
        engine.embed_sample("s", sample_pcm(1))  # under the minimum
    with pytest.raises(ValueError):
        engine.embed_sample("s", sample_pcm(20))  # over the maximum


def test_sample_cache_is_capped(monkeypatch):
    import lsdj.engine as engine_module

    monkeypatch.setattr(engine_module, "SAMPLE_CACHE_SIZE", 2)
    engine, _ = make_engine({})
    for index in range(3):
        engine.embed_sample(f"sample:{index}", sample_pcm(4))
    assert list(engine._samples) == ["sample:1", "sample:2"]


def test_sample_cache_evicts_least_recently_used_not_oldest(monkeypatch):
    # A sample still on the pad is touched by every style send, so it
    # must never be the eviction victim while it is live.
    import lsdj.engine as engine_module

    monkeypatch.setattr(engine_module, "SAMPLE_CACHE_SIZE", 2)
    engine, _ = make_engine({})
    engine.embed_sample("sample:live", sample_pcm(4))
    engine.embed_sample("sample:old", sample_pcm(4))
    engine.set_style([("sample:live", 1.0)], sample_keys=frozenset({"sample:live"}))
    engine.embed_sample("sample:new", sample_pcm(4))
    assert list(engine._samples) == ["sample:live", "sample:new"]


def test_failed_embed_does_not_evict(monkeypatch):
    import lsdj.engine as engine_module

    monkeypatch.setattr(engine_module, "SAMPLE_CACHE_SIZE", 1)
    engine, _ = make_engine({})
    engine.embed_sample("sample:kept", sample_pcm(4))

    def explode(_):
        raise RuntimeError("musiccoca blew up")

    engine._system = SimpleNamespace(embed_style=explode)
    with pytest.raises(RuntimeError):
        engine.embed_sample("sample:new", sample_pcm(4))
    assert list(engine._samples) == ["sample:kept"]


def make_streaming_engine():
    """An engine whose generate() records the conditioning it was handed."""
    engine, _ = make_engine({})
    generate_calls = []

    def generate(
        style=None,
        notes=None,
        drums=None,
        cfg_drums=None,
        temperature=None,
        top_k=None,
        cfg_musiccoca=None,
        cfg_notes=None,
        frames=None,
        state=None,
    ):
        # A copy: generate_chunk mutates self._notes in place after the call
        # (the onset decay), so recording the live list would show the decayed
        # state, not what this chunk was handed.
        generate_calls.append(
            {
                "notes": None if notes is None else list(notes),
                "drums": drums,
                "cfg_drums": cfg_drums,
                "temperature": temperature,
                "top_k": top_k,
                "cfg_musiccoca": cfg_musiccoca,
                "cfg_notes": cfg_notes,
            }
        )
        return (
            SimpleNamespace(samples=np.zeros((48_000, 2), dtype=np.float32)),
            "stream-state",
        )

    engine._system.generate = generate
    return engine, generate_calls


def test_generate_chunk_is_masked_until_steered():
    engine, calls = make_streaming_engine()
    engine.generate_chunk()
    assert (calls[0]["notes"], calls[0]["drums"], calls[0]["cfg_drums"]) == (
        None,
        None,
        None,
    )


def test_generate_chunk_passes_the_baseline_generation_params():
    # An untouched deck generates at the reference operating point (issue #84):
    # the tuning knobs reach generate() every chunk, defaulting to the baseline
    # until set_generation overrides them.
    engine, calls = make_streaming_engine()
    engine.generate_chunk()
    assert (
        calls[0]["temperature"],
        calls[0]["top_k"],
        calls[0]["cfg_musiccoca"],
        calls[0]["cfg_notes"],
    ) == (TEMPERATURE, TOP_K, CFG_MUSICCOCA, CFG_NOTES)


def test_set_generation_reaches_every_chunk_until_changed():
    engine, calls = make_streaming_engine()
    engine.set_generation(temperature=0.7, top_k=20, cfg_musiccoca=3.0, cfg_notes=1.0)
    engine.generate_chunk()
    engine.generate_chunk()
    for call in calls:
        assert (
            call["temperature"],
            call["top_k"],
            call["cfg_musiccoca"],
            call["cfg_notes"],
        ) == (0.7, 20, 3.0, 1.0)
    # Reset-to-baseline is a plain set back to the reference constants.
    engine.set_generation(
        temperature=TEMPERATURE,
        top_k=TOP_K,
        cfg_musiccoca=CFG_MUSICCOCA,
        cfg_notes=CFG_NOTES,
    )
    engine.generate_chunk()
    assert calls[-1]["temperature"] == TEMPERATURE


def test_set_generation_clamps_temperature_off_zero():
    # A temperature of 0 divides the sampling logits by zero; the engine floors
    # it at MIN_TEMPERATURE so the lowest live value still generates.
    from lsdj.engine import MIN_TEMPERATURE

    engine, calls = make_streaming_engine()
    engine.set_generation(
        temperature=0.0, top_k=TOP_K, cfg_musiccoca=CFG_MUSICCOCA, cfg_notes=CFG_NOTES
    )
    engine.generate_chunk()
    assert calls[0]["temperature"] == MIN_TEMPERATURE


def test_set_generation_rejects_bad_values():
    engine, _ = make_engine({})
    with pytest.raises(ValueError, match="top_k"):
        engine.set_generation(
            temperature=1.0, top_k=0, cfg_musiccoca=1.6, cfg_notes=2.4
        )
    with pytest.raises(ValueError, match="top_k"):
        engine.set_generation(
            temperature=1.0, top_k=True, cfg_musiccoca=1.6, cfg_notes=2.4
        )
    with pytest.raises(ValueError, match="cfg_musiccoca"):
        engine.set_generation(
            temperature=1.0, top_k=50, cfg_musiccoca=7.5, cfg_notes=2.4
        )
    with pytest.raises(ValueError, match="cfg_notes"):
        engine.set_generation(
            temperature=1.0, top_k=50, cfg_musiccoca=1.6, cfg_notes=-2.0
        )


def test_set_notes_applies_to_every_chunk_until_changed():
    engine, calls = make_streaming_engine()
    multihot = [0] * 128
    multihot[60] = 3
    engine.set_notes(multihot)
    engine.generate_chunk()
    engine.generate_chunk()
    assert [call["notes"] for call in calls] == [multihot, multihot]
    engine.set_notes(None)
    engine.generate_chunk()
    assert calls[-1]["notes"] is None


def test_set_drums_wraps_the_flag_for_the_model():
    engine, calls = make_streaming_engine()
    engine.set_drums(0)
    engine.generate_chunk()
    engine.set_drums(1)
    engine.generate_chunk()
    engine.set_drums(None)
    engine.generate_chunk()
    assert [call["drums"] for call in calls] == [[0], [1], None]


def test_drum_cfg_always_reaches_generate():
    # Drums Adherence (issue #50) always guides the drum conditioning — like
    # the reference, independent of the suppress flag. It falls back to the
    # constructor baseline (None) only before it has ever been set.
    engine, calls = make_streaming_engine()
    engine.generate_chunk()  # never set → baseline
    engine.set_drums(0, 5.0)  # suppress, adherence 5
    engine.generate_chunk()
    engine.set_drums(None, 3.0)  # auto — adherence still applies
    engine.generate_chunk()
    assert [call["cfg_drums"] for call in calls] == [None, 5.0, 3.0]


def test_set_drums_rejects_out_of_range_cfg():
    engine, _ = make_engine({})
    with pytest.raises(ValueError, match="drum cfg"):
        engine.set_drums(0, 7.5)
    with pytest.raises(ValueError, match="drum cfg"):
        engine.set_drums(0, -2.0)


def test_held_onset_decays_to_sustain_after_the_first_chunk():
    # Issue #46/#48: a held press marked onset (2) must sound its attack once,
    # then continue as sustain (1) — otherwise the engine re-applies "first
    # time" every chunk and the model re-attacks the note at the chunk rate.
    engine, calls = make_streaming_engine()
    multihot = [0] * 128
    multihot[60] = 2  # a fresh onset on C4
    engine.set_notes(multihot)
    engine.generate_chunk()
    engine.generate_chunk()
    assert calls[0]["notes"][60] == 2  # first chunk attacks
    assert calls[1]["notes"][60] == 1  # then decays to a sustain
    # The held-state itself decayed, so a later change re-onsets deliberately.
    assert engine._notes[60] == 1


def test_chord_follow_state_does_not_decay():
    # State 3 (model-decides) is not an onset: it stays every chunk so the
    # model keeps its attack freedom — the forgiving chord-follow default.
    engine, calls = make_streaming_engine()
    multihot = [0] * 128
    multihot[60] = 3
    engine.set_notes(multihot)
    engine.generate_chunk()
    engine.generate_chunk()
    assert calls[0]["notes"][60] == 3
    assert calls[1]["notes"][60] == 3


def test_set_notes_is_full_state_not_a_reference():
    # The engine must hold its own copy: a sender mutating the list it
    # passed cannot desync the held state (the idempotence ADR-0023 needs).
    engine, calls = make_streaming_engine()
    multihot = [0] * 128
    multihot[60] = 3
    engine.set_notes(multihot)
    multihot[60] = 0
    engine.generate_chunk()
    assert calls[0]["notes"][60] == 3


def test_set_notes_rejects_bad_shapes_and_values():
    engine, _ = make_engine({})
    with pytest.raises(ValueError, match="128"):
        engine.set_notes([0] * 127)
    with pytest.raises(ValueError, match="-1, 0, 1, 2, or 3"):
        engine.set_notes([0] * 127 + [4])
    with pytest.raises(ValueError, match="0, 1, or None"):
        engine.set_drums(2)


def test_chunk_frames_knob_changes_generation_length():
    # The ADR-0023 performance knob: the default is the 1 s chunk; the knob
    # shrinks the next generate() call and the pacing unit together.
    engine, _ = make_engine({})
    frames_seen = []

    def generate(style=None, frames=None, state=None, **_extra):
        frames_seen.append(frames)
        return (
            SimpleNamespace(samples=np.zeros((48_000, 2), dtype=np.float32)),
            "stream-state",
        )

    engine._system.generate = generate
    engine.generate_chunk()
    engine.set_chunk_frames(5)
    engine.generate_chunk()
    assert frames_seen == [FRAMES_PER_CHUNK, 5]
    assert engine.chunk_seconds == pytest.approx(0.2)


def test_set_chunk_frames_rejects_bad_values():
    engine, _ = make_engine({})
    for bad in (0, FRAMES_PER_CHUNK + 1, 2.5, True):
        with pytest.raises(ValueError, match="chunk frames"):
            engine.set_chunk_frames(bad)


def test_render_clip_never_carries_the_stream_conditioning():
    # A standalone clip is independent of the live stream (ADR-0012):
    # held note/drum steering must not leak into it.
    engine, _ = make_engine({"air horn": np.array([1.0, 0.0])})
    kwargs_seen = []

    def generate(style=None, frames=None, state=None, **extra):
        kwargs_seen.append(extra)
        return (
            SimpleNamespace(samples=np.zeros((48_000, 2), dtype=np.float32)),
            None,
        )

    engine._system.generate = generate
    engine.set_notes([3] * 128)
    engine.set_drums(1)
    engine.render_clip("air horn", 1.0)
    # The note/drum stream conditioning is absent from a clip render...
    for key in ("notes", "drums", "cfg_drums"):
        assert key not in kwargs_seen[0]


def test_render_clip_carries_the_deck_tuning():
    # ...but the deck's tuned sampling/guidance DOES shape its pad renders
    # (issue #84): the character carries into rendered clips.
    engine, _ = make_engine({"air horn": np.array([1.0, 0.0])})
    kwargs_seen = []

    def generate(style=None, frames=None, state=None, **extra):
        kwargs_seen.append(extra)
        return (
            SimpleNamespace(samples=np.zeros((48_000, 2), dtype=np.float32)),
            None,
        )

    engine._system.generate = generate
    engine.set_generation(temperature=0.7, top_k=20, cfg_musiccoca=3.0, cfg_notes=1.0)
    engine.render_clip("air horn", 1.0)
    assert kwargs_seen[0] == {
        "temperature": 0.7,
        "top_k": 20,
        "cfg_musiccoca": 3.0,
        "cfg_notes": 1.0,
    }


def test_render_clip_leaves_the_stream_untouched():
    engine, _ = make_engine({"air horn": np.array([1.0, 0.0])})
    chunk_calls = []

    def generate(style=None, frames=None, state=None, **_extra):
        chunk_calls.append((frames, state))
        samples = np.full((48_000, 2), 0.5, dtype=np.float32)
        return SimpleNamespace(samples=samples), "clip-state"

    engine._system.generate = generate
    engine._state = "stream-state"
    engine._style = "stream-style"

    pcm = engine.render_clip("air horn", 1.5)
    # Trimmed to exactly 1.5 s of interleaved stereo float32.
    assert len(pcm) == int(1.5 * 48_000) * 2 * 4
    # ceil(1.5 / CHUNK_SECONDS) = 2 chunks, threaded through a state of
    # their own...
    assert [frames for frames, _ in chunk_calls] == [25, 25]
    assert chunk_calls[1][1] == "clip-state"
    # ...while the live stream's continuity stays untouched.
    assert engine._state == "stream-state"
    assert engine._style == "stream-style"


def test_render_clip_reuses_the_text_embed_cache():
    engine, calls = make_engine({"air horn": np.array([1.0, 0.0])})
    engine._system.generate = lambda style=None, frames=None, state=None, **_extra: (
        SimpleNamespace(samples=np.zeros((48_000, 2), dtype=np.float32)),
        None,
    )
    engine.render_clip("air horn", 1.0)
    engine.render_clip("air horn", 1.0)
    assert calls == ["air horn"]
