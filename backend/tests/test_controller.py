"""Generation server tests: input validation at the trust boundary for the
render and generate RPC. A fake render worker stands in for the spawned
process; the lifespan (which would shut down a real worker) is harmless to
enter without one.
"""

import asyncio
import io
import json
import queue
import wave

import pytest
from fastapi.testclient import TestClient

from lsdj import controller, sa3


class FakeProcess:
    def __init__(self):
        self.alive = True

    def is_alive(self):
        return self.alive

    def terminate(self):
        self.alive = False

    def join(self, timeout=None):
        pass


class FakeRenderWorker:
    """The third-engine worker, answering render commands immediately."""

    def __init__(self):
        self.cmd_queue = queue.Queue()
        self.clip_queue = queue.Queue()
        self.render_lock = asyncio.Lock()
        self.process = FakeProcess()
        self.ready = False
        self.ready_waits = 0
        # The worker half of the round-trip: a configured response
        # answers any render_clip command.
        self.render_response = None

    def await_ready(self):
        self.ready_waits += 1
        self.ready = True

    def send(self, command):
        self.cmd_queue.put(command)
        if command.get("type") == "render_clip" and self.render_response is not None:
            self.clip_queue.put((command["id"], self.render_response))


@pytest.fixture
def client():
    return TestClient(controller.app)


# --- /api/generate (M18, ADR-0012) ---------------------------------------


def generate_request(**overrides):
    body = {"prompt": "vinyl spinback", "seconds": 3.0, "kind": "sfx"}
    body.update(overrides)
    return body


def pcm16_wav(*, sample_rate=44_100, channels=2, sample_width=2, frames=16) -> bytes:
    out = io.BytesIO()
    with wave.open(out, "wb") as target:
        target.setframerate(sample_rate)
        target.setnchannels(channels)
        target.setsampwidth(sample_width)
        target.writeframes(b"\x00" * frames * channels * sample_width)
    return out.getvalue()


def generate_multipart(metadata, audio=None, extra=()):
    fields = [
        ("request", (None, json.dumps(metadata))),
        (
            "init_audio",
            ("source.wav", pcm16_wav() if audio is None else audio, "audio/wav"),
        ),
    ]
    fields.extend(extra)
    return fields


def test_generate_returns_wav_and_strips_the_prompt(client, monkeypatch):
    calls = []

    async def fake_generate(prompt, seconds, kind):
        calls.append((prompt, seconds, kind))
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate", json=generate_request(prompt="  deep house loop  ")
    )
    assert response.status_code == 200
    assert response.headers["content-type"] == "audio/wav"
    assert response.content == b"RIFFwav"
    assert calls == [("deep house loop", 3.0, "sfx")]


def test_generate_forwards_optional_json_controls(client, monkeypatch):
    calls = []

    async def fake_generate(prompt, seconds, kind, **options):
        calls.append((prompt, seconds, kind, options))
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate",
        json=generate_request(
            init_noise_level=0.6,
            negative_prompt="  vocals  ",
            cfg=4.5,
            apg=0.75,
            seed=12345,
        ),
    )
    assert response.status_code == 200
    assert calls == [
        (
            "vinyl spinback",
            3.0,
            "sfx",
            {
                "init_noise_level": 0.6,
                "cfg": 4.5,
                "apg": 0.75,
                "negative_prompt": "vocals",
                "seed": 12345,
            },
        )
    ]


@pytest.fixture
def lora_registry(tmp_path, monkeypatch):
    """A registry pointed at via the env override: two small adapters, one
    medium, plus five smalls (stack0-4) for the over-the-cap case."""
    slugs = [("small", "crackle"), ("small", "hiss"), ("medium", "maqam")]
    slugs += [("small", f"stack{n}") for n in range(5)]
    for base, slug in slugs:
        adapter_dir = tmp_path / base / slug
        adapter_dir.mkdir(parents=True)
        (adapter_dir / "adapter_model.safetensors").write_bytes(b"fake")
    monkeypatch.setenv("SA3_LORAS_HOME", str(tmp_path))
    return tmp_path


