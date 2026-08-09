"""Sidecar transport tests: the loopback-TCP framing + the queue adapters that
bridge the socket to run_deck_worker, exercised against a socketpair with a fake
engine — no model, no Rust process."""

import io
import json
import socket
import struct
import threading
import time

import pytest

from lsdj.sidecar import (
    FRAME_CONTROL,
    FRAME_AUTH,
    FRAME_EMBED,
    FRAME_PCM,
    FRAME_STATUS,
    SharedSocketCmdQueues,
    SocketCmdQueue,
    SocketOutQueue,
    authenticate_to_host,
    read_frame,
    run_sidecar,
    run_shared_sidecar,
    write_frame,
)

FAKE_PCM = b"\x01\x02\x03\x04" * 8


class FakeEngine:
    """The run_deck_worker engine contract, enough for the transport test."""

    # The worker paces on the engine's chunk length (ADR-0023).
    chunk_seconds = 1.0

    def __init__(self, model="fake"):
        self.styles = []

    def set_style(self, prompts, sample_keys=frozenset()):
        self.styles.append(prompts)

    def set_notes(self, notes):
        pass

    def set_drums(self, flag):
        pass

    def generate_chunk(self):
        return FAKE_PCM


class RecordingSock:
    """A stand-in socket that records framed sends (SocketOutQueue only calls
    sendall)."""

    def __init__(self):
        self.buffer = bytearray()

    def sendall(self, data):
        self.buffer.extend(data)


def test_frame_round_trips_through_a_buffer():
    sock = RecordingSock()
    write_frame(sock, FRAME_STATUS, b'{"event":"ready"}')
    write_frame(sock, FRAME_PCM, b"\x00\x01\x02\x03")

    reader = io.BytesIO(bytes(sock.buffer))
    assert read_frame(reader) == (FRAME_STATUS, b'{"event":"ready"}')
    assert read_frame(reader) == (FRAME_PCM, b"\x00\x01\x02\x03")
    # Clean EOF at a boundary → None.
    assert read_frame(reader) is None


def test_read_frame_returns_none_on_truncated_payload():
    # A header promising 16 bytes but only 4 present → truncation → None.
    head = struct.pack("<BI", FRAME_PCM, 16)
    reader = io.BytesIO(head + b"\x00\x00\x00\x00")
    assert read_frame(reader) is None


def test_frames_are_bounded_in_both_directions(monkeypatch):
    import lsdj.sidecar as sidecar_mod

    monkeypatch.setattr(sidecar_mod, "MAX_FRAME_BYTES", 4)
    with pytest.raises(ValueError, match="exceeds the cap"):
        sidecar_mod.write_frame(RecordingSock(), FRAME_PCM, b"12345")
    with pytest.raises(ValueError, match="exceeds the cap"):
        sidecar_mod.read_frame(io.BytesIO(struct.pack("<BI", FRAME_PCM, 5)))
    with pytest.raises(ValueError, match="unknown sidecar frame type"):
        sidecar_mod.write_frame(RecordingSock(), 255, b"")


def test_authentication_frame_comes_from_the_launch_environment():
    sock = RecordingSock()
    token = "a" * 64
    authenticate_to_host(sock, {"LSDJ_WORKER_LAUNCH_TOKEN": token})
    assert read_frame(io.BytesIO(bytes(sock.buffer))) == (FRAME_AUTH, token.encode())
    with pytest.raises(RuntimeError, match="launch token is missing"):
        authenticate_to_host(RecordingSock(), {})


def test_out_queue_maps_audio_and_status_to_frames():
    sock = RecordingSock()
    out = SocketOutQueue(sock)
    out.put(("audio", b"\xaa\xbb\xcc\xdd"))
    out.put(("status", {"event": "chunk", "index": 3}))

    reader = io.BytesIO(bytes(sock.buffer))
    ftype, payload = read_frame(reader)
    assert ftype == FRAME_PCM
    assert payload == b"\xaa\xbb\xcc\xdd"
    ftype, payload = read_frame(reader)
    assert ftype == FRAME_STATUS
    assert json.loads(payload) == {"event": "chunk", "index": 3}


