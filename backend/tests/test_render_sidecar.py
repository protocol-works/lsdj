"""Bounded, authenticated protocol for the dedicated MRT2 render worker."""

import hashlib
import io
import json
import socket
import struct
import threading

import pytest

from lsdj.sidecar import (
    FRAME_AUTH,
    FRAME_RENDER_BEGIN,
    FRAME_RENDER_CANCEL,
    FRAME_RENDER_CHUNK,
    FRAME_RENDER_END,
    FRAME_RENDER_ERROR,
    FRAME_RENDER_REQUEST,
    FRAME_STATUS,
    MAX_RENDER_FRAMES,
    MAX_RENDER_PCM_BYTES,
    MAX_RENDER_PROMPT_CHARS,
    MAX_RENDER_REQUEST_BYTES,
    PYTORCH_CUDA_RUNTIME,
    RENDER_BYTES_PER_FRAME,
    RENDER_CHANNELS,
    RENDER_PCM_CHUNK_BYTES,
    RENDER_SAMPLE_RATE,
    RENDER_SCHEMA_VERSION,
    RenderProtocolError,
    RenderRequest,
    authenticate_to_host,
    read_frame,
    read_render_command,
    read_render_response,
    render_frames_for_seconds,
    run_render_worker,
    write_frame,
    write_render_response,
)

JOB_ID = "render-job-0123456789abcdef"


class RecordingSock:
    def __init__(self):
        self.buffer = bytearray()

    def sendall(self, data):
        self.buffer.extend(data)

    def setsockopt(self, *_args):
        pass


class FakeRenderEngine:
    def __init__(self):
        self.warmups = 0
        self.requests = []

    def warm_up(self):
        self.warmups += 1

    def render_clip(self, prompt, seconds):
        self.requests.append((prompt, seconds))
        frames = round(seconds * RENDER_SAMPLE_RATE)
        return b"\0" * (frames * RENDER_BYTES_PER_FRAME)


class BlockingRenderEngine(FakeRenderEngine):
    def __init__(self):
        super().__init__()
        self.started = threading.Event()
        self.release = threading.Event()

    def render_clip(self, prompt, seconds):
        self.requests.append((prompt, seconds))
        self.started.set()
        self.release.wait(timeout=5)
        return b"\0" * (round(seconds * RENDER_SAMPLE_RATE) * RENDER_BYTES_PER_FRAME)


class SignalAfterFirstChunkSock:
    """Record frame writes and optionally signal after the first PCM chunk."""

    def __init__(self, sock, signal=None):
        self.sock = sock
        self.signal = signal
        self.signalled = False
        self.frame_types = []

    def sendall(self, data):
        self.frame_types.append(data[0])
        self.sock.sendall(data)
        if (
            data[0] == FRAME_RENDER_CHUNK
            and not self.signalled
            and self.signal is not None
        ):
            self.signalled = True
            self.signal()

    def makefile(self, *args, **kwargs):
        return self.sock.makefile(*args, **kwargs)

    def shutdown(self, how):
        return self.sock.shutdown(how)


def request_payload(
    *, job_id=JOB_ID, sequence=1, prompt="bright piano", frames=24_000, **extra
):
    return json.dumps(
        {
            "schemaVersion": RENDER_SCHEMA_VERSION,
            "jobId": job_id,
            "sequence": sequence,
            "prompt": prompt,
            "frames": frames,
            **extra,
        },
        separators=(",", ":"),
    ).encode()


def cancel_payload(job_id=JOB_ID, sequence=1):
    return json.dumps(
        {
            "schemaVersion": RENDER_SCHEMA_VERSION,
            "jobId": job_id,
            "sequence": sequence,
        },
        separators=(",", ":"),
    ).encode()