def test_generate_forwards_the_lora_stack_with_aligned_strengths(
    client, monkeypatch, lora_registry
):
    # Two adapters ride together; a missing strength defaults to 1.0 so the
    # dirs and strengths lists stay aligned position by position.
    calls = []

    async def fake_generate(prompt, seconds, kind, **options):
        calls.append(options)
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate",
        json=generate_request(
            loras=[
                {"name": "small/crackle", "strength": 1.5},
                {"name": "small/hiss"},
            ]
        ),
    )
    assert response.status_code == 200
    assert calls == [
        {
            "lora_dirs": [
                str(lora_registry / "small" / "crackle"),
                str(lora_registry / "small" / "hiss"),
            ],
            "lora_strengths": [1.5, 1.0],
        }
    ]


@pytest.mark.parametrize("strength", [0.0, 4.0])
def test_generate_accepts_the_lora_strength_boundaries(
    client, monkeypatch, lora_registry, strength
):
    # 0 is the bit-exact bypass (ADR-0028), 4 the guard-rail ceiling.
    calls = []

    async def fake_generate(prompt, seconds, kind, **options):
        calls.append(options)
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate",
        json=generate_request(loras=[{"name": "small/crackle", "strength": strength}]),
    )
    assert response.status_code == 200
    assert calls[0]["lora_strengths"] == [strength]


def test_generate_treats_an_empty_lora_stack_as_no_adapters(
    client, monkeypatch, lora_registry
):
    calls = []

    async def fake_generate(prompt, seconds, kind, **options):
        calls.append(options)
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post("/api/generate", json=generate_request(loras=[]))
    assert response.status_code == 200
    assert calls == [{}]


@pytest.mark.parametrize(
    "loras",
    [
        {"name": "small/crackle"},  # not an array
        ["small/crackle"],  # entry not an object
        [{"name": None}],
        [{"name": "small/absent"}],  # not installed
        [{"name": "small/../crackle"}],  # traversal
        [{"name": "medium/maqam"}],  # medium adapter cannot ride an sfx (small) DiT
        [{"name": "small/crackle", "strength": -0.1}],
        [{"name": "small/crackle", "strength": 4.1}],
        [{"name": "small/crackle", "strength": True}],
        [{"name": "small/crackle", "strength": "1"}],
        # A repeated adapter would double its delta silently.
        [{"name": "small/crackle"}, {"name": "small/crackle"}],
        # One over MAX_LORA_STACK, and every name resolves — only the cap trips.
        [{"name": f"small/stack{n}"} for n in range(5)],
    ],
)
def test_generate_rejects_invalid_lora_stacks(
    client, monkeypatch, lora_registry, loras
):
    async def fake_generate(prompt, seconds, kind, **options):  # pragma: no cover
        raise AssertionError("invalid input must not reach generation")

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post("/api/generate", json=generate_request(loras=loras))
    assert response.status_code == 422


def test_generate_rejects_a_nan_lora_strength(client, monkeypatch, lora_registry):
    # httpx's json= encoder refuses NaN, but Python's json.loads parses it —
    # so it can reach the server, and the boundary must catch it.
    async def fake_generate(prompt, seconds, kind, **options):  # pragma: no cover
        raise AssertionError("invalid input must not reach generation")

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate",
        content=(
            '{"prompt": "x", "seconds": 3, "kind": "sfx", '
            '"loras": [{"name": "small/crackle", "strength": NaN}]}'
        ),
        headers={"content-type": "application/json"},
    )
    assert response.status_code == 422


@pytest.mark.parametrize("channels", [1, 2])
def test_generate_forwards_multipart_init_audio_and_inpaint(
    client, monkeypatch, channels
):
    calls = []
    source = pcm16_wav(channels=channels)
    metadata = generate_request(
        seconds=3.0,
        init_noise_level=0.55,
        inpaint_range=[0.0, 3.0],
        seed=7,
    )

    async def fake_generate(prompt, seconds, kind, **options):
        calls.append((prompt, seconds, kind, options))
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post("/api/generate", files=generate_multipart(metadata, source))
    assert response.status_code == 200
    assert calls == [
        (
            "vinyl spinback",
            3.0,
            "sfx",
            {
                "init_noise_level": 0.55,
                "seed": 7,
                "inpaint_range": (0.0, 3.0),
                "init_audio": source,
            },
        )
    ]


