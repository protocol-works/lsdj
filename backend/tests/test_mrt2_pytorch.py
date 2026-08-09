from pathlib import Path
from types import SimpleNamespace
import tempfile

import numpy as np
import pytest

from lsdj.engine import CHANNELS, FRAME_SECONDS, NOTE_SUSTAIN, SAMPLE_RATE
from lsdj.mrt2 import (
    MODEL_SNAPSHOTS,
    PROCESSOR_SNAPSHOT,
    RuntimeSelection,
    RuntimeUnavailable,
)
from lsdj.mrt2_pytorch import (
    GPU_ADMISSION_TIMEOUT_SECONDS,
    PytorchBindings,
    PytorchMrt2Engine,
)
from lsdj.gpu_broker import BrokerCancelled, Priority


class RecordingBroker:
    def __init__(self, events=None, *, error=None):
        self.events = events if events is not None else []
        self.error = error
        self.calls = []
        self.releases = []
        self.lease = object()

    def acquire(self, service, **kwargs):
        self.events.append("broker_acquire")
        self.calls.append((service, kwargs))
        if self.error is not None:
            raise self.error
        return self.lease

    def release(self, lease):
        self.events.append("broker_release")
        self.releases.append(lease)


DEFAULT_TEST_BROKER = object()


class FakeCuda:
    def __init__(self, available=True, events=None):
        self.available = available
        self.events = events if events is not None else []

    def is_available(self):
        self.events.append("cuda_available")
        return self.available

    def current_device(self):
        return 0

    def get_device_properties(self, _index):
        return SimpleNamespace(name="Fake NVIDIA", total_memory=12 * 1024**3)

    def get_device_capability(self, _index):
        return (9, 9)


class FakeTorch:
    bfloat16 = "bf16"

    def __init__(self, available=True, events=None):
        self.cuda = FakeCuda(available, events)
        self.version = SimpleNamespace(cuda="13.0")
        self._C = SimpleNamespace(_cuda_getDriverVersion=lambda: 13020)


class FakeProcessor:
    def __init__(self):
        self.embeds = []
        self.tokenizes = []

    def embed(self, value):
        self.embeds.append(value)
        if isinstance(value, str):
            return np.array([len(value), 2.0], dtype=np.float32)
        return np.array([10.0, 4.0], dtype=np.float32)

    def tokenize(self, embedding):
        self.tokenizes.append(np.asarray(embedding).copy())
        return np.arange(12, dtype=np.int64)


class FakeModel:
    def __init__(self, events=None, *, fail_cuda_load=False):
        self.processor = FakeProcessor()
        self.calls = []
        self.processor_path = None
        self.bad_shape = False
        self.events = events if events is not None else []
        self.fail_cuda_load = fail_cuda_load

    def to(self, device):
        assert device == "cuda"
        self.events.append("model_to_cuda")
        if self.fail_cuda_load:
            raise RuntimeError("injected CUDA allocation failure")
        return self

    def eval(self):
        return self

    def load_processor(self, path, *, device):
        self.events.append("processor_to_cuda")
        self.processor_path = (path, device)

    def generate(self, **kwargs):
        self.calls.append(kwargs)
        frames = kwargs["frames"]
        shape = (frames * 1920, CHANNELS)
        if self.bad_shape:
            shape = (frames * 1920, 1)
        return np.zeros(shape, dtype=np.float32), {"call": len(self.calls)}


class FakeAutoModel:
    def __init__(self, model, events=None):
        self.model = model
        self.calls = []
        self.events = events if events is not None else []

    def from_pretrained(self, path, **kwargs):
        self.events.append("from_pretrained")
        self.calls.append((path, kwargs))
        return self.model


