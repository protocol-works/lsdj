#!/usr/bin/env python3
"""Audit the fail-closed Windows SA3/CUDA candidate manifest.

This audit deliberately fails for a release while any gated artifact lacks an
application-controlled SHA-256.  ``--allow-incomplete`` is only for reviewing
the public, immutable metadata before issue #108 supplies an authenticated
terms/download flow; it does not make the runtime installable.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
PIN_PATH = ROOT / "sa3-pytorch-cuda-pin.json"
REQUIREMENT = re.compile(r"(?m)^([A-Za-z0-9_.-]+)==([^ \\\n]+) \\")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def audit_lock(pin: dict) -> None:
    runtime = pin["sharedRuntime"]
    lock_path = ROOT / runtime["requirementsLock"]
    if (lock_path.stat().st_size, sha256(lock_path)) != (
        runtime["requirementsLockSize"],
        runtime["requirementsLockSha256"],
    ):
        raise RuntimeError("shared runtime lock size or SHA-256 does not match the pin")
    text = lock_path.read_text(encoding="utf-8")
    requirements = dict(REQUIREMENT.findall(text))
    if runtime["packages"].items() > requirements.items():
        raise RuntimeError("shared runtime direct package pins do not match the lock")
    if any(value in text for value in ("git+", "http://", " @ ", "--editable")):
        raise RuntimeError("shared runtime lock contains a mutable dependency")
    blocks = re.split(r"(?m)(?=^[A-Za-z0-9_.-]+==)", text)
    if any("==" in block and "--hash=sha256:" not in block for block in blocks):
        raise RuntimeError("shared runtime lock contains an unhashed dependency")


def missing_artifact_hashes(pin: dict) -> list[str]:
    missing = []
    for model_name, model in pin["models"].items():
        if not (model.get("required") or model.get("enabled")):
            continue
        for artifact_name in ("weight", "config"):
            artifact = model[artifact_name]
            if artifact.get("sha256") is None:
                missing.append(f"{model_name}/{artifact['path']}")
    return missing


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--allow-incomplete",
        action="store_true",
        help="review the non-release candidate even though gated hashes are missing",
    )
    args = parser.parse_args()
    pin = json.loads(PIN_PATH.read_text(encoding="utf-8"))
    if pin.get("schemaVersion") != 1 or pin.get("backend") != "pytorch_cuda":
        raise RuntimeError("unsupported Windows SA3/CUDA pin schema")
    if pin.get("platform") != "windows-x86_64":
        raise RuntimeError("the CUDA candidate must be Windows x64 only")
    if pin["source"]["revision"] not in pin["source"]["archiveUrl"]:
        raise RuntimeError("source archive URL is not tied to the immutable revision")
    audit_lock(pin)
    missing = missing_artifact_hashes(pin)
    complete = not missing
    if pin.get("gatedArtifactsComplete") is not complete:
        raise RuntimeError("gatedArtifactsComplete disagrees with artifact hashes")
    if pin.get("releaseReady") and (not complete or pin["releaseBlockers"]):
        raise RuntimeError("releaseReady cannot be true while gates remain")
    if missing and not args.allow_incomplete:
        raise RuntimeError(
            "gated artifact SHA-256 values are missing: " + ", ".join(missing)
        )
    print(
        "SA3/CUDA pin audit complete"
        + (" (candidate remains release-blocked)" if missing else "")
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"SA3/CUDA pin audit failed: {error}", file=sys.stderr)
        raise SystemExit(1)