def test_generate_accepts_the_optional_control_boundaries(client, monkeypatch):
    calls = []

    async def fake_generate(prompt, seconds, kind, **options):
        calls.append(options)
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate",
        json=generate_request(
            init_noise_level=sa3.MIN_INIT_NOISE_LEVEL,
            negative_prompt="kick",
            cfg=sa3.MIN_CFG,
            apg=sa3.MIN_APG,
            seed=sa3.MAX_SEED,
        ),
    )
    assert response.status_code == 200
    assert calls == [
        {
            "init_noise_level": sa3.MIN_INIT_NOISE_LEVEL,
            "cfg": sa3.MIN_CFG,
            "apg": sa3.MIN_APG,
            "negative_prompt": "kick",
            "seed": sa3.MAX_SEED,
        }
    ]


def test_generate_accepts_the_optional_control_upper_boundaries(client, monkeypatch):
    calls = []

    async def fake_generate(prompt, seconds, kind, **options):
        calls.append(options)
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate",
        json=generate_request(
            init_noise_level=sa3.MAX_INIT_NOISE_LEVEL,
            negative_prompt="kick",
            cfg=sa3.MAX_CFG,
            apg=sa3.MAX_APG,
            seed=0,
        ),
    )
    assert response.status_code == 200
    assert calls == [
        {
            "init_noise_level": sa3.MAX_INIT_NOISE_LEVEL,
            "cfg": sa3.MAX_CFG,
            "apg": sa3.MAX_APG,
            "negative_prompt": "kick",
            "seed": 0,
        }
    ]


@pytest.mark.parametrize(
    "body",
    [
        generate_request(prompt=""),
        generate_request(prompt="   "),
        generate_request(prompt=7),
        generate_request(prompt="x" * (sa3.MAX_PROMPT_LENGTH + 1)),
        generate_request(kind="banger"),
        generate_request(kind=None),
        generate_request(kind=[]),
        generate_request(seconds=0.1),
        generate_request(seconds=33.0),
        generate_request(kind="track", seconds=381.0),
        generate_request(seconds=True),
        generate_request(seconds="3"),
        "not an object",
    ],
)
def test_generate_validates_the_trust_boundary(client, monkeypatch, body):
    async def fake_generate(prompt, seconds, kind):  # pragma: no cover
        raise AssertionError("invalid input must not reach generation")

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post("/api/generate", json=body)
    assert response.status_code == 422


def test_generate_accepts_a_track_at_track_length(client, monkeypatch):
    # M19 (ADR-0013): 'track' runs the medium DiT with the 6:20 ceiling,
    # while pad kinds keep the small-model 32 s bound.
    calls = []

    async def fake_generate(prompt, seconds, kind):
        calls.append((prompt, seconds, kind))
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate", json=generate_request(kind="track", seconds=380.0)
    )
    assert response.status_code == 200
    assert calls == [("vinyl spinback", 380.0, "track")]


def test_generate_rejects_nan_seconds(client, monkeypatch):
    # httpx's json= encoder refuses NaN, but Python's json.loads parses it —
    # so it can reach the server, and the boundary must catch it.
    async def fake_generate(prompt, seconds, kind):  # pragma: no cover
        raise AssertionError("invalid input must not reach generation")

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate",
        content='{"prompt": "x", "seconds": NaN, "kind": "sfx"}',
        headers={"content-type": "application/json"},
    )
    assert response.status_code == 422