def make_engine(*, cuda=True, gpu_broker=DEFAULT_TEST_BROKER, events=None, model=None):
    events = events if events is not None else []
    if gpu_broker is DEFAULT_TEST_BROKER:
        gpu_broker = RecordingBroker(events)
    model = model if model is not None else FakeModel(events)
    auto_model = FakeAutoModel(model, events)
    bindings = PytorchBindings(
        torch=FakeTorch(cuda, events),
        auto_model=auto_model,
        versions={
            "torch": "2.12.1",
            "transformers": "5.8.0",
            "huggingface_hub": "1.5.0",
        },
    )
    runtime_root = Path(tempfile.mkdtemp(prefix="lsdj-mrt2-local-"))
    model_root = runtime_root / "models" / "mrt2_small"
    processor_root = runtime_root / "models" / "musiccoca"
    model_root.mkdir(parents=True)
    processor_root.mkdir(parents=True)
    for filename in ("config.json", "model.safetensors", "modeling_magenta_rt2.py"):
        (model_root / filename).write_bytes(b"fixture")
    for filename in (
        "mel_params.npz",
        "music_encoder.pt",
        "quantizer.pt",
        "spm.model",
        "text_encoder.pt",
    ):
        (processor_root / filename).write_bytes(b"fixture")
    selection = RuntimeSelection("pytorch-cuda", "linux", "cuda", False, True)
    engine = PytorchMrt2Engine(
        selection=selection,
        bindings=bindings,
        cache_root=runtime_root,
        gpu_broker=gpu_broker,
    )
    return engine, model, auto_model, runtime_root


def test_loads_only_pinned_local_snapshots():
    engine, model, auto_model, runtime_root = make_engine()
    assert auto_model.calls[0][0] == (runtime_root / "models" / "mrt2_small").resolve()
    assert auto_model.calls[0][1] == {
        "trust_remote_code": True,
        "dtype": "bf16",
        "local_files_only": True,
    }
    assert model.processor_path[1] == "cuda"
    assert engine.diagnostics()["remote_code_revision"].startswith("7037d995")


def test_cuda_is_mandatory_and_never_falls_back_to_cpu():
    with pytest.raises(RuntimeUnavailable, match="no CPU fallback"):
        make_engine(cuda=False)


def test_missing_broker_root_fails_before_cuda_is_touched(monkeypatch):
    events = []
    monkeypatch.delenv("LSDJ_CACHE_HOME", raising=False)

    with pytest.raises(RuntimeUnavailable, match="shared GPU broker"):
        make_engine(gpu_broker=None, events=events)

    assert events == []


def test_weighted_style_and_controls_map_to_upstream_generate():
    engine, model, _, _ = make_engine()
    engine.set_style([("funk", 3.0), ("dub", 1.0)])
    engine.set_generation(0.7, 20, 3.0, 1.0)
    notes = [0] * 128
    notes[60] = 2
    engine.set_notes(notes)
    engine.set_drums(0, 5.0)
    engine.set_chunk_frames(5)
    pcm = engine.generate_chunk()

    assert len(pcm) == round(5 * FRAME_SECONDS * SAMPLE_RATE) * CHANNELS * 4
    call = model.calls[-1]
    assert call["style"] == list(range(12))
    assert call["notes"][60] == 2
    assert call["drums"] == [0]
    assert (call["temperature"], call["top_k"]) == (0.7, 20)
    assert (call["cfg_musiccoca"], call["cfg_notes"], call["cfg_drums"]) == (
        3.0,
        1.0,
        5.0,
    )
    assert call["guidance"] is True
    assert engine._notes[60] == NOTE_SUSTAIN
    np.testing.assert_allclose(model.processor.tokenizes[-1], [3.75, 2.0])


def test_reset_to_reseed_discards_continuation_state():
    engine, model, _, _ = make_engine()
    engine.generate_chunk()
    assert engine._state is not None
    engine.reset(seed=42)
    engine.generate_chunk()
    assert model.calls[-1]["seed"] == 42
    assert model.calls[-1]["state"] is None


def test_render_clip_does_not_carry_live_stream_conditioning():
    engine, model, _, _ = make_engine()
    notes = [0] * 128
    notes[60] = 2
    engine.set_notes(notes)
    engine.set_drums(1, 4.0)

    engine.render_clip("air horn", FRAME_SECONDS)

    assert model.calls[-1]["notes"] is None
    assert model.calls[-1]["drums"] is None
    assert model.calls[-1]["cfg_drums"] is None