def test_cmd_queue_parses_control_frames_and_shutdown_on_eof():
    # Two control frames then EOF; the adapter yields the parsed dicts, then a
    # synthetic shutdown so the worker loop exits.
    wire = bytearray()
    rec = RecordingSock()
    rec.buffer = wire
    write_frame(rec, FRAME_CONTROL, b'{"type":"play"}')
    write_frame(rec, FRAME_CONTROL, b'{"type":"stop"}')
    # A non-control frame must be ignored.
    write_frame(rec, FRAME_PCM, b"\x00\x00\x00\x00")

    cmd = SocketCmdQueue(io.BytesIO(bytes(wire)))
    assert cmd.get(timeout=1.0) == {"type": "play"}
    assert cmd.get(timeout=1.0) == {"type": "stop"}
    assert cmd.get(timeout=1.0) == {"type": "shutdown"}


def test_cmd_queue_passes_note_conditioning_through_intact():
    # The pump is a generic JSON pass-through; pin that ADR-0023's
    # full-state conditioning payloads reach the worker unmodified.
    multihot = [0] * 128
    multihot[60] = 3
    wire = bytearray()
    rec = RecordingSock()
    rec.buffer = wire
    write_frame(
        rec,
        FRAME_CONTROL,
        json.dumps({"type": "set_notes", "notes": multihot}).encode(),
    )
    write_frame(rec, FRAME_CONTROL, b'{"type":"set_drums","drums":null}')

    cmd = SocketCmdQueue(io.BytesIO(bytes(wire)))
    assert cmd.get(timeout=1.0) == {"type": "set_notes", "notes": multihot}
    assert cmd.get(timeout=1.0) == {"type": "set_drums", "drums": None}