@pytest.mark.parametrize(
    "overrides",
    [
        {"init_noise_level": None},
        {"init_noise_level": True},
        {"init_noise_level": 0.009},
        {"init_noise_level": 5.01},
        {"init_noise_level": float("nan")},
        {"cfg": None},
        {"cfg": True},
        {"cfg": -20.01},
        {"cfg": 20.01},
        {"cfg": float("inf")},
        {"apg": None},
        {"apg": True},
        {"apg": -0.01, "cfg": 2},
        {"apg": 1.01, "cfg": 2},
        {"apg": 0.5},
        {"apg": 0.5, "cfg": 1},
        {"negative_prompt": None, "cfg": 2},
        {"negative_prompt": 7, "cfg": 2},
        {"negative_prompt": "vocals"},
        {"negative_prompt": "vocals", "cfg": 1},
        {"negative_prompt": "x" * (sa3.MAX_PROMPT_LENGTH + 1), "cfg": 2},
        {"seed": None},
        {"seed": True},
        {"seed": 1.5},
        {"seed": -1},
        {"seed": sa3.MAX_SEED + 1},
        {"inpaint_range": None},
        {"inpaint_range": []},
        {"inpaint_range": [0]},
        {"inpaint_range": [0, 1, 2]},
        {"inpaint_range": [True, 1]},
        {"inpaint_range": [0, float("nan")]},
        {"inpaint_range": [-1, 1]},
        {"inpaint_range": [2, 1]},
        {"inpaint_range": [0, 4]},
        {"inpaint_range": [0, 1]},
    ],
)
def test_generate_rejects_invalid_optional_controls(client, monkeypatch, overrides):
    async def fake_generate(prompt, seconds, kind, **options):  # pragma: no cover
        raise AssertionError("invalid input must not reach generation")

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate",
        content=json.dumps(generate_request(**overrides)),
        headers={"content-type": "application/json"},
    )
    assert response.status_code == 422


def test_generate_treats_a_blank_negative_prompt_as_absent(client, monkeypatch):
    calls = []

    async def fake_generate(prompt, seconds, kind, **options):
        calls.append(options)
        return b"RIFFwav"

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate", json=generate_request(negative_prompt="   ")
    )
    assert response.status_code == 200
    assert calls == [{}]


@pytest.mark.parametrize(
    "audio",
    [
        b"",
        b"not a wave",
        pcm16_wav(sample_rate=48_000),
        pcm16_wav(channels=3),
        pcm16_wav(sample_width=1),
        pcm16_wav(frames=0),
        pcm16_wav()[:-4],
    ],
)
def test_generate_rejects_unsupported_init_wav(client, monkeypatch, audio):
    async def fake_generate(prompt, seconds, kind, **options):  # pragma: no cover
        raise AssertionError("invalid audio must not reach generation")

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post(
        "/api/generate", files=generate_multipart(generate_request(), audio)
    )
    assert response.status_code == 422


def test_generate_rejects_an_oversized_init_file(client, monkeypatch):
    monkeypatch.setattr(sa3, "MAX_INIT_AUDIO_BYTES", 48)
    response = client.post(
        "/api/generate", files=generate_multipart(generate_request(), pcm16_wav())
    )
    assert response.status_code == 413


def test_generate_rejects_an_oversized_multipart_body_before_parsing(
    client, monkeypatch
):
    monkeypatch.setattr(controller, "MAX_MULTIPART_BODY_BYTES", 1)
    response = client.post(
        "/api/generate", files=generate_multipart(generate_request())
    )
    assert response.status_code == 413


def test_generate_rejects_oversized_multipart_metadata(client, monkeypatch):
    monkeypatch.setattr(sa3, "MAX_GENERATE_METADATA_BYTES", 8)
    response = client.post(
        "/api/generate", files=generate_multipart(generate_request())
    )
    assert response.status_code == 413


def test_generate_rejects_an_oversized_json_body(client, monkeypatch):
    monkeypatch.setattr(sa3, "MAX_GENERATE_METADATA_BYTES", 8)
    response = client.post("/api/generate", json=generate_request())
    assert response.status_code == 413


def test_generate_bounds_a_chunked_multipart_stream_without_content_length(monkeypatch):
    monkeypatch.setattr(controller, "MAX_MULTIPART_BODY_BYTES", 5)
    messages = iter(
        [
            {"type": "http.request", "body": b"123", "more_body": True},
            {"type": "http.request", "body": b"456", "more_body": False},
        ]
    )

    async def receive():
        return next(messages)

    request = controller.Request(
        {
            "type": "http",
            "method": "POST",
            "path": "/api/generate",
            "headers": [],
        },
        receive,
    )
    with pytest.raises(controller.HTTPException) as caught:
        asyncio.run(controller._bounded_multipart_request(request))
    assert caught.value.status_code == 413


