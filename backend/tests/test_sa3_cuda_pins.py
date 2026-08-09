import hashlib
import json
import re
from pathlib import Path

from lsdj import sa3_cuda


ROOT = Path(__file__).parents[2]
PIN_PATH = ROOT / "sa3-pytorch-cuda-pin.json"
LOCK_PATH = ROOT / "backend/runtime-locks/windows-gpu-pytorch.txt"
REQUIREMENT = re.compile(r"^([a-z0-9][a-z0-9_.-]*)==([^ \\]+) \\$", re.MULTILINE)


def test_shared_windows_runtime_is_hash_locked_and_matches_executable_policy():
    pin = json.loads(PIN_PATH.read_text())
    lock = LOCK_PATH.read_bytes()
    runtime = pin["sharedRuntime"]
    requirements = dict(REQUIREMENT.findall(lock.decode()))

    assert len(requirements) == 44
    assert runtime["packages"] == sa3_cuda.EXPECTED_PACKAGES
    assert runtime["packages"].items() <= requirements.items()
    assert runtime["requirementsLockSize"] == len(lock)
    assert runtime["requirementsLockSha256"] == hashlib.sha256(lock).hexdigest()
    assert runtime["requirementsLockSha256"] == sa3_cuda.RUNTIME_LOCK_SHA256
    assert lock.count(b"--hash=sha256:") >= len(requirements)
    assert not any(token in lock for token in (b"git+", b"http://", b" @ "))


def test_candidate_cannot_be_released_with_missing_gated_hashes():
    pin = json.loads(PIN_PATH.read_text())
    missing = [
        f"{model_name}/{artifact['path']}"
        for model_name, model in pin["models"].items()
        if model.get("required") or model.get("enabled")
        for artifact in (model["weight"], model["config"])
        if artifact["sha256"] is None
    ]

    assert missing == [
        "small-music/model_config.json",
        "small-sfx/model_config.json",
    ]
    assert pin["gatedArtifactsComplete"] is False
    assert pin["releaseReady"] is False
    assert pin["releaseBlockers"]


def test_cuda_manifest_uses_upstream_as_an_external_immutable_dependency():
    pin = json.loads(PIN_PATH.read_text())
    source = pin["source"]

    assert source["repository"] == "https://github.com/Stability-AI/stable-audio-3"
    assert source["revision"] == sa3_cuda.SOURCE_REVISION
    assert source["revision"] in source["archiveUrl"]
    assert source["license"] == "MIT"
    pinned_models = {
        (model["repository"], model["revision"]) for model in pin["models"].values()
    }
    assert {
        (model["repository"], model["revision"])
        for model in sa3_cuda.MODEL_PINS.values()
    } <= pinned_models
