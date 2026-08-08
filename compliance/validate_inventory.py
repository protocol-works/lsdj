#!/usr/bin/env python3
"""Validate the revision-specific model/runtime compliance inventory.

This is deliberately standard-library-only so release jobs can run it before
installing any application dependencies.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any
from urllib.parse import urlparse


ROOT_REQUIRED = {
    "schema_version",
    "inventory_revision",
    "audited_at",
    "audit_base_revision",
    "purpose",
    "project_use",
    "catalogs",
    "assets",
}
ASSET_REQUIRED = {
    "id",
    "name",
    "family",
    "asset_type",
    "support_status",
    "upstream",
    "revision",
    "licenses",
    "notices",
    "access",
    "distribution",
    "dependencies",
    "owner_review",
    "evidence",
}
LICENSE_REQUIRED = {
    "status",
    "identifier",
    "name",
    "scope",
    "terms_url",
    "notice_url",
}
ACCESS_REQUIRED = {
    "gated",
    "account_required",
    "credential_required",
    "terms_acceptance_required",
    "privacy_url",
    "acceptable_use_url",
}
DISTRIBUTION_REQUIRED = {
    "mode",
    "source_url",
    "installer_contains_asset",
    "installer_contains_weights",
    "redistribution_confirmed",
    "immutable_reference_enforced",
    "release_gate",
    "notes",
}
NOTICE_REQUIRED = {"required_text", "attribution", "sources"}
OWNER_REVIEW_REQUIRED = {"required", "status", "question", "issue"}
REVISION_KINDS = {"git_commit", "model_snapshot", "unresolved_upstream"}
LICENSE_STATUSES = {"declared", "underlying", "unresolved", "not_applicable"}
OWNER_STATUSES = {"pending", "confirmed", "not_required"}
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
INVENTORY_REV_RE = re.compile(r"^\d{4}-\d{2}-\d{2}\.\d+$")
MUTABLE_URL_RE = re.compile(r"/(?:blob|tree|resolve)/(?:main|master|HEAD)(?:/|$)", re.I)


def _missing(value: dict[str, Any], required: set[str], path: str) -> list[str]:
    return [
        f"{path}: missing field {field!r}" for field in sorted(required - value.keys())
    ]


def _is_https(value: Any) -> bool:
    return (
        isinstance(value, str)
        and urlparse(value).scheme == "https"
        and bool(urlparse(value).netloc)
    )


def _url_error(value: Any, path: str, *, nullable: bool = False) -> list[str]:
    if nullable and value is None:
        return []
    return [] if _is_https(value) else [f"{path}: must be an https URL"]


def validate(data: Any) -> list[str]:
    """Return all validation errors without stopping at the first one."""
    if not isinstance(data, dict):
        return ["root: expected an object"]
    errors = _missing(data, ROOT_REQUIRED, "root")
    if errors:
        return errors

    if data["schema_version"] != 1:
        errors.append("root.schema_version: only version 1 is supported")
    if not INVENTORY_REV_RE.fullmatch(str(data["inventory_revision"])):
        errors.append("root.inventory_revision: expected YYYY-MM-DD.N")
    if not DATE_RE.fullmatch(str(data["audited_at"])):
        errors.append("root.audited_at: expected YYYY-MM-DD")
    if not COMMIT_RE.fullmatch(str(data["audit_base_revision"])):
        errors.append("root.audit_base_revision: expected a full 40-character commit")

    project_use = data["project_use"]
    if not isinstance(project_use, dict):
        errors.append("root.project_use: expected an object")
    else:
        required = {
            "reported_context",
            "owner_confirmation_status",
            "future_distribution_or_commercial_change_is_release_gate",
            "public_record_must_exclude",
        }
        errors.extend(_missing(project_use, required, "root.project_use"))
        owner_confirmation_status = project_use.get("owner_confirmation_status")
        if not isinstance(
            owner_confirmation_status, str
        ) or owner_confirmation_status not in {"pending", "confirmed"}:
            errors.append("root.project_use.owner_confirmation_status: invalid status")
        if (
            project_use.get("future_distribution_or_commercial_change_is_release_gate")
            is not True
        ):
            errors.append(
                "root.project_use: future use/distribution changes must be a release gate"
            )

    assets = data["assets"]
    if not isinstance(assets, list) or not assets:
        errors.append("root.assets: expected a non-empty list")
        return errors

    ids: set[str] = set()
    for index, asset in enumerate(assets):
        path = f"root.assets[{index}]"
        if not isinstance(asset, dict):
            errors.append(f"{path}: expected an object")
            continue
        missing = _missing(asset, ASSET_REQUIRED, path)
        errors.extend(missing)
        if missing:
            continue

        asset_id = asset["id"]
        if not isinstance(asset_id, str) or not re.fullmatch(
            r"[a-z0-9][a-z0-9-]*", asset_id
        ):
            errors.append(f"{path}.id: expected a lowercase kebab-case identifier")
        elif asset_id in ids:
            errors.append(f"{path}.id: duplicate identifier {asset_id!r}")
        else:
            ids.add(asset_id)

        upstream = asset["upstream"]
        if not isinstance(upstream, dict):
            errors.append(f"{path}.upstream: expected an object")
        else:
            errors.extend(
                _missing(upstream, {"project", "canonical_url"}, f"{path}.upstream")
            )
            errors.extend(
                _url_error(
                    upstream.get("canonical_url"), f"{path}.upstream.canonical_url"
                )
            )

        revision = asset["revision"]
        if not isinstance(revision, dict):
            errors.append(f"{path}.revision: expected an object")
        else:
            errors.extend(
                _missing(revision, {"kind", "value", "url"}, f"{path}.revision")
            )
            kind = revision.get("kind")
            value = revision.get("value")
            url = revision.get("url")
            distribution_for_revision = asset.get("distribution")
            distribution_for_revision = (
                distribution_for_revision
                if isinstance(distribution_for_revision, dict)
                else {}
            )
            if not isinstance(kind, str) or kind not in REVISION_KINDS:
                errors.append(f"{path}.revision.kind: invalid revision kind")
            if kind == "unresolved_upstream":
                if value is not None:
                    errors.append(
                        f"{path}.revision.value: unresolved upstream revisions use null"
                    )
                if distribution_for_revision.get("immutable_reference_enforced"):
                    errors.append(
                        f"{path}.revision: unresolved upstream revision cannot be marked immutable"
                    )
                if not distribution_for_revision.get("release_gate"):
                    errors.append(
                        f"{path}.revision: unresolved upstream revision must remain a release gate"
                    )
            elif not isinstance(value, str) or not COMMIT_RE.fullmatch(value):
                errors.append(
                    f"{path}.revision.value: expected a full immutable 40-character hash"
                )
            errors.extend(_url_error(url, f"{path}.revision.url"))
            if isinstance(url, str) and MUTABLE_URL_RE.search(url):
                errors.append(
                    f"{path}.revision.url: mutable branch references are forbidden"
                )
            if (
                kind != "unresolved_upstream"
                and isinstance(url, str)
                and isinstance(value, str)
                and value not in url
            ):
                errors.append(
                    f"{path}.revision.url: must contain the exact revision value"
                )

        licenses = asset["licenses"]
        if not isinstance(licenses, dict):
            errors.append(f"{path}.licenses: expected an object")
        else:
            errors.extend(_missing(licenses, {"code", "weights"}, f"{path}.licenses"))
            for license_kind in ("code", "weights"):
                records = licenses.get(license_kind)
                license_path = f"{path}.licenses.{license_kind}"
                if not isinstance(records, list) or not records:
                    errors.append(f"{license_path}: expected a non-empty list")
                    continue
                for license_index, record in enumerate(records):
                    record_path = f"{license_path}[{license_index}]"
                    if not isinstance(record, dict):
                        errors.append(f"{record_path}: expected an object")
                        continue
                    record_missing = _missing(record, LICENSE_REQUIRED, record_path)
                    errors.extend(record_missing)
                    if record_missing:
                        continue
                    status = record["status"]
                    identifier = record["identifier"]
                    if not isinstance(status, str) or status not in LICENSE_STATUSES:
                        errors.append(f"{record_path}.status: invalid status")
                    if not isinstance(identifier, str):
                        errors.append(f"{record_path}.identifier: expected a string")
                    if isinstance(status, str) and status in {"declared", "underlying"}:
                        if not isinstance(identifier, str) or identifier in {
                            "NONE",
                            "NOASSERTION",
                        }:
                            errors.append(
                                f"{record_path}.identifier: applicable license needs an identifier"
                            )
                        errors.extend(
                            _url_error(record["terms_url"], f"{record_path}.terms_url")
                        )
                    elif status == "unresolved" and identifier != "NOASSERTION":
                        errors.append(
                            f"{record_path}.identifier: unresolved licenses use NOASSERTION"
                        )
                    elif status == "not_applicable" and identifier != "NONE":
                        errors.append(
                            f"{record_path}.identifier: non-applicable licenses use NONE"
                        )
                    errors.extend(
                        _url_error(
                            record["notice_url"],
                            f"{record_path}.notice_url",
                            nullable=True,
                        )
                    )

        notices = asset["notices"]
        if not isinstance(notices, dict):
            errors.append(f"{path}.notices: expected an object")
        else:
            errors.extend(_missing(notices, NOTICE_REQUIRED, f"{path}.notices"))
            for field in ("required_text", "attribution", "sources"):
                if field in notices and not isinstance(notices[field], list):
                    errors.append(f"{path}.notices.{field}: expected a list")
            for source_index, source in enumerate(notices.get("sources", [])):
                errors.extend(
                    _url_error(source, f"{path}.notices.sources[{source_index}]")
                )

        access = asset["access"]
        if not isinstance(access, dict):
            errors.append(f"{path}.access: expected an object")
        else:
            errors.extend(_missing(access, ACCESS_REQUIRED, f"{path}.access"))
            for field in (
                "gated",
                "account_required",
                "credential_required",
                "terms_acceptance_required",
            ):
                if field in access and not isinstance(access[field], bool):
                    errors.append(f"{path}.access.{field}: expected a boolean")
            for field in ("privacy_url", "acceptable_use_url"):
                errors.extend(
                    _url_error(
                        access.get(field), f"{path}.access.{field}", nullable=True
                    )
                )
            if access.get("gated") and not access.get("terms_acceptance_required"):
                errors.append(
                    f"{path}.access: gated assets must record terms acceptance"
                )

        distribution = asset["distribution"]
        if not isinstance(distribution, dict):
            errors.append(f"{path}.distribution: expected an object")
        else:
            errors.extend(
                _missing(distribution, DISTRIBUTION_REQUIRED, f"{path}.distribution")
            )
            source_url = distribution.get("source_url")
            errors.extend(_url_error(source_url, f"{path}.distribution.source_url"))
            if isinstance(source_url, str) and MUTABLE_URL_RE.search(source_url):
                errors.append(
                    f"{path}.distribution.source_url: mutable branch references are forbidden"
                )
            revision_for_distribution = asset.get("revision")
            revision_for_distribution = (
                revision_for_distribution
                if isinstance(revision_for_distribution, dict)
                else {}
            )
            revision_value = revision_for_distribution.get("value")
            if (
                revision_for_distribution.get("kind") != "unresolved_upstream"
                and isinstance(source_url, str)
                and isinstance(revision_value, str)
                and revision_value not in source_url
            ):
                errors.append(
                    f"{path}.distribution.source_url: must contain the exact revision value"
                )
            for field in (
                "installer_contains_asset",
                "installer_contains_weights",
                "redistribution_confirmed",
                "immutable_reference_enforced",
                "release_gate",
            ):
                if field in distribution and not isinstance(distribution[field], bool):
                    errors.append(f"{path}.distribution.{field}: expected a boolean")
            if distribution.get("installer_contains_weights") and not distribution.get(
                "redistribution_confirmed"
            ):
                errors.append(
                    f"{path}.distribution: unconfirmed weights must not be placed in installers"
                )
            if not distribution.get(
                "immutable_reference_enforced"
            ) and not distribution.get("release_gate"):
                errors.append(
                    f"{path}.distribution: a mutable runtime path must remain a release gate"
                )

        owner_review = asset["owner_review"]
        if not isinstance(owner_review, dict):
            errors.append(f"{path}.owner_review: expected an object")
        else:
            errors.extend(
                _missing(owner_review, OWNER_REVIEW_REQUIRED, f"{path}.owner_review")
            )
            owner_status = owner_review.get("status")
            if not isinstance(owner_status, str) or owner_status not in OWNER_STATUSES:
                errors.append(f"{path}.owner_review.status: invalid status")
            errors.extend(
                _url_error(owner_review.get("issue"), f"{path}.owner_review.issue")
            )
            if (
                owner_review.get("required") is True
                and owner_review.get("status") == "not_required"
            ):
                errors.append(
                    f"{path}.owner_review: required review cannot be marked not_required"
                )

        if not isinstance(asset["dependencies"], list):
            errors.append(f"{path}.dependencies: expected a list")
        if not isinstance(asset["evidence"], list) or not asset["evidence"]:
            errors.append(f"{path}.evidence: expected a non-empty list")
        else:
            for evidence_index, evidence in enumerate(asset["evidence"]):
                errors.extend(
                    _url_error(evidence, f"{path}.evidence[{evidence_index}]")
                )

    for index, asset in enumerate(assets):
        if not isinstance(asset, dict) or "dependencies" not in asset:
            continue
        for dependency in (
            asset["dependencies"] if isinstance(asset["dependencies"], list) else []
        ):
            if not isinstance(dependency, str):
                errors.append(
                    f"root.assets[{index}].dependencies: expected string asset ids"
                )
                continue
            if dependency not in ids:
                errors.append(
                    f"root.assets[{index}].dependencies: unknown asset id {dependency!r}"
                )
            if dependency == asset.get("id"):
                errors.append(
                    f"root.assets[{index}].dependencies: self-dependency is forbidden"
                )

    assets_by_id = {
        item["id"]: item
        for item in assets
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    }
    catalogs = data["catalogs"]
    if not isinstance(catalogs, dict):
        errors.append("root.catalogs: expected an object")
    else:
        required = {
            "bundled_lora_ids",
            "official_lora_ids",
            "documented_reference_lora_ids",
            "note",
        }
        errors.extend(_missing(catalogs, required, "root.catalogs"))
        for field in (
            "bundled_lora_ids",
            "official_lora_ids",
            "documented_reference_lora_ids",
        ):
            values = catalogs.get(field)
            if not isinstance(values, list):
                errors.append(f"root.catalogs.{field}: expected a list")
                continue
            for asset_id in values:
                if not isinstance(asset_id, str):
                    errors.append(f"root.catalogs.{field}: expected string asset ids")
                    continue
                if asset_id not in ids:
                    errors.append(
                        f"root.catalogs.{field}: unknown asset id {asset_id!r}"
                    )
                else:
                    asset = assets_by_id[asset_id]
                    if asset.get("asset_type") != "lora":
                        errors.append(
                            f"root.catalogs.{field}: {asset_id!r} is not a LoRA"
                        )

    return errors


def load(path: Path) -> Any:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "path",
        nargs="?",
        type=Path,
        default=Path(__file__).with_name("model-assets.json"),
    )
    args = parser.parse_args(argv)
    try:
        data = load(args.path)
    except (OSError, json.JSONDecodeError) as exc:
        print(f"{args.path}: {exc}", file=sys.stderr)
        return 2
    errors = validate(data)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print(f"validated {len(data['assets'])} assets in {args.path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