@pytest.mark.parametrize(
    "files",
    [
        [("request", (None, json.dumps(generate_request())))],
        [("init_audio", ("source.wav", pcm16_wav(), "audio/wav"))],
        generate_multipart(
            generate_request(),
            extra=[("extra", (None, "surprise"))],
        ),
        [
            ("request", (None, json.dumps(generate_request()))),
            ("request", (None, json.dumps(generate_request()))),
            ("init_audio", ("source.wav", pcm16_wav(), "audio/wav")),
        ],
    ],
)
def test_generate_rejects_bad_multipart_shape(client, files):
    response = client.post("/api/generate", files=files)
    assert response.status_code == 422


def test_generate_rejects_bad_content_types_and_malformed_bodies(client):
    unsupported = client.post(
        "/api/generate", content="hello", headers={"content-type": "text/plain"}
    )
    malformed_json = client.post(
        "/api/generate", content="{", headers={"content-type": "application/json"}
    )
    malformed_multipart = client.post(
        "/api/generate",
        content="not multipart",
        headers={"content-type": "multipart/form-data"},
    )
    assert unsupported.status_code == 422
    assert malformed_json.status_code == 422
    assert malformed_multipart.status_code == 422


def test_generate_maps_missing_checkout_to_503(client, monkeypatch):
    async def fake_generate(prompt, seconds, kind):
        raise controller.sa3.GenerationUnavailable("setup hint")

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post("/api/generate", json=generate_request())
    assert response.status_code == 503
    assert "setup hint" in response.json()["detail"]


def test_generate_maps_cli_failure_to_502(client, monkeypatch):
    async def fake_generate(prompt, seconds, kind):
        raise controller.sa3.GenerationFailed("error: no DiT weights found")

    monkeypatch.setattr(controller.sa3, "generate", fake_generate)
    response = client.post("/api/generate", json=generate_request())
    assert response.status_code == 502
    assert "no DiT weights" in response.json()["detail"]


# --- /api/render (M18, the third Magenta engine) --------------------------


@pytest.fixture
def render_worker(monkeypatch):
    fake = FakeRenderWorker()
    monkeypatch.setitem(controller.render_state, "worker", fake)
    return fake


def test_float32_wav_wraps_the_pcm_exactly():
    pcm = b"\x00\x00\x80\x3f" * 4  # four float32 ones
    wav = controller.float32_wav(pcm, 48_000, 2)
    assert wav[:4] == b"RIFF"
    assert wav[8:16] == b"WAVEfmt "
    assert int.from_bytes(wav[20:22], "little") == 3  # IEEE float
    assert int.from_bytes(wav[22:24], "little") == 2
    assert int.from_bytes(wav[24:28], "little") == 48_000
    assert int.from_bytes(wav[40:44], "little") == len(pcm)
    assert wav[44:] == pcm


def test_render_returns_the_worker_clip_as_wav(client, render_worker):
    render_worker.render_response = {"pcm": b"\x00" * 16}
    response = client.post("/api/render", json={"prompt": " air horn ", "seconds": 2.0})
    assert response.status_code == 200
    assert response.headers["content-type"] == "audio/wav"
    assert response.content[44:] == b"\x00" * 16
    assert render_worker.ready_waits == 1  # first use waits for the model
    command = render_worker.cmd_queue.get_nowait()
    assert command["type"] == "render_clip"
    assert command["prompt"] == "air horn"
    assert command["seconds"] == 2.0


def test_render_maps_worker_failure_to_502(client, render_worker):
    render_worker.render_response = {"error": "render failed"}
    response = client.post("/api/render", json={"prompt": "air horn", "seconds": 2.0})
    assert response.status_code == 502


def test_render_discards_a_stale_answer_in_the_queue(client, render_worker):
    # A timed-out render answered late; the next request must not be
    # served someone else's clip.
    render_worker.clip_queue.put(("clip-old", {"pcm": b"\xff" * 8}))
    render_worker.render_response = {"pcm": b"\x00" * 8}
    response = client.post("/api/render", json={"prompt": "air horn", "seconds": 2.0})
    assert response.status_code == 200
    assert response.content[44:] == b"\x00" * 8