def _read_frames_until(sock_file, predicate, timeout=3.0):
    """Read frames until `predicate(ftype, payload)` is true; returns that frame."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        frame = read_frame(sock_file)
        if frame is None:
            raise AssertionError("socket closed before the expected frame")
        if predicate(*frame):
            return frame
    raise AssertionError("timed out waiting for the expected frame")


def test_sidecar_streams_pcm_and_status_over_a_socketpair():
    # The whole sidecar transport end to end: a socketpair stands in for the
    # Rust↔sidecar link; a fake engine stands in for the model.
    shell, side = socket.socketpair()
    try:
        thread = threading.Thread(
            target=run_sidecar,
            args=(side, "a", "fake"),
            kwargs={"engine_factory": lambda model: FakeEngine(model)},
            daemon=True,
        )
        thread.start()

        shell_reader = shell.makefile("rb")
        # The worker announces 'ready' first.
        ftype, payload = _read_frames_until(
            shell_reader, lambda t, p: t == FRAME_STATUS and b"ready" in p
        )
        assert json.loads(payload)["event"] == "ready"

        # Drive a style + play; expect PCM frames to start flowing.
        write_frame(
            shell,
            FRAME_CONTROL,
            json.dumps(
                {"type": "set_style", "prompts": [{"text": "techno", "weight": 1.0}]}
            ).encode(),
        )
        write_frame(shell, FRAME_CONTROL, json.dumps({"type": "play"}).encode())

        ftype, payload = _read_frames_until(shell_reader, lambda t, p: t == FRAME_PCM)
        assert payload == FAKE_PCM
        write_frame(shell, FRAME_CONTROL, b'{"type":"shutdown"}')
        thread.join(timeout=2)
        assert not thread.is_alive()
    finally:
        # Closing the shell end → the sidecar's reader hits EOF → shutdown.
        shell.close()
        side.close()


def test_sidecar_main_argument_parsing(monkeypatch):
    # `main` parses --deck/--model/--port and dials the loopback port; stub the
    # connect + run so no real model loads.
    captured = {}

    def fake_create_connection(addr):
        captured["addr"] = addr
        return RecordingSock()

    def fake_run(sock, deck, model, *, runtime="auto", engine_factory=None):
        captured["deck"] = deck
        captured["model"] = model
        captured["runtime"] = runtime

    import lsdj.sidecar as sidecar_mod

    monkeypatch.setattr(sidecar_mod.socket, "create_connection", fake_create_connection)
    # RecordingSock has no setsockopt; give it a no-op.
    monkeypatch.setattr(
        RecordingSock, "setsockopt", lambda *a, **k: None, raising=False
    )
    monkeypatch.setattr(sidecar_mod, "run_sidecar", fake_run)
    monkeypatch.setenv("LSDJ_WORKER_LAUNCH_TOKEN", "a" * 64)

    sidecar_mod.main(
        [
            "--deck",
            "b",
            "--model",
            "mrt2_small",
            "--runtime",
            "mlx",
            "--port",
            "5050",
        ]
    )
    assert captured["addr"] == ("127.0.0.1", 5050)
    assert captured["deck"] == "b"
    assert captured["model"] == "mrt2_small"
    assert captured["runtime"] == "mlx"
    assert "LSDJ_WORKER_LAUNCH_TOKEN" not in sidecar_mod.os.environ


def test_cmd_queue_decodes_embed_frame_to_embed_sample():
    # A FRAME_EMBED ([u32 id_len][id][pcm]) becomes an embed_sample command the
    # worker handles (M15 style sampling routed to the sidecar in native).
    sample_id = b"sample:a:1"
    pcm = b"\x00\x01\x02\x03\x04\x05\x06\x07"
    payload = len(sample_id).to_bytes(4, "little") + sample_id + pcm
    rec = RecordingSock()
    write_frame(rec, FRAME_EMBED, payload)

    cmd = SocketCmdQueue(io.BytesIO(bytes(rec.buffer)))
    assert cmd.get(timeout=1.0) == {
        "type": "embed_sample",
        "id": "sample:a:1",
        "pcm": pcm,
    }


def test_cmd_queue_rejects_malformed_embed_ids_without_desynchronizing():
    rec = RecordingSock()
    # Declared ID extends past the payload, then an invalid UTF-8 ID. Both must
    # be ignored while the following valid control frame remains readable.
    write_frame(rec, FRAME_EMBED, (99).to_bytes(4, "little") + b"tiny")
    write_frame(rec, FRAME_EMBED, (1).to_bytes(4, "little") + b"\xff" + FAKE_PCM)
    write_frame(rec, FRAME_CONTROL, b'{"type":"play"}')

    cmd = SocketCmdQueue(io.BytesIO(bytes(rec.buffer)))
    assert cmd.get(timeout=1.0) == {"type": "play"}
    assert cmd.get(timeout=1.0) == {"type": "shutdown"}


def test_shared_command_stream_demultiplexes_by_deck():
    rec = RecordingSock()
    write_frame(rec, FRAME_CONTROL, b'\x01{"type":"play"}')
    write_frame(rec, FRAME_CONTROL, b'\x00{"type":"stop"}')

    commands = SharedSocketCmdQueues(io.BytesIO(bytes(rec.buffer)))
    assert commands.queues[1].get(timeout=1.0) == {"type": "play"}
    assert commands.queues[0].get(timeout=1.0) == {"type": "stop"}
    assert commands.queues[0].get(timeout=1.0) == {"type": "shutdown"}
    assert commands.queues[1].get(timeout=1.0) == {"type": "shutdown"}


def test_shared_sidecar_multiplexes_two_decks_over_one_socket():
    shell, side = socket.socketpair()
    try:
        thread = threading.Thread(
            target=run_shared_sidecar,
            args=(side, ("same", "same")),
            kwargs={
                "runtime": "fake",
                "engine_factory": lambda model: FakeEngine(model),
            },
            daemon=True,
        )
        thread.start()
        reader = shell.makefile("rb")
        ready_decks = set()
        warming_decks = set()
        while ready_decks != {0, 1}:
            frame_type, payload = read_frame(reader)
            if frame_type == FRAME_STATUS and b'"warming"' in payload:
                warming_decks.add(payload[0])
            if frame_type == FRAME_STATUS and b'"ready"' in payload:
                ready_decks.add(payload[0])
        assert warming_decks == {0, 1}

        write_frame(shell, FRAME_CONTROL, b'\x01{"type":"play"}')
        while True:
            frame_type, payload = read_frame(reader)
            if frame_type == FRAME_PCM:
                assert payload[0] == 1
                assert payload[1:] == FAKE_PCM
                break
        write_frame(shell, FRAME_CONTROL, b'\x00{"type":"shutdown"}')
        write_frame(shell, FRAME_CONTROL, b'\x01{"type":"shutdown"}')
        thread.join(timeout=2)
        assert not thread.is_alive()
    finally:
        shell.close()
        side.close()
