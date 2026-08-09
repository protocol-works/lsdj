import json
import hashlib
import pathlib
import threading
import wave

import numpy as np
import pytest

from lsdj import sa3_cuda, sa3_cuda_worker as worker


class FakeTorch:
    @staticmethod
    def from_numpy(value):
        return value


class FakeModel:
    def __init__(self, seconds=0.5):
        self.seconds = seconds
        self.kwargs = None
        self.loras = []
        self.strengths = []

    def load_lora(self, paths):
        self.loras = paths

    def set_lora_strength(self, strength, lora_index=None):
        self.strengths.append((strength, lora_index))

    def generate(self, **kwargs):
        self.kwargs = kwargs
        for index in range(kwargs["steps"]):
            kwargs["callback"]({"i": index})
        return np.zeros(
            (1, 2, round(self.seconds * worker.SAMPLE_RATE)), dtype=np.float32
        )


def pcm16_wav(path, seconds=0.5):
    frames = round(seconds * worker.SAMPLE_RATE)
    with wave.open(str(path), "wb") as output:
        output.setnchannels(2)
        output.setsampwidth(2)
        output.setframerate(worker.SAMPLE_RATE)
        output.writeframes(b"\0" * frames * 4)


def request(tmp_path, **updates):
    launch_token = "qualification-token-with-32-bytes-minimum"
    value = {
        "schema_version": 1,
        "job_id": "sa3-job-123",
        "launch_token_sha256": hashlib.sha256(launch_token.encode()).hexdigest(),
        "prompt": "warm dub loop",
        "seconds": 0.5,
        "kind": "music",
        "steps": 8,
        "cfg": 4.5,
        "apg": 0.75,
        "seed": 123,
        "negative_prompt": "vocals",
        "init_noise_level": 0.6,
        "inpaint_range": None,
        "init_audio": None,
        "lora_files": [],
        "lora_strengths": [],
        "model_dir": str(tmp_path / "model"),
        "output": str(tmp_path / "out.wav"),
    }
    value.update(updates)
    return worker.WorkerRequest.from_dict(value)


def test_maps_every_shared_control_to_the_pinned_python_api(tmp_path):
    model = FakeModel()
    events = []
    item = request(tmp_path)
    worker.run_generation(
        item,
        model=model,
        torch_module=FakeTorch,
        cancelled=lambda: False,
        emit=events.append,
    )
    assert model.kwargs | {"callback": None} == {
        "prompt": "warm dub loop",
        "negative_prompt": "vocals",
        "duration": 0.5,
        "steps": 8,
        "cfg_scale": 4.5,
        "apg_scale": 0.75,
        "seed": 123,
        "batch_size": 1,
        "chunked_decode": True,
        "callback": None,
        "disable_tqdm": True,
        "init_audio": None,
        "init_noise_level": 0.6,
        "inpaint_audio": None,
    }
    assert events[-1] == {"event": "done"}
    with wave.open(str(item.output), "rb") as output:
        assert output.getparams()[:4] == (2, 2, 44_100, 22_050)


def test_maps_inpainting_and_continuation_to_upstream_inpaint_api(tmp_path):
    init = tmp_path / "init.wav"
    pcm16_wav(init)
    item = request(tmp_path, init_audio=str(init), inpaint_range=[0.25, 0.5])
    model = FakeModel()
    worker.run_generation(
        item,
        model=model,
        torch_module=FakeTorch,
        cancelled=lambda: False,
    )
    assert model.kwargs["init_audio"] is None
    assert model.kwargs["inpaint_audio"][0] == 44_100
    assert model.kwargs["inpaint_audio"][1].shape == (2, 22_050)
    assert model.kwargs["inpaint_mask_start_seconds"] == 0.25
    assert model.kwargs["inpaint_mask_end_seconds"] == 0.5


def test_stacked_lora_strengths_are_set_per_index(tmp_path):
    adapters = []
    for name in ("one", "two"):
        directory = tmp_path / name
        directory.mkdir()
        (directory / f"{name}.safetensors").write_bytes(b"fixture")
        adapters.append(str(directory))
    item = request(tmp_path, lora_files=adapters, lora_strengths=[0.75, 1.5])
    model = FakeModel()
    worker.run_generation(
        item,
        model=model,
        torch_module=FakeTorch,
        cancelled=lambda: False,
    )
    assert [pathlib.Path(path).name for path in model.loras] == [
        "one.safetensors",
        "two.safetensors",
    ]
    assert model.strengths == [(0.75, 0), (1.5, 1)]