def test_render_respawns_a_dead_worker(client, render_worker, monkeypatch):
    render_worker.process.alive = False
    spawned = FakeRenderWorker()
    spawned.render_response = {"pcm": b"\x00" * 8}
    # ensure_render_worker sees the dead process and builds a fresh one.
    monkeypatch.setattr(controller, "RenderProcess", lambda: spawned)

    response = client.post("/api/render", json={"prompt": "x", "seconds": 2.0})
    assert response.status_code == 200
    assert spawned.ready_waits == 1


def test_render_timeout_scales_with_length_above_a_floor():
    # M19 (ADR-0013): tracks render for minutes at the measured 1.86×
    # real time; short clips keep the flat pad deadline.
    assert controller.render_timeout_for(2.0) == controller.RENDER_TIMEOUT_SECONDS
    assert controller.render_timeout_for(180.0) == 360.0


def test_render_accepts_a_track_up_to_the_cap(client, render_worker):
    render_worker.render_response = {"pcm": b"\x00" * 8}
    response = client.post("/api/render", json={"prompt": "x", "seconds": 180.0})
    assert response.status_code == 200


def test_render_timeout_kills_the_wedged_worker(client, render_worker, monkeypatch):
    # No configured response: the worker never answers — wedged. The kill
    # plus reset lets the next request respawn clean instead of burning
    # the full timeout against the same wedge (and a late answer from a
    # merely-slow worker can never land in a stranger's request).
    monkeypatch.setattr(controller, "render_timeout_for", lambda seconds: 0.05)
    response = client.post("/api/render", json={"prompt": "air horn", "seconds": 2.0})
    assert response.status_code == 502
    assert not render_worker.process.is_alive()
    assert controller.render_state["worker"] is None


def test_render_fails_fast_when_handed_a_dead_worker(
    client, render_worker, monkeypatch
):
    # A request queued on the lock can hold a worker another request just
    # killed; the in-lock liveness check answers at once instead of
    # burning the render timeout against the corpse.
    def handed_a_corpse():
        render_worker.process.alive = False
        return render_worker

    monkeypatch.setattr(controller, "ensure_render_worker", handed_a_corpse)
    monkeypatch.setattr(controller, "render_timeout_for", lambda seconds: 0.05)
    response = client.post("/api/render", json={"prompt": "air horn", "seconds": 2.0})
    assert response.status_code == 502
    assert response.json()["detail"] == "render engine died"
    assert controller.render_state["worker"] is None


def test_render_start_failure_discards_the_worker(client, render_worker):
    def never_ready():
        raise queue.Empty

    render_worker.await_ready = never_ready
    response = client.post("/api/render", json={"prompt": "air horn", "seconds": 2.0})
    assert response.status_code == 502
    assert not render_worker.process.is_alive()
    assert controller.render_state["worker"] is None


@pytest.mark.parametrize(
    "body",
    [
        {"prompt": "", "seconds": 2.0},
        {"prompt": "x" * (sa3.MAX_PROMPT_LENGTH + 1), "seconds": 2.0},
        {"prompt": "x", "seconds": 0.1},
        {"prompt": "x", "seconds": 181.0},
        {"prompt": "x", "seconds": True},
        "not an object",
    ],
)
def test_render_validates_the_trust_boundary(client, render_worker, body):
    render_worker.render_response = {"pcm": b"\x00" * 8}
    response = client.post("/api/render", json=body)
    assert response.status_code == 422
    assert render_worker.cmd_queue.empty()


def test_models_endpoint_returns_list_and_ram(client, monkeypatch):
    """The native model picker fetches /api/models (no /ws/deck hello in native)."""
    monkeypatch.setattr(
        controller.engine, "available_models", lambda: ["mrt2_small", "mrt2_base"]
    )
    response = client.get("/api/models")
    assert response.status_code == 200
    body = response.json()
    assert body["models"] == ["mrt2_small", "mrt2_base"]
    assert body["sample_rate"] == 48000
    assert body["total_ram_gb"] > 0
    assert "mrt2_small" in body["model_ram_estimate_gb"]
