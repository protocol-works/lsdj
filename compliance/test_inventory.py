from __future__ import annotations

import copy
import json
import unittest
from pathlib import Path

from compliance.validate_inventory import load, validate


MANIFEST = Path(__file__).with_name("model-assets.json")


class InventoryValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.data = load(MANIFEST)

    def test_repository_inventory_is_valid(self) -> None:
        self.assertEqual(validate(self.data), [])

    def test_runtime_acquisition_pins_match_compliance_revisions(self) -> None:
        root = MANIFEST.parent.parent
        mrt2_pin = json.loads((root / "mrt2-pytorch-pin.json").read_text())
        sa3_pin = json.loads((root / "sa3-pin.json").read_text())
        sa3_cuda_pin = json.loads(
            (root / "sa3-pytorch-cuda-pin.json").read_text()
        )
        assets = {asset["id"]: asset for asset in self.data["assets"]}

        self.assertEqual(
            assets["pytorch-mrt2-base-weights"]["revision"]["value"],
            mrt2_pin["models"]["mrt2_base"]["revision"],
        )
        self.assertEqual(
            assets["pytorch-mrt2-small-weights"]["revision"]["value"],
            mrt2_pin["models"]["mrt2_small"]["revision"],
        )
        self.assertEqual(
            assets["pytorch-musiccoca-processor"]["revision"]["value"],
            mrt2_pin["processor"]["revision"],
        )
        self.assertEqual(
            assets["stable-audio-3-code"]["revision"]["value"],
            sa3_pin["commit"],
        )
        self.assertEqual(
            assets["stable-audio-3-small-music-cuda-weights"]["revision"]["value"],
            sa3_cuda_pin["models"]["small-music"]["revision"],
        )
        self.assertEqual(
            assets["stable-audio-3-small-sfx-cuda-weights"]["revision"]["value"],
            sa3_cuda_pin["models"]["small-sfx"]["revision"],
        )
        self.assertEqual(
            assets["stable-audio-3-small-music-cuda-weights"]["artifact_integrity"][
                "weight_sha256"
            ],
            sa3_cuda_pin["models"]["small-music"]["weight"]["sha256"],
        )
        self.assertEqual(
            assets["stable-audio-3-small-sfx-cuda-weights"]["artifact_integrity"][
                "weight_sha256"
            ],
            sa3_cuda_pin["models"]["small-sfx"]["weight"]["sha256"],
        )
        self.assertIsNone(
            assets["stable-audio-3-small-music-cuda-weights"]["artifact_integrity"][
                "config_sha256"
            ]
        )
        self.assertIsNone(
            assets["stable-audio-3-small-sfx-cuda-weights"]["artifact_integrity"][
                "config_sha256"
            ]
        )
        self.assertEqual(
            assets["pytorch-mrt2-port-code"]["distribution"]["mode"],
            "reference_only_not_acquired",
        )

    def test_missing_required_asset_field_is_rejected(self) -> None:
        broken = copy.deepcopy(self.data)
        del broken["assets"][0]["notices"]
        self.assertTrue(
            any("missing field 'notices'" in error for error in validate(broken))
        )

    def test_mutable_revision_is_rejected(self) -> None:
        broken = copy.deepcopy(self.data)
        broken["assets"][0]["revision"] = {
            "kind": "git_commit",
            "value": "main",
            "url": "https://github.com/protocol-works/lsdj/tree/main",
        }
        errors = validate(broken)
        self.assertTrue(any("40-character hash" in error for error in errors))
        self.assertTrue(any("mutable branch" in error for error in errors))

    def test_revision_url_must_name_exact_revision(self) -> None:
        broken = copy.deepcopy(self.data)
        broken["assets"][0]["revision"]["url"] = (
            "https://github.com/protocol-works/lsdj"
        )
        self.assertTrue(
            any(
                "must contain the exact revision" in error for error in validate(broken)
            )
        )

    def test_unresolved_upstream_revision_is_explicit_and_gated(self) -> None:
        broken = copy.deepcopy(self.data)
        asset = next(
            item for item in broken["assets"] if item["id"] == "t5gemma-b-b-ul2"
        )
        asset["distribution"]["release_gate"] = False
        errors = validate(broken)
        self.assertTrue(any("must remain a release gate" in error for error in errors))

        broken = copy.deepcopy(self.data)
        asset = next(
            item for item in broken["assets"] if item["id"] == "t5gemma-b-b-ul2"
        )
        asset["revision"]["value"] = "97ea9b7e92738bb57437867277ae38e65345b8d7"
        errors = validate(broken)
        self.assertTrue(any("use null" in error for error in errors))

    def test_non_object_asset_returns_errors_without_crashing(self) -> None:
        broken = copy.deepcopy(self.data)
        broken["assets"].append("not-an-object")
        errors = validate(broken)
        self.assertTrue(any("expected an object" in error for error in errors))

    def test_non_object_nested_fields_return_errors_without_crashing(self) -> None:
        broken = copy.deepcopy(self.data)
        asset = next(
            item for item in broken["assets"] if item["id"] == "t5gemma-b-b-ul2"
        )
        asset["distribution"] = "not-an-object"
        errors = validate(broken)
        self.assertTrue(
            any("distribution: expected an object" in error for error in errors)
        )

        broken = copy.deepcopy(self.data)
        broken["assets"][0]["revision"] = "not-an-object"
        errors = validate(broken)
        self.assertTrue(
            any("revision: expected an object" in error for error in errors)
        )

    def test_unhashable_nested_values_return_errors_without_crashing(self) -> None:
        mutations = [
            (
                "project status",
                lambda data: data["project_use"].__setitem__(
                    "owner_confirmation_status", {}
                ),
                "owner_confirmation_status: invalid status",
            ),
            (
                "revision kind",
                lambda data: data["assets"][0]["revision"].__setitem__("kind", {}),
                "revision.kind: invalid revision kind",
            ),
            (
                "license status",
                lambda data: data["assets"][0]["licenses"]["code"][0].__setitem__(
                    "status", {}
                ),
                "status: invalid status",
            ),
            (
                "license identifier",
                lambda data: data["assets"][1]["licenses"]["code"][0].__setitem__(
                    "identifier", {}
                ),
                "identifier: expected a string",
            ),
            (
                "owner status",
                lambda data: data["assets"][0]["owner_review"].__setitem__(
                    "status", {}
                ),
                "owner_review.status: invalid status",
            ),
            (
                "dependency id",
                lambda data: data["assets"][1]["dependencies"].append({}),
                "dependencies: expected string asset ids",
            ),
            (
                "catalog id",
                lambda data: data["catalogs"]["official_lora_ids"].append({}),
                "official_lora_ids: expected string asset ids",
            ),
        ]
        for label, mutate, expected in mutations:
            with self.subTest(label=label):
                broken = copy.deepcopy(self.data)
                mutate(broken)
                errors = validate(broken)
                self.assertTrue(any(expected in error for error in errors), errors)

    def test_unconfirmed_weights_cannot_be_in_installer(self) -> None:
        broken = copy.deepcopy(self.data)
        asset = next(
            item for item in broken["assets"] if item["id"] == "google-mrt2-weights"
        )
        asset["distribution"]["installer_contains_weights"] = True
        self.assertTrue(
            any("unconfirmed weights" in error for error in validate(broken))
        )

    def test_mutable_runtime_path_must_remain_release_gate(self) -> None:
        broken = copy.deepcopy(self.data)
        asset = next(
            item for item in broken["assets"] if item["id"] == "google-mrt2-weights"
        )
        asset["distribution"]["release_gate"] = False
        self.assertTrue(
            any("mutable runtime path" in error for error in validate(broken))
        )

    def test_catalog_ids_must_resolve_to_loras(self) -> None:
        broken = copy.deepcopy(self.data)
        broken["catalogs"]["official_lora_ids"] = ["google-mrt2-weights"]
        self.assertTrue(any("is not a LoRA" in error for error in validate(broken)))


if __name__ == "__main__":
    unittest.main()