def test_warmup_exercises_cuda_then_clears_state():
    engine, model, _, _ = make_engine()
    engine.warm_up()
    assert model.calls[-1]["frames"] == 1
    assert engine._state is None


def test_shared_deck_reuses_one_model_with_independent_continuation_state():
    first, model, _, _ = make_engine()
    second = first.shared_deck()
    assert first._system is second._system
    assert first._model_lock is second._model_lock
    assert first._gpu_broker is second._gpu_broker
    assert first._gpu_lease is second._gpu_lease

    first.generate_chunk()
    second.generate_chunk()
    assert model.calls[0]["state"] is None
    assert model.calls[1]["state"] is None
    first.generate_chunk()
    assert model.calls[2]["state"] == {"call": 1}
    assert first._state != second._state


def test_invalid_upstream_audio_shape_fails_before_pcm_handoff():
    engine, model, _, _ = make_engine()
    model.bad_shape = True
    with pytest.raises(RuntimeError, match="invalid audio shape"):
        engine.generate_chunk()


def test_diagnostics_disclose_unqualified_runtime_and_cuda_versions():
    engine, _, _, _ = make_engine()
    diagnostics = engine.diagnostics()
    model_pin = MODEL_SNAPSHOTS["mrt2_small"]
    assert diagnostics["model_repository"] == model_pin["repository"]
    assert diagnostics["model_revision"] == model_pin["revision"]
    assert diagnostics["remote_code_repository"] == model_pin["repository"]
    assert diagnostics["remote_code_revision"] == model_pin["revision"]
    assert diagnostics["processor_repository"] == PROCESSOR_SNAPSHOT["repository"]
    assert diagnostics["processor_revision"] == PROCESSOR_SNAPSHOT["revision"]
    assert diagnostics["hardware_qualified"] is False
    assert diagnostics["experimental"] is True
    assert diagnostics["torch_cuda_runtime"] == "13.0"
    assert diagnostics["nvidia_driver"] == "13.2"
    assert diagnostics["cuda_device"] == "Fake NVIDIA"
    assert diagnostics["capabilities"]["negative_prompt"] is False


def test_mrt2_model_lifetime_takes_realtime_priority_over_background_sa3():
    broker = RecordingBroker()
    engine, _, _, _ = make_engine(gpu_broker=broker)
    engine.generate_chunk()

    assert len(broker.calls) == 1, "generation must reuse the model-lifetime lease"
    service, values = broker.calls[0]
    assert service == "mrt2"
    assert values["priority"] is Priority.MRT2_REALTIME
    assert values["reservation_bytes"] == 0
    assert values["capacity_bytes"] == 0
    assert values["timeout_seconds"] == GPU_ADMISSION_TIMEOUT_SECONDS
    assert broker.releases == [], "only worker-process exit may release CUDA ownership"
    assert engine.diagnostics()["gpu_broker"] == {
        "enabled": True,
        "priority": 100,
        "preempts": "sa3-background",
    }


def test_gpu_lease_is_acquired_before_any_cuda_or_model_allocation():
    events = []
    broker = RecordingBroker(events)

    make_engine(gpu_broker=broker, events=events)

    assert events == [
        "broker_acquire",
        "cuda_available",
        "from_pretrained",
        "model_to_cuda",
        "processor_to_cuda",
    ]


def test_cancelled_gpu_admission_never_touches_cuda_or_the_model():
    events = []
    broker = RecordingBroker(events, error=BrokerCancelled("injected cancellation"))

    with pytest.raises(BrokerCancelled, match="injected cancellation"):
        make_engine(gpu_broker=broker, events=events)

    assert events == ["broker_acquire"]
    assert broker.releases == []


def test_cuda_load_failure_keeps_the_lease_until_process_cleanup():
    events = []
    broker = RecordingBroker(events)
    model = FakeModel(events, fail_cuda_load=True)

    with pytest.raises(RuntimeUnavailable, match="could not initialize on CUDA"):
        make_engine(gpu_broker=broker, events=events, model=model)

    assert events == [
        "broker_acquire",
        "cuda_available",
        "from_pretrained",
        "model_to_cuda",
    ]
    assert broker.releases == [], "failed CUDA teardown is safe only at process exit"