def test_cancellation_is_observed_between_sampling_steps(tmp_path):
    calls = 0

    def cancelled():
        nonlocal calls
        calls += 1
        return calls >= 2

    with pytest.raises(worker.WorkerCancelled, match="cancelled"):
        worker.run_generation(
            request(tmp_path),
            model=FakeModel(),
            torch_module=FakeTorch,
            cancelled=cancelled,
        )
    assert not (tmp_path / "out.wav").exists()


def test_priority_waiter_cancels_the_disposable_worker(tmp_path):
    class Broker:
        @staticmethod
        def should_yield(_lease):
            return True

    with pytest.raises(worker.WorkerCancelled, match="yielded"):
        worker.run_generation(
            request(tmp_path),
            model=FakeModel(),
            torch_module=FakeTorch,
            cancelled=lambda: False,
            broker=Broker(),
            lease=object(),
        )


@pytest.mark.parametrize(
    "updates, message",
    [
        ({"kind": "track"}, "Small Music and Small SFX"),
        ({"steps": 0}, "steps"),
        ({"inpaint_range": [0.1, 0.2]}, "requires init_audio"),
        ({"lora_files": ["one"], "lora_strengths": []}, "LoRA stack"),
    ],
)
def test_request_contract_fails_closed(updates, message, tmp_path):
    with pytest.raises(worker.WorkerError, match=message):
        request(tmp_path, **updates)


def test_launch_token_authenticates_one_private_request_without_storing_secret(
    tmp_path,
):
    item = request(tmp_path)
    token = "qualification-token-with-32-bytes-minimum"
    worker.verify_launch_token(item, {worker.LAUNCH_TOKEN_ENV: token})
    with pytest.raises(worker.WorkerError, match="does not match"):
        worker.verify_launch_token(item, {worker.LAUNCH_TOKEN_ENV: "x" * 40})
    with pytest.raises(worker.WorkerError, match="missing or invalid"):
        worker.verify_launch_token(item, {})


def test_broker_watchdog_hard_stops_disposable_worker_during_model_load():
    exited = threading.Event()
    events = []

    class Broker:
        @staticmethod
        def should_yield(_lease):
            return True

    stop, thread = worker.start_broker_watchdog(
        Broker(),
        object(),
        events.append,
        poll_seconds=0.001,
        exit_process=lambda code: exited.set() if code == 2 else None,
    )
    assert exited.wait(1)
    thread.join(timeout=1)
    stop.set()
    assert events == [
        {
            "event": "cancelled",
            "message": "Stable Audio yielded to realtime MRT2 generation",
        }
    ]


def test_request_file_is_bounded_and_rejects_symlinks(tmp_path):
    real = tmp_path / "request.json"
    real.write_text(json.dumps({"schema_version": 1}))
    link = tmp_path / "link.json"
    link.symlink_to(real)
    with pytest.raises(worker.WorkerError, match="regular file"):
        worker.read_request(link)


def test_provenance_matches_exact_source_runtime_model_and_bundle_path(tmp_path):
    root = tmp_path / "runtime"
    model_dir = root / "models" / "small-music"
    model_dir.mkdir(parents=True)
    item = request(tmp_path, model_dir=str(model_dir))
    stamp = root / "provenance.json"
    value = {
        "schema_version": 1,
        "backend": sa3_cuda.BACKEND_NAME,
        "gated_artifacts_complete": True,
        "source_revision": sa3_cuda.SOURCE_REVISION,
        "runtime_lock_sha256": sa3_cuda.RUNTIME_LOCK_SHA256,
        "packages": sa3_cuda.EXPECTED_PACKAGES,
        "model": sa3_cuda.MODEL_PINS["music"],
    }
    stamp.write_text(json.dumps(value))

    assert worker.verify_provenance(stamp, item) == value

    value["source_revision"] = "0" * 40
    stamp.write_text(json.dumps(value))
    with pytest.raises(worker.WorkerError, match="immutable"):
        worker.verify_provenance(stamp, item)


def test_provenance_rejects_model_path_outside_verified_runtime(tmp_path):
    root = tmp_path / "runtime"
    model_dir = tmp_path / "other" / "small-music"
    model_dir.mkdir(parents=True)
    root.mkdir()
    item = request(tmp_path, model_dir=str(model_dir))
    stamp = root / "provenance.json"
    stamp.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "backend": sa3_cuda.BACKEND_NAME,
                "gated_artifacts_complete": True,
                "source_revision": sa3_cuda.SOURCE_REVISION,
                "runtime_lock_sha256": sa3_cuda.RUNTIME_LOCK_SHA256,
                "packages": sa3_cuda.EXPECTED_PACKAGES,
                "model": sa3_cuda.MODEL_PINS["music"],
            }
        )
    )

    with pytest.raises(worker.WorkerError, match="outside"):
        worker.verify_provenance(stamp, item)