def begin_payload(*, job_id=JOB_ID, sequence=1, frames=24_000, **extra):
    return json.dumps(
        {
            "schemaVersion": RENDER_SCHEMA_VERSION,
            "jobId": job_id,
            "sequence": sequence,
            "sampleRate": RENDER_SAMPLE_RATE,
            "channels": RENDER_CHANNELS,
            "sampleFormat": "f32le",
            "frames": frames,
            "pcmBytes": frames * RENDER_BYTES_PER_FRAME,
            **extra,
        },
        separators=(",", ":"),
    ).encode()


def read_ready(reader):
    frame_type, payload = read_frame(reader)
    assert frame_type == FRAME_STATUS
    assert json.loads(payload)["event"] == "render_ready"


def test_render_command_is_strict_and_bounded():
    valid = io.BytesIO(
        struct.pack("<BI", FRAME_RENDER_REQUEST, len(request_payload()))
        + request_payload()
    )
    request = read_render_command(valid)
    assert request == RenderRequest(JOB_ID, 1, "bright piano", 24_000)
    assert request.seconds == 0.5
    assert request.pcm_bytes == 24_000 * RENDER_BYTES_PER_FRAME

    cancel = RecordingSock()
    write_frame(cancel, FRAME_RENDER_CANCEL, cancel_payload())
    assert read_render_command(io.BytesIO(cancel.buffer)).sequence == 1

    invalid_payloads = [
        request_payload(prompt=" "),
        request_payload(prompt="x" * (MAX_RENDER_PROMPT_CHARS + 1)),
        request_payload(frames=23_999),
        request_payload(frames=MAX_RENDER_FRAMES + 1),
        request_payload(frames=True),
        request_payload(frames=24_000.0),
        request_payload(sequence=True),
        request_payload(sequence=1.0),
        request_payload(schemaVersion=True),
        request_payload(job_id="short"),
        request_payload(unexpected=True),
        b'{"schemaVersion":1,"schemaVersion":1,"jobId":"render-job-0123456789abcdef","sequence":1,"prompt":"p","frames":24000}',
        (
            b'{"schemaVersion":1,"jobId":"render-job-0123456789abcdef",'
            b'"sequence":1,"prompt":"p","frames":' + b"9" * 1000 + b"}"
        ),
        b"[" * 2000 + b"0" + b"]" * 2000,
    ]
    for payload in invalid_payloads:
        wire = RecordingSock()
        write_frame(wire, FRAME_RENDER_REQUEST, payload)
        with pytest.raises(RenderProtocolError):
            read_render_command(io.BytesIO(wire.buffer))


def test_render_frame_rounding_contract_uses_half_up_not_ties_to_even():
    half_frame = (24_000 + 0.5) / RENDER_SAMPLE_RATE
    assert round(half_frame * RENDER_SAMPLE_RATE) == 24_000
    assert render_frames_for_seconds(half_frame) == 24_001


def test_render_command_rejects_truncation_oversize_and_out_of_order_frames():
    with pytest.raises(RenderProtocolError, match="header is truncated"):
        read_render_command(io.BytesIO(b"\x06\x01"))

    payload = request_payload()
    with pytest.raises(RenderProtocolError, match="payload is truncated"):
        read_render_command(
            io.BytesIO(
                struct.pack("<BI", FRAME_RENDER_REQUEST, len(payload)) + payload[:-1]
            )
        )

    with pytest.raises(RenderProtocolError, match="exceeds"):
        read_render_command(
            io.BytesIO(
                struct.pack("<BI", FRAME_RENDER_REQUEST, MAX_RENDER_REQUEST_BYTES + 1)
            )
        )

    wire = RecordingSock()
    write_frame(wire, FRAME_RENDER_CHUNK, b"\0" * RENDER_BYTES_PER_FRAME)
    with pytest.raises(RenderProtocolError, match="out of order"):
        read_render_command(io.BytesIO(wire.buffer))


