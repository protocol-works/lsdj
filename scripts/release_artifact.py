#!/usr/bin/env python3
"""Create and verify fail-closed release producer bundles.

Release producers never hand a bare installer to the publisher.  Each producer
uploads a directory containing its assets, a checksum file, and metadata that
binds those assets to the release tag and source revision.  The publisher uses
this module to verify every required producer before it creates a draft GitHub
Release, then verifies the uploaded draft before making it public.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn

SCHEMA_VERSION = 1
METADATA_NAME = "release-metadata.json"
CHECKSUMS_NAME = "SHA256SUMS.txt"
TAG_PATTERN = re.compile(r"^v[0-9]{4}\.(0[1-9]|1[0-2])\.[1-9][0-9]*$")
REVISION_PATTERN = re.compile(r"^[0-9a-f]{40}$")
PRODUCER_PATTERN = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
PORTABLE_FILENAME_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
WINDOWS_RESERVED_STEMS = {
    "aux",
    "clock$",
    "con",
    "nul",
    "prn",
    *(f"com{number}" for number in range(1, 10)),
    *(f"lpt{number}" for number in range(1, 10)),
}


class ArtifactError(RuntimeError):
    """The producer bundle or draft release violated the release contract."""


@dataclass(frozen=True)
class ProducerPolicy:
    platform: str
    architecture: str
    asset_suffix: str
    asset_count: int


# Every policy entry is mandatory. Keep this set identical to the publisher's
# --required-producer arguments; omission, duplication, or an unexpected bundle
# fails before a GitHub Release is created.
PRODUCER_POLICIES = {
    "macos-arm64": ProducerPolicy(
        platform="macos",
        architecture="arm64",
        asset_suffix=".dmg",
        asset_count=1,
    ),
    "linux-x64": ProducerPolicy(
        platform="linux",
        architecture="x86_64",
        asset_suffix=".appimage",
        asset_count=1,
    ),
    "windows-x64": ProducerPolicy(
        platform="windows",
        architecture="x86_64",
        asset_suffix=".exe",
        asset_count=1,
    ),
}


def fail(message: str) -> NoReturn:
    raise ArtifactError(message)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def require_release_identity(release_tag: str, revision: str) -> None:
    if not TAG_PATTERN.fullmatch(release_tag):
        fail(f"invalid release tag: {release_tag!r}")
    if not REVISION_PATTERN.fullmatch(revision):
        fail(f"invalid source revision: {revision!r}")


def require_plain_file(path: Path, label: str) -> None:
    if path.is_symlink() or not path.is_file():
        fail(f"{label} must be a regular non-symlink file: {path}")


def portable_filename_key(filename: str, label: str = "asset filename") -> str:
    """Validate one name on Linux, macOS, and Windows and return its alias key."""

    if (
        not PORTABLE_FILENAME_PATTERN.fullmatch(filename)
        or filename.endswith(".")
        or filename.split(".", 1)[0].casefold() in WINDOWS_RESERVED_STEMS
    ):
        fail(f"unsafe or non-portable {label}: {filename!r}")
    return filename.casefold()


def require_unique_portable_names(names: list[str], label: str) -> None:
    seen: set[str] = set()
    for name in names:
        key = portable_filename_key(name, label)
        if key in seen:
            fail(f"duplicate or case-colliding {label}: {name!r}")
        seen.add(key)


def require_empty_output(output_dir: Path) -> None:
    if output_dir.exists():
        if output_dir.is_symlink() or not output_dir.is_dir():
            fail(f"output path is not a plain directory: {output_dir}")
        if any(output_dir.iterdir()):
            fail(f"output directory must be empty: {output_dir}")
    else:
        output_dir.mkdir(parents=True)


def canonical_json(data: object) -> str:
    return json.dumps(data, indent=2, sort_keys=True) + "\n"


def write_text_lf(path: Path, content: str) -> None:
    with path.open("w", encoding="utf-8", newline="\n") as destination:
        destination.write(content)


def create_bundle(
    *,
    producer: str,
    release_tag: str,
    revision: str,
    assets: list[Path],
    output_dir: Path,
) -> None:
    require_release_identity(release_tag, revision)
    if not PRODUCER_PATTERN.fullmatch(producer):
        fail(f"invalid producer name: {producer!r}")
    policy = PRODUCER_POLICIES.get(producer)
    if policy is None:
        fail(f"producer is not in the release policy: {producer}")
    if len(assets) != policy.asset_count:
        fail(
            f"{producer} must emit exactly {policy.asset_count} asset(s); "
            f"received {len(assets)}"
        )

    reserved_names = {
        portable_filename_key(METADATA_NAME),
        portable_filename_key(CHECKSUMS_NAME),
    }
    asset_names: set[str] = set()
    for asset in assets:
        require_plain_file(asset, "release asset")
        name_key = portable_filename_key(asset.name)
        if asset.suffix.lower() != policy.asset_suffix:
            fail(f"{producer} asset must end in {policy.asset_suffix}: {asset.name}")
        if asset.stat().st_size <= 0:
            fail(f"{producer} asset must not be empty: {asset.name}")
        if name_key in reserved_names or name_key in asset_names:
            fail(f"duplicate or reserved asset name: {asset.name}")
        asset_names.add(name_key)

    require_empty_output(output_dir)
    manifest_assets = []
    checksum_lines = []
    for asset in sorted(assets, key=lambda candidate: candidate.name):
        destination = output_dir / asset.name
        shutil.copyfile(asset, destination)
        digest = sha256(destination)
        size = destination.stat().st_size
        manifest_assets.append(
            {"filename": destination.name, "sha256": digest, "size": size}
        )
        checksum_lines.append(f"{digest}  {destination.name}\n")

    metadata = {
        "architecture": policy.architecture,
        "assets": manifest_assets,
        "platform": policy.platform,
        "producer": producer,
        "release_tag": release_tag,
        "revision": revision,
        "schema_version": SCHEMA_VERSION,
    }
    write_text_lf(output_dir / METADATA_NAME, canonical_json(metadata))
    write_text_lf(output_dir / CHECKSUMS_NAME, "".join(checksum_lines))


def load_json(path: Path, label: str) -> dict:
    require_plain_file(path, label)

    def unique_object(pairs: list[tuple[str, object]]) -> dict:
        value = {}
        for key, item in pairs:
            if key in value:
                fail(f"{label} contains duplicate JSON key: {key!r}")
            value[key] = item
        return value

    try:
        value = json.loads(
            path.read_text(encoding="utf-8"), object_pairs_hook=unique_object
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        fail(f"could not parse {label} {path}: {exc}")
    if not isinstance(value, dict):
        fail(f"{label} must contain a JSON object: {path}")
    return value


def load_checksums(path: Path) -> dict[str, str]:
    require_plain_file(path, "checksum file")
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as exc:
        fail(f"could not read checksum file {path}: {exc}")
    checksums: dict[str, str] = {}
    checksum_keys: set[str] = set()
    for line in lines:
        parts = line.split("  ", 1)
        if len(parts) != 2 or not SHA256_PATTERN.fullmatch(parts[0]):
            fail(f"malformed checksum line in {path}: {line!r}")
        filename = parts[1]
        filename_key = portable_filename_key(filename, "checksum filename")
        if filename_key in checksum_keys:
            fail(f"unsafe or duplicate checksum filename in {path}: {filename!r}")
        checksum_keys.add(filename_key)
        checksums[filename] = parts[0]
    return checksums


def verify_bundle(
    *,
    bundle_dir: Path,
    producer: str,
    release_tag: str,
    revision: str,
) -> list[Path]:
    if bundle_dir.is_symlink() or not bundle_dir.is_dir():
        fail(f"missing producer bundle for {producer}: {bundle_dir}")
    policy = PRODUCER_POLICIES.get(producer)
    if policy is None:
        fail(f"required producer is not in the release policy: {producer}")

    entries = list(bundle_dir.iterdir())
    if any(entry.is_symlink() or not entry.is_file() for entry in entries):
        fail(f"producer bundle contains a symlink or nested path: {bundle_dir}")
    require_unique_portable_names(
        [entry.name for entry in entries], "producer bundle filename"
    )

    metadata = load_json(bundle_dir / METADATA_NAME, "release metadata")
    metadata_fields = {
        "architecture",
        "assets",
        "platform",
        "producer",
        "release_tag",
        "revision",
        "schema_version",
    }
    if set(metadata) != metadata_fields:
        fail(f"{producer} metadata fields do not exactly match schema version 1")
    expected_identity = {
        "architecture": policy.architecture,
        "platform": policy.platform,
        "producer": producer,
        "release_tag": release_tag,
        "revision": revision,
        "schema_version": SCHEMA_VERSION,
    }
    for key, expected in expected_identity.items():
        if metadata.get(key) != expected:
            fail(
                f"{producer} metadata {key!r} is {metadata.get(key)!r}; "
                f"expected {expected!r}"
            )

    manifest_assets = metadata.get("assets")
    if (
        not isinstance(manifest_assets, list)
        or len(manifest_assets) != policy.asset_count
    ):
        fail(f"{producer} metadata has the wrong number of assets")
    checksums = load_checksums(bundle_dir / CHECKSUMS_NAME)
    expected_files = {METADATA_NAME, CHECKSUMS_NAME}
    verified_assets = []
    manifest_names: set[str] = set()
    manifest_keys: set[str] = set()
    for item in manifest_assets:
        if not isinstance(item, dict):
            fail(f"{producer} metadata contains a non-object asset entry")
        if set(item) != {"filename", "sha256", "size"}:
            fail(f"{producer} asset metadata fields do not exactly match the schema")
        filename = item.get("filename")
        digest = item.get("sha256")
        size = item.get("size")
        if not isinstance(filename, str):
            fail(f"{producer} metadata contains an unsafe asset filename: {filename!r}")
        filename_key = portable_filename_key(filename)
        if filename_key in manifest_keys:
            fail(f"{producer} metadata contains duplicate asset {filename}")
        if Path(filename).suffix.lower() != policy.asset_suffix:
            fail(f"{producer} asset has the wrong suffix: {filename}")
        if not isinstance(digest, str) or not SHA256_PATTERN.fullmatch(digest):
            fail(f"{producer} metadata has an invalid SHA-256 for {filename}")
        if not isinstance(size, int) or isinstance(size, bool) or size <= 0:
            fail(f"{producer} metadata has an invalid size for {filename}")

        asset = bundle_dir / filename
        require_plain_file(asset, "release asset")
        if asset.stat().st_size != size:
            fail(f"{producer} asset size does not match metadata: {filename}")
        if sha256(asset) != digest:
            fail(f"{producer} asset checksum does not match metadata: {filename}")
        if checksums.get(filename) != digest:
            fail(
                f"{producer} asset checksum does not match {CHECKSUMS_NAME}: {filename}"
            )
        manifest_names.add(filename)
        manifest_keys.add(filename_key)
        expected_files.add(filename)
        verified_assets.append(asset)

    if set(checksums) != manifest_names:
        fail(f"{producer} checksum file does not exactly match its metadata assets")
    if {entry.name for entry in entries} != expected_files:
        fail(f"{producer} bundle contains missing or unexpected files")
    return verified_assets


def verify_bundles(
    *,
    input_root: Path,
    required_producers: list[str],
    release_tag: str,
    revision: str,
    output_dir: Path,
) -> None:
    require_release_identity(release_tag, revision)
    if not required_producers or len(set(required_producers)) != len(
        required_producers
    ):
        fail("required producers must be a non-empty unique list")
    policy_producers = set(PRODUCER_POLICIES)
    if set(required_producers) != policy_producers:
        fail(
            "required producers do not exactly match the release policy: "
            f"received {sorted(required_producers)!r}, "
            f"expected {sorted(policy_producers)!r}"
        )
    if input_root.is_symlink() or not input_root.is_dir():
        fail(f"release input root is missing or unsafe: {input_root}")

    producer_dirs = list(input_root.iterdir())
    if any(path.is_symlink() or not path.is_dir() for path in producer_dirs):
        fail(f"release input contains a non-directory producer entry: {input_root}")
    if {path.name for path in producer_dirs} != set(required_producers):
        fail("downloaded producer set does not exactly match the required producer set")

    require_empty_output(output_dir)
    release_assets: list[dict] = []
    output_names = {portable_filename_key("release-index.json")}
    for producer in sorted(required_producers):
        bundle = input_root / producer
        verified_assets = verify_bundle(
            bundle_dir=bundle,
            producer=producer,
            release_tag=release_tag,
            revision=revision,
        )
        publish_files = [
            *verified_assets,
            bundle / METADATA_NAME,
            bundle / CHECKSUMS_NAME,
        ]
        for source in publish_files:
            if source.name == METADATA_NAME:
                destination_name = f"{producer}-{METADATA_NAME}"
            elif source.name == CHECKSUMS_NAME:
                destination_name = f"{producer}-{CHECKSUMS_NAME}"
            else:
                destination_name = source.name
            destination_key = portable_filename_key(
                destination_name, "published release filename"
            )
            if destination_key in output_names:
                fail(
                    f"release producers collide on published filename: {destination_name}"
                )
            output_names.add(destination_key)
            destination = output_dir / destination_name
            shutil.copyfile(source, destination)
            release_assets.append(
                {
                    "filename": destination_name,
                    "sha256": sha256(destination),
                    "size": destination.stat().st_size,
                }
            )

    release_index = {
        "assets": sorted(release_assets, key=lambda item: item["filename"]),
        "producers": sorted(required_producers),
        "release_tag": release_tag,
        "revision": revision,
        "schema_version": SCHEMA_VERSION,
    }
    index_path = output_dir / "release-index.json"
    write_text_lf(index_path, canonical_json(release_index))


def require_draft_release_identity(
    *,
    data: dict,
    release_tag: str,
    revision: str,
    expected_release_id: int | None = None,
) -> int:
    """Bind one draft release to its immutable ID, tag, and source revision."""

    require_release_identity(release_tag, revision)
    if data.get("tag_name") != release_tag:
        fail("draft GitHub Release is attached to the wrong tag")
    if data.get("draft") is not True:
        fail("GitHub Release must remain a draft until its assets are verified")
    release_id = data.get("id")
    if (
        not isinstance(release_id, int)
        or isinstance(release_id, bool)
        or release_id <= 0
    ):
        fail("draft GitHub Release has an invalid immutable release ID")
    if expected_release_id is not None and release_id != expected_release_id:
        fail("draft GitHub Release ID does not match the release created by this run")
    if data.get("target_commitish") != revision:
        fail("draft GitHub Release is attached to the wrong source revision")
    source_marker = f"Source revision: {revision}"
    body = data.get("body")
    if not isinstance(body, str) or not (
        body == source_marker or body.startswith(source_marker + "\n")
    ):
        fail("draft GitHub Release is missing its source revision marker")
    return release_id


def verify_github_release(
    *,
    release_json: Path,
    verified_dir: Path,
    release_tag: str,
    revision: str,
    expected_release_id: int,
) -> None:
    data = load_json(release_json, "GitHub release response")
    require_draft_release_identity(
        data=data,
        release_tag=release_tag,
        revision=revision,
        expected_release_id=expected_release_id,
    )

    local_files = {}
    local_keys: set[str] = set()
    if verified_dir.is_symlink() or not verified_dir.is_dir():
        fail(f"verified release directory is missing or unsafe: {verified_dir}")
    for path in verified_dir.iterdir():
        require_plain_file(path, "verified release file")
        filename_key = portable_filename_key(path.name, "verified release filename")
        if filename_key in local_keys:
            fail(f"verified release contains case-colliding filename: {path.name}")
        local_keys.add(filename_key)
        size = path.stat().st_size
        if size <= 0:
            fail(f"verified release file must not be empty: {path.name}")
        local_files[path.name] = {"sha256": sha256(path), "size": size}

    remote_files = {}
    remote_keys: set[str] = set()
    assets = data.get("assets")
    if not isinstance(assets, list):
        fail("GitHub release response has no asset list")
    for asset in assets:
        if not isinstance(asset, dict):
            fail("GitHub release response contains a non-object asset")
        name = asset.get("name")
        size = asset.get("size")
        state = asset.get("state")
        if not isinstance(name, str) or not isinstance(size, int):
            fail("GitHub release response contains invalid asset metadata")
        filename_key = portable_filename_key(name, "GitHub release filename")
        if state != "uploaded":
            fail(f"GitHub release asset did not finish uploading: {name!r}")
        if filename_key in remote_keys:
            fail(f"GitHub release contains a duplicate asset: {name}")
        remote_keys.add(filename_key)
        local = local_files.get(name)
        if local is None or size != local["size"]:
            fail(
                "draft GitHub Release assets do not exactly match the verified local files"
            )
        digest = asset.get("digest")
        if digest != f"sha256:{local['sha256']}":
            fail(f"GitHub release asset digest does not match: {name}")
        remote_files[name] = {"sha256": local["sha256"], "size": size}
    if remote_files != local_files:
        fail(
            "draft GitHub Release assets do not exactly match the verified local files"
        )


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    create = commands.add_parser("create", help="create one producer bundle")
    create.add_argument("--producer", required=True)
    create.add_argument("--release-tag", required=True)
    create.add_argument("--revision", required=True)
    create.add_argument("--asset", action="append", required=True, type=Path)
    create.add_argument("--output-dir", required=True, type=Path)

    verify = commands.add_parser("verify", help="verify all required producer bundles")
    verify.add_argument("--input-root", required=True, type=Path)
    verify.add_argument("--required-producer", action="append", required=True)
    verify.add_argument("--release-tag", required=True)
    verify.add_argument("--revision", required=True)
    verify.add_argument("--output-dir", required=True, type=Path)

    verify_release = commands.add_parser(
        "verify-github-release", help="verify an uploaded draft before publication"
    )
    verify_release.add_argument("--release-json", required=True, type=Path)
    verify_release.add_argument("--verified-dir", required=True, type=Path)
    verify_release.add_argument("--release-tag", required=True)
    verify_release.add_argument("--revision", required=True)
    verify_release.add_argument("--expected-release-id", required=True, type=int)

    verify_identity = commands.add_parser(
        "verify-draft-identity",
        help="verify a draft release identity and print its immutable ID",
    )
    verify_identity.add_argument("--release-json", required=True, type=Path)
    verify_identity.add_argument("--release-tag", required=True)
    verify_identity.add_argument("--revision", required=True)
    verify_identity.add_argument("--expected-release-id", type=int)
    return root


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "create":
            create_bundle(
                producer=args.producer,
                release_tag=args.release_tag,
                revision=args.revision,
                assets=args.asset,
                output_dir=args.output_dir,
            )
        elif args.command == "verify":
            verify_bundles(
                input_root=args.input_root,
                required_producers=args.required_producer,
                release_tag=args.release_tag,
                revision=args.revision,
                output_dir=args.output_dir,
            )
        elif args.command == "verify-github-release":
            verify_github_release(
                release_json=args.release_json,
                verified_dir=args.verified_dir,
                release_tag=args.release_tag,
                revision=args.revision,
                expected_release_id=args.expected_release_id,
            )
        else:
            data = load_json(args.release_json, "GitHub release response")
            release_id = require_draft_release_identity(
                data=data,
                release_tag=args.release_tag,
                revision=args.revision,
                expected_release_id=args.expected_release_id,
            )
            print(release_id)
    except ArtifactError as exc:
        print(f"release artifact: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
