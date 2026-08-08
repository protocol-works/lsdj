"""Fail-closed checks for the pinned official TFLite runtime manifest."""

import json
import pathlib
import re

from lsdj import sa3
from lsdj.sa3_contract import GenerationRequest

ROOT = pathlib.Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "sa3-tflite-pin.json"
LOCK = ROOT / "scripts" / "sa3-tflite-requirements.lock"


def load_manifest() -> dict:
    return json.loads(MANIFEST.read_text())


def test_runtime_and_models_use_immutable_official_revisions():
    manifest = load_manifest()
    runtime = manifest["runtime"]
    models = manifest["models"]
    assert runtime["repo"] == "https://github.com/Stability-AI/stable-audio-3"
    assert re.fullmatch(r"[0-9a-f]{40}", runtime["revision"])
    assert models["repo"] == "stabilityai/stable-audio-3-optimized"
    assert re.fullmatch(r"[0-9a-f]{40}", models["revision"])
    assert runtime["repo"] == sa3.TFLITE_RUNTIME_REPO
    assert runtime["revision"] == sa3.TFLITE_RUNTIME_REVISION
    assert models["repo"] == sa3.TFLITE_MODELS_REPO
    assert models["revision"] == sa3.TFLITE_MODELS_REVISION


def test_every_model_asset_has_a_safe_path_exact_size_and_sha256():
    models = load_manifest()["models"]
    assets = [*models["shared"]]
    for bundle in models["bundles"].values():
        assets.extend(bundle)
    for asset in assets:
        assert asset["size"] > 0
        assert re.fullmatch(r"[0-9a-f]{64}", asset["sha256"])
        install_path = pathlib.PurePosixPath(asset["installPath"])
        assert not install_path.is_absolute()
        assert ".." not in install_path.parts
        assert asset["path"].startswith("tflite/")


def test_adapter_preflight_paths_match_the_pinned_manifest():
    manifest = load_manifest()["models"]
    installed = {entry["installPath"] for entry in manifest["shared"]}
    for entries in manifest["bundles"].values():
        installed.update(entry["installPath"] for entry in entries)
    required = set()
    for kind in ("sfx", "music", "track"):
        request = GenerationRequest("fixture", 0.5, kind, init_audio=b"wav")
        required.update(str(path) for path in sa3._required_tflite_assets(request))
    required.remove("models/tokenizer.model")
    assert required == installed


def test_measured_bundle_storage_totals_are_stable():
    models = load_manifest()["models"]
    shared = sum(entry["size"] for entry in models["shared"])
    totals = {
        name: shared + sum(entry["size"] for entry in entries)
        for name, entries in models["bundles"].items()
    }
    assert totals == {
        "sm-music": 2_836_149_512,
        "sm-sfx": 2_836_149_512,
        "medium": 10_027_905_456,
    }


def test_runtime_lock_is_hash_pinned_and_covers_official_direct_dependencies():
    lock = LOCK.read_text()
    for package in (
        "ai-edge-litert",
        "numpy",
        "sentencepiece",
        "soundfile",
        "huggingface-hub",
    ):
        assert re.search(rf"(?m)^{re.escape(package)}==", lock)
    assert "--hash=sha256:" in lock
    requirements = [
        line
        for line in lock.splitlines()
        if line and not line[0].isspace() and not line.startswith("#")
    ]
    assert requirements
    assert all("==" in requirement for requirement in requirements)