def test_render_response_round_trip_is_chunked_hashed_and_exact():
    request = RenderRequest(JOB_ID, 1, "piano", 3 * RENDER_SAMPLE_RATE)
    pcm = bytes(range(256)) * (request.pcm_bytes // 256)
    assert len(pcm) == request.pcm_bytes
    wire = RecordingSock()
    write_render_response(wire, request, pcm)

    reader = io.BytesIO(wire.buffer)
    assert read_render_response(reader, request, require_eof=True) == pcm
    frame_reader = io.BytesIO(wire.buffer)
    frame_types = []
    while frame := read_frame(frame_reader):
        frame_types.append(frame[0])
        if frame[0] == FRAME_RENDER_CHUNK:
            assert 0 < len(frame[1]) <= RENDER_PCM_CHUNK_BYTES
            assert len(frame[1]) % RENDER_BYTES_PER_FRAME == 0
    assert frame_types[0] == FRAME_RENDER_BEGIN
    assert frame_types[-1] == FRAME_RENDER_END
    assert frame_types.count(FRAME_RENDER_CHUNK) > 1


@pytest.mark.parametrize("delta", [-RENDER_BYTES_PER_FRAME, RENDER_BYTES_PER_FRAME])
def test_render_response_writer_rejects_short_and_extra_pcm(delta):
    request = RenderRequest(JOB_ID, 1, "piano", 24_000)
    with pytest.raises(RenderProtocolError, match="expected"):
        write_render_response(
            RecordingSock(), request, b"\0" * (request.pcm_bytes + delta)
        )


def test_render_response_reader_rejects_out_of_order_oversized_and_extra_pcm():
    request = RenderRequest(JOB_ID, 1, "piano", 24_000)
    out_of_order = RecordingSock()
    write_frame(out_of_order, FRAME_RENDER_CHUNK, b"\0" * RENDER_BYTES_PER_FRAME)
    with pytest.raises(RenderProtocolError, match="out of order"):
        read_render_response(io.BytesIO(out_of_order.buffer), request)

    oversized = bytearray()
    begin = begin_payload()
    oversized.extend(struct.pack("<BI", FRAME_RENDER_BEGIN, len(begin)) + begin)
    oversized.extend(struct.pack("<BI", FRAME_RENDER_CHUNK, RENDER_PCM_CHUNK_BYTES + 1))
    with pytest.raises(RenderProtocolError, match="exceeds"):
        read_render_response(io.BytesIO(oversized), request)

    extra = RecordingSock()
    write_frame(extra, FRAME_RENDER_BEGIN, begin_payload())
    write_frame(extra, FRAME_RENDER_CHUNK, b"\0" * request.pcm_bytes)
    write_frame(extra, FRAME_RENDER_CHUNK, b"\0" * RENDER_BYTES_PER_FRAME)
    with pytest.raises(RenderProtocolError, match="extra PCM"):
        read_render_response(io.BytesIO(extra.buffer), request)


def test_render_response_reader_rejects_truncation_bad_totals_and_frames_after_end():
    request = RenderRequest(JOB_ID, 1, "piano", 24_000)
    pcm = b"\0" * request.pcm_bytes
    wire = RecordingSock()
    write_render_response(wire, request, pcm)

    with pytest.raises(RenderProtocolError, match="truncated"):
        read_render_response(io.BytesIO(wire.buffer[:-1]), request)

    trailing = RecordingSock()
    trailing.buffer.extend(wire.buffer)
    write_frame(trailing, FRAME_RENDER_CHUNK, b"\0" * RENDER_BYTES_PER_FRAME)
    with pytest.raises(RenderProtocolError, match="after end"):
        read_render_response(io.BytesIO(trailing.buffer), request, require_eof=True)

    too_large = RecordingSock()
    frames = MAX_RENDER_PCM_BYTES // RENDER_BYTES_PER_FRAME + 1
    write_frame(too_large, FRAME_RENDER_BEGIN, begin_payload(frames=frames))
    with pytest.raises(RenderProtocolError, match="audio identity"):
        read_render_response(io.BytesIO(too_large.buffer), request)


def test_render_response_rejects_undersized_but_internally_consistent_audio():
    active = RenderRequest(JOB_ID, 1, "piano", 48_000)
    undersized = RenderRequest(JOB_ID, 1, "piano", 24_000)
    wire = RecordingSock()
    write_render_response(wire, undersized, b"\0" * undersized.pcm_bytes)
    with pytest.raises(RenderProtocolError, match="audio identity"):
        read_render_response(io.BytesIO(wire.buffer), active)


@pytest.mark.parametrize(
    "override",
    [
        {"schemaVersion": True},
        {"sequence": 1.0},
        {"sampleRate": 48_000.0},
        {"channels": True},
        {"frames": 24_000.0, "pcmBytes": 192_000.0},
        {"pcmBytes": 192_000.0},
    ],
)
def test_render_response_rejects_coercible_scalar_types(override):
    request = RenderRequest(JOB_ID, 1, "piano", 24_000)
    wire = RecordingSock()
    write_frame(wire, FRAME_RENDER_BEGIN, begin_payload(**override))
    with pytest.raises(RenderProtocolError):
        read_render_response(io.BytesIO(wire.buffer), request)


@pytest.mark.parametrize(
    "override",
    [
        {"schemaVersion": 1.0},
        {"sequence": 2},
        {"frames": 24_001},
        {"pcmBytes": 192_000.0},
        {"sha256": "0" * 64},
    ],
)
def test_render_response_end_requires_exact_active_identity_total_and_hash(override):
    request = RenderRequest(JOB_ID, 1, "piano", 24_000)
    pcm = b"\0" * request.pcm_bytes
    end = {
        "schemaVersion": RENDER_SCHEMA_VERSION,
        "jobId": request.job_id,
        "sequence": request.sequence,
        "frames": request.frames,
        "pcmBytes": request.pcm_bytes,
        "sha256": hashlib.sha256(pcm).hexdigest(),
        **override,
    }
    wire = RecordingSock()
    write_frame(wire, FRAME_RENDER_BEGIN, begin_payload())
    write_frame(wire, FRAME_RENDER_CHUNK, pcm)
    write_frame(
        wire,
        FRAME_RENDER_END,
        json.dumps(end, separators=(",", ":")).encode(),
    )
    with pytest.raises(RenderProtocolError, match="end metadata"):
        read_render_response(io.BytesIO(wire.buffer), request)


def test_render_worker_reuses_one_warm_model_for_serial_requests():
    shell, worker = socket.socketpair()
    engine = FakeRenderEngine()
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={"engine_factory": lambda model: engine},
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    try:
        read_ready(reader)
        for sequence, job_id, prompt in [
            (1, JOB_ID, "first"),
            (2, "render-job-fedcba9876543210", "second"),
        ]:
            request = RenderRequest(job_id, sequence, prompt, 24_000)
            write_frame(
                shell,
                FRAME_RENDER_REQUEST,
                request_payload(job_id=job_id, sequence=sequence, prompt=prompt),
            )
            pcm = read_render_response(reader, request)
            assert len(pcm) == 24_000 * RENDER_BYTES_PER_FRAME
        assert engine.warmups == 1
        assert engine.requests == [("first", 0.5), ("second", 0.5)]
        shell.shutdown(socket.SHUT_WR)
        thread.join(timeout=2)
        assert not thread.is_alive()
    finally:
        reader.close()
        shell.close()
        worker.close()


@pytest.mark.parametrize(
    "payload_delta", [-RENDER_BYTES_PER_FRAME, RENDER_BYTES_PER_FRAME]
)
def test_render_worker_fails_closed_on_invalid_engine_pcm(payload_delta):
    class InvalidEngine(FakeRenderEngine):
        def render_clip(self, prompt, seconds):
            expected = round(seconds * RENDER_SAMPLE_RATE) * RENDER_BYTES_PER_FRAME
            return b"\0" * (expected + payload_delta)

    shell, worker = socket.socketpair()
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={"engine_factory": lambda model: InvalidEngine()},
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    try:
        read_ready(reader)
        write_frame(shell, FRAME_RENDER_REQUEST, request_payload())
        frame_type, payload = read_frame(reader)
        assert frame_type == FRAME_RENDER_ERROR
        assert json.loads(payload)["code"] == "invalid_audio"
        thread.join(timeout=2)
        assert not thread.is_alive()
    finally:
        reader.close()
        shell.close()
        worker.close()


def test_render_worker_sanitizes_startup_and_generation_failures():
    secret = "/Users/example/private/model.safetensors"

    def fail_startup(*, model):
        raise RuntimeError(secret)

    startup_wire = RecordingSock()
    run_render_worker(startup_wire, "mrt2_small", engine_factory=fail_startup)
    frame_type, payload = read_frame(io.BytesIO(startup_wire.buffer))
    assert frame_type == FRAME_RENDER_ERROR
    assert secret not in payload.decode()

    class FailingEngine(FakeRenderEngine):
        def render_clip(self, prompt, seconds):
            raise RuntimeError(secret)

    shell, worker = socket.socketpair()
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={"engine_factory": lambda model: FailingEngine()},
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    try:
        read_ready(reader)
        write_frame(shell, FRAME_RENDER_REQUEST, request_payload())
        frame_type, payload = read_frame(reader)
        assert frame_type == FRAME_RENDER_ERROR
        parsed = json.loads(payload)
        assert parsed["code"] == "render_failed"
        assert secret not in parsed["message"]
        thread.join(timeout=2)
        assert not thread.is_alive()
    finally:
        reader.close()
        shell.close()
        worker.close()


def test_render_worker_queued_cancel_wins_before_result():
    shell, worker = socket.socketpair()
    engine = BlockingRenderEngine()
    terminated = []
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={
            "engine_factory": lambda model: engine,
            "terminate": terminated.append,
        },
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    try:
        read_ready(reader)
        write_frame(shell, FRAME_RENDER_REQUEST, request_payload())
        assert engine.started.wait(timeout=1)
        write_frame(shell, FRAME_RENDER_CANCEL, cancel_payload())
        frame_type, payload = read_frame(reader)
        assert frame_type == FRAME_RENDER_ERROR
        parsed = json.loads(payload)
        assert parsed["code"] == "cancelled"
        assert parsed["sequence"] == 1
        thread.join(timeout=2)
        assert not thread.is_alive()
        assert terminated == [2]
    finally:
        engine.release.set()
        reader.close()
        shell.close()
        worker.close()


def test_render_worker_eof_during_generation_terminates_without_a_late_response():
    shell, worker = socket.socketpair()
    engine = BlockingRenderEngine()
    terminated = []
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={
            "engine_factory": lambda model: engine,
            "terminate": terminated.append,
        },
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    try:
        read_ready(reader)
        write_frame(shell, FRAME_RENDER_REQUEST, request_payload())
        assert engine.started.wait(timeout=1)
        shell.shutdown(socket.SHUT_WR)
        thread.join(timeout=2)
        assert not thread.is_alive()
        assert terminated == [0]
    finally:
        engine.release.set()
        reader.close()
        shell.close()
        worker.close()


def test_render_worker_rejects_overlapping_requests_and_exits():
    shell, worker = socket.socketpair()
    engine = BlockingRenderEngine()
    terminated = []
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={
            "engine_factory": lambda model: engine,
            "terminate": terminated.append,
        },
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    try:
        read_ready(reader)
        write_frame(shell, FRAME_RENDER_REQUEST, request_payload())
        assert engine.started.wait(timeout=1)
        write_frame(
            shell,
            FRAME_RENDER_REQUEST,
            request_payload(job_id="render-job-overlap12345678", sequence=2),
        )
        frame_type, payload = read_frame(reader)
        assert frame_type == FRAME_RENDER_ERROR
        assert json.loads(payload)["code"] == "protocol_error"
        thread.join(timeout=2)
        assert not thread.is_alive()
        assert terminated == [2]
    finally:
        engine.release.set()
        reader.close()
        shell.close()
        worker.close()


@pytest.mark.parametrize("second_sequence", [1, 3])
def test_render_worker_rejects_duplicate_and_out_of_order_sequences(second_sequence):
    shell, worker = socket.socketpair()
    engine = FakeRenderEngine()
    terminated = []
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={
            "engine_factory": lambda model: engine,
            "terminate": terminated.append,
        },
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    try:
        read_ready(reader)
        first = RenderRequest(JOB_ID, 1, "first", 24_000)
        write_frame(shell, FRAME_RENDER_REQUEST, request_payload(prompt="first"))
        assert len(read_render_response(reader, first)) == first.pcm_bytes

        second_job = "render-job-sequence123456789"
        write_frame(
            shell,
            FRAME_RENDER_REQUEST,
            request_payload(job_id=second_job, sequence=second_sequence),
        )
        frame_type, payload = read_frame(reader)
        assert frame_type == FRAME_RENDER_ERROR
        parsed = json.loads(payload)
        assert parsed["code"] == "sequence_error"
        assert parsed["sequence"] == second_sequence
        thread.join(timeout=2)
        assert not thread.is_alive()
        assert terminated == [2]
    finally:
        reader.close()
        shell.close()
        worker.close()


def test_render_worker_cancel_during_chunks_prevents_end_and_terminates():
    shell, raw_worker = socket.socketpair()
    terminated = []
    worker = SignalAfterFirstChunkSock(
        raw_worker,
        lambda: write_frame(shell, FRAME_RENDER_CANCEL, cancel_payload()),
    )
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={
            "engine_factory": lambda model: FakeRenderEngine(),
            "terminate": terminated.append,
        },
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    frames = (RENDER_PCM_CHUNK_BYTES * 2) // RENDER_BYTES_PER_FRAME
    try:
        read_ready(reader)
        write_frame(shell, FRAME_RENDER_REQUEST, request_payload(frames=frames))
        assert read_frame(reader)[0] == FRAME_RENDER_BEGIN
        assert read_frame(reader)[0] == FRAME_RENDER_CHUNK
        trailing = read_frame(reader)
        assert trailing is None or trailing[0] == FRAME_RENDER_ERROR
        thread.join(timeout=2)
        assert not thread.is_alive()
        assert terminated == [2]
        assert FRAME_RENDER_END not in worker.frame_types
    finally:
        reader.close()
        shell.close()
        raw_worker.close()


def test_render_worker_eof_during_chunks_prevents_end_and_terminates():
    shell, raw_worker = socket.socketpair()
    worker = SignalAfterFirstChunkSock(raw_worker)
    terminated = []
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={
            "engine_factory": lambda model: FakeRenderEngine(),
            "terminate": terminated.append,
        },
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    frames = (RENDER_PCM_CHUNK_BYTES * 2) // RENDER_BYTES_PER_FRAME
    try:
        read_ready(reader)
        write_frame(shell, FRAME_RENDER_REQUEST, request_payload(frames=frames))
        assert read_frame(reader)[0] == FRAME_RENDER_BEGIN
        assert read_frame(reader)[0] == FRAME_RENDER_CHUNK
        shell.shutdown(socket.SHUT_WR)
        thread.join(timeout=2)
        assert not thread.is_alive()
        assert terminated == [0]
        assert FRAME_RENDER_END not in worker.frame_types
    finally:
        reader.close()
        shell.close()
        raw_worker.close()


def test_render_reader_unexpected_exception_is_sanitized_and_wakes_worker(monkeypatch):
    import lsdj.sidecar as sidecar_mod

    secret = "/Users/example/private/prompt-and-path"
    shell, worker = socket.socketpair()
    terminated = []
    monkeypatch.setattr(
        sidecar_mod,
        "read_render_command",
        lambda reader: (_ for _ in ()).throw(RuntimeError(secret)),
    )
    thread = threading.Thread(
        target=run_render_worker,
        args=(worker, "mrt2_small"),
        kwargs={
            "engine_factory": lambda model: FakeRenderEngine(),
            "terminate": terminated.append,
        },
        daemon=True,
    )
    thread.start()
    reader = shell.makefile("rb")
    try:
        read_ready(reader)
        frame_type, payload = read_frame(reader)
        assert frame_type == FRAME_RENDER_ERROR
        assert secret not in payload.decode()
        assert json.loads(payload)["message"] == "render control connection failed"
        thread.join(timeout=2)
        assert not thread.is_alive()
        assert terminated == [2]
    finally:
        reader.close()
        shell.close()
        worker.close()


def test_auth_capability_is_consumed_and_cannot_be_retried():
    token = "a" * 64
    env = {"LSDJ_WORKER_LAUNCH_TOKEN": token}
    connection = RecordingSock()
    authenticate_to_host(connection, env)
    assert env == {}
    assert read_frame(io.BytesIO(connection.buffer)) == (FRAME_AUTH, token.encode())
    with pytest.raises(RuntimeError, match="launch token is missing"):
        authenticate_to_host(connection, env)
    reader = io.BytesIO(connection.buffer)
    assert read_frame(reader) == (FRAME_AUTH, token.encode())
    assert read_frame(reader) is None


def test_render_worker_main_authenticates_before_starting_the_model(monkeypatch):
    import lsdj.sidecar as sidecar_mod

    connection = RecordingSock()
    captured = {}
    monkeypatch.setattr(
        sidecar_mod.socket, "create_connection", lambda address: connection
    )

    def fake_run(
        sock, model, *, runtime, engine_factory=None, terminate=sidecar_mod.os._exit
    ):
        captured.update(sock=sock, model=model, runtime=runtime)
        captured["token_present"] = (
            sidecar_mod.WORKER_TOKEN_ENV in sidecar_mod.os.environ
        )

    monkeypatch.setattr(sidecar_mod, "run_render_worker", fake_run)
    monkeypatch.setenv(sidecar_mod.WORKER_TOKEN_ENV, "a" * 64)
    sidecar_mod.main(
        [
            "--render-worker",
            "--model",
            "mrt2_small",
            "--runtime",
            PYTORCH_CUDA_RUNTIME,
            "--port",
            "5051",
        ]
    )

    assert read_frame(io.BytesIO(connection.buffer)) == (FRAME_AUTH, b"a" * 64)
    assert captured == {
        "sock": connection,
        "model": "mrt2_small",
        "runtime": PYTORCH_CUDA_RUNTIME,
        "token_present": False,
    }


def test_render_worker_main_rejects_missing_auth_and_non_pytorch_runtime(monkeypatch):
    import lsdj.sidecar as sidecar_mod

    connection = RecordingSock()
    monkeypatch.setattr(
        sidecar_mod.socket, "create_connection", lambda address: connection
    )
    monkeypatch.delenv(sidecar_mod.WORKER_TOKEN_ENV, raising=False)
    with pytest.raises(RuntimeError, match="launch token is missing"):
        sidecar_mod.main(
            [
                "--render-worker",
                "--model",
                "mrt2_small",
                "--runtime",
                PYTORCH_CUDA_RUNTIME,
                "--port",
                "5051",
            ]
        )

    with pytest.raises(SystemExit):
        sidecar_mod.main(
            [
                "--render-worker",
                "--model",
                "mrt2_small",
                "--runtime",
                "mlx",
                "--port",
                "5051",
            ]
        )
