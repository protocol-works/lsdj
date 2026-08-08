#!/usr/bin/env python3
"""Reproduce the external provenance checks for sa3-pin.json.

The default audit downloads and hashes the small source/runtime archives, uses
the immutable Hugging Face revision's LFS metadata for the eight multi-GB model
objects, and checks that the dependency lock remains fully hash-pinned. Pass
--include-model-bytes for the stronger (roughly 9 GB) end-to-end model audit.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
PIN_PATH = ROOT / "sa3-pin.json"
LOCK_PATH = ROOT / "scripts" / "sa3-requirements.lock"
USER_AGENT = "LSDJ-SA3-pin-audit/1"


def fetch(url: str):
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "https" or parsed.username or parsed.password:
        raise RuntimeError(f"refusing non-HTTPS or credential-bearing URL: {url}")
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    response = urllib.request.urlopen(request, timeout=60)
    final = urllib.parse.urlparse(response.geturl())
    if final.scheme != "https":
        response.close()
        raise RuntimeError(f"redirect left HTTPS: {response.geturl()}")
    return response


def read_json(url: str) -> dict[str, Any]:
    with fetch(url) as response:
        return json.load(response)


def read_text(url: str) -> str:
    with fetch(url) as response:
        return response.read().decode("utf-8")


def audit_publisher_checksum(
    label: str, artifact: dict[str, Any], checksum_url: str
) -> None:
    filename = Path(urllib.parse.unquote(urllib.parse.urlparse(artifact["url"]).path)).name
    checksums = {}
    for line in read_text(checksum_url).splitlines():
        fields = line.split(maxsplit=1)
        if len(fields) == 2:
            checksums[fields[1].lstrip("* ")] = fields[0].lower()
    actual = checksums.get(filename)
    if actual != artifact["sha256"].lower():
        raise RuntimeError(f"{label}: pin does not match publisher checksum metadata")
    print(f"verified publisher checksum: {label} (sha256:{actual})")


def hash_url(label: str, artifact: dict[str, Any]) -> None:
    digest = hashlib.sha256()
    size = 0
    with fetch(artifact["url"]) as response:
        while chunk := response.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    actual_hash = digest.hexdigest()
    expected_hash = artifact["sha256"].lower()
    expected_size = artifact["size"]
    if (size, actual_hash) != (expected_size, expected_hash):
        raise RuntimeError(
            f"{label}: expected {expected_size} bytes/{expected_hash}, "
            f"received {size} bytes/{actual_hash}"
        )
    print(f"verified bytes: {label} ({size} bytes, sha256:{actual_hash})")


def audit_model_metadata(pin: dict[str, Any]) -> None:
    models = pin["models"]
    endpoint = (
        f"https://huggingface.co/api/models/{models['repo']}/revision/"
        f"{models['revision']}?blobs=true"
    )
    document = read_json(endpoint)
    siblings = {item["rfilename"]: item for item in document.get("siblings", [])}
    for artifact in models["artifacts"]:
        path = artifact["path"]
        sibling = siblings.get(path)
        if sibling is None:
            raise RuntimeError(f"model revision does not contain {path}")
        lfs = sibling.get("lfs") or {}
        actual_hash = lfs.get("sha256") or lfs.get("oid", "").removeprefix("sha256:")
        actual_size = lfs.get("size", sibling.get("size"))
        if (actual_size, actual_hash) != (artifact["size"], artifact["sha256"].lower()):
            raise RuntimeError(
                f"{path}: manifest does not match immutable revision LFS metadata"
            )
        print(
            f"verified LFS object: {path} "
            f"({actual_size} bytes, sha256:{actual_hash})"
        )


def audit_model_bytes(pin: dict[str, Any]) -> None:
    models = pin["models"]
    for artifact in models["artifacts"]:
        filename = artifact["path"].removeprefix("MLX/")
        direct = {
            **artifact,
            "url": (
                f"https://huggingface.co/{models['repo']}/resolve/"
                f"{models['revision']}/MLX/{filename}?download=true"
            ),
        }
        hash_url(artifact["path"], direct)


def audit_lock() -> None:
    lock = LOCK_PATH.read_text(encoding="utf-8")
    packages = re.findall(r"(?m)^[A-Za-z0-9_.-]+==[^ \\\n]+", lock)
    hashes = re.findall(r"--hash=sha256:[0-9a-f]{64}", lock)
    if not packages or not hashes:
        raise RuntimeError("dependency lock is missing packages or SHA-256 hashes")
    blocks = re.split(r"(?m)(?=^[A-Za-z0-9_.-]+==)", lock)
    unpinned = [
        block.split("==", 1)[0]
        for block in blocks
        if "==" in block and "--hash=" not in block
    ]
    if unpinned:
        raise RuntimeError(f"dependency lock entries lack hashes: {', '.join(unpinned)}")
    print(f"verified lock structure: {len(packages)} packages, {len(hashes)} SHA-256 hashes")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--include-model-bytes",
        action="store_true",
        help="download and hash all eight model objects (roughly 9 GB)",
    )
    args = parser.parse_args()
    pin = json.loads(PIN_PATH.read_text(encoding="utf-8"))

    hash_url("Stable Audio 3 source", pin["source"])
    for runtime in pin["runtime"]["uv"]:
        audit_publisher_checksum(
            f"uv {runtime['version']} ({runtime['target']})",
            runtime,
            f"{runtime['url']}.sha256",
        )
        hash_url(f"uv {runtime['version']} ({runtime['target']})", runtime)
    for runtime in pin["runtime"]["python"]:
        release_match = re.search(r"/releases/download/([^/]+)/", runtime["url"])
        if release_match is None:
            raise RuntimeError("Python runtime URL does not identify an immutable release")
        release = release_match.group(1)
        audit_publisher_checksum(
            f"Python {runtime['version']} ({runtime['target']})",
            runtime,
            "https://github.com/astral-sh/python-build-standalone/"
            f"releases/download/{release}/SHA256SUMS",
        )
        hash_url(f"Python {runtime['version']} ({runtime['target']})", runtime)
    audit_model_metadata(pin)
    if args.include_model_bytes:
        audit_model_bytes(pin)
    audit_lock()
    print("SA3 pin audit complete")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"SA3 pin audit failed: {error}", file=sys.stderr)
        raise SystemExit(1)
