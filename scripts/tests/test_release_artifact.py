import importlib.util
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).parents[1] / "release_artifact.py"
REPO_ROOT = Path(__file__).parents[2]
SPEC = importlib.util.spec_from_file_location("release_artifact", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
release_artifact = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = release_artifact
SPEC.loader.exec_module(release_artifact)

REVISION = "a" * 40
TAG = "v2026.08.7"
REQUIRED_PRODUCERS = ["macos-arm64", "linux-x64", "windows-x64"]


class ReleaseArtifactTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.macos_asset = self.root / "LSDJ_2026.08.7_aarch64.dmg"
        self.macos_asset.write_bytes(b"verified dmg bytes")
        self.linux_asset = self.root / "LSDJ_2026.08.7_amd64.AppImage"
        self.linux_asset.write_bytes(b"verified appimage bytes")
        self.windows_asset = self.root / "LSDJ_2026.08.7_x64-setup.exe"
        self.windows_asset.write_bytes(b"verified signed nsis bytes")

    def tearDown(self):
        self.temporary.cleanup()

    def create_bundle(self, producer="macos-arm64"):
        asset = {
            "macos-arm64": self.macos_asset,
            "linux-x64": self.linux_asset,
            "windows-x64": self.windows_asset,
        }[producer]
        bundle = self.root / "incoming" / producer
        release_artifact.create_bundle(
            producer=producer,
            release_tag=TAG,
            revision=REVISION,
            assets=[asset],
            output_dir=bundle,
        )
        return bundle

    def create_all_bundles(self):
        self.create_bundle("macos-arm64")
        self.create_bundle("linux-x64")
        self.create_bundle("windows-x64")
        return self.root / "incoming"

    def draft_release(self, assets, **updates):
        data = {
            "id": 12345,
            "tag_name": TAG,
            "target_commitish": REVISION,
            "draft": True,
            "body": f"Source revision: {REVISION}\n\nGenerated notes",
            "assets": assets,
        }
        data.update(updates)
        return data

    def test_create_and_verify_bundle(self):
        incoming = self.create_all_bundles()
        output = self.root / "verified"

        release_artifact.verify_bundles(
            input_root=incoming,
            required_producers=REQUIRED_PRODUCERS,
            release_tag=TAG,
            revision=REVISION,
            output_dir=output,
        )

        self.assertEqual(
            {path.name for path in output.iterdir()},
            {
                self.macos_asset.name,
                self.linux_asset.name,
                self.windows_asset.name,
                "macos-arm64-release-metadata.json",
                "macos-arm64-SHA256SUMS.txt",
                "linux-x64-release-metadata.json",
                "linux-x64-SHA256SUMS.txt",
                "windows-x64-release-metadata.json",
                "windows-x64-SHA256SUMS.txt",
                "release-index.json",
            },
        )
        index = json.loads((output / "release-index.json").read_text())
        self.assertEqual(index["release_tag"], TAG)
        self.assertEqual(index["revision"], REVISION)
        self.assertEqual(
            index["producers"], ["linux-x64", "macos-arm64", "windows-x64"]
        )

    def test_tampered_asset_fails_closed(self):
        incoming = self.create_all_bundles()
        (incoming / "macos-arm64" / self.macos_asset.name).write_bytes(b"tampered")

        with self.assertRaisesRegex(release_artifact.ArtifactError, "size|checksum"):
            release_artifact.verify_bundles(
                input_root=incoming,
                required_producers=REQUIRED_PRODUCERS,
                release_tag=TAG,
                revision=REVISION,
                output_dir=self.root / "verified",
            )

    def test_empty_installer_fails_closed(self):
        self.macos_asset.write_bytes(b"")

        with self.assertRaisesRegex(
            release_artifact.ArtifactError, "must not be empty"
        ):
            self.create_bundle()

    def test_host_specific_or_ambiguous_asset_names_fail_closed(self):
        for filename in ("LSDJ\\setup.dmg", "LSDJ\nsetup.dmg"):
            with self.subTest(filename=filename):
                with self.assertRaisesRegex(
                    release_artifact.ArtifactError, "non-portable"
                ):
                    release_artifact.portable_filename_key(filename)

    def test_missing_required_producer_fails_closed(self):
        incoming = self.root / "incoming"
        incoming.mkdir()

        with self.assertRaisesRegex(release_artifact.ArtifactError, "producer set"):
            release_artifact.verify_bundles(
                input_root=incoming,
                required_producers=REQUIRED_PRODUCERS,
                release_tag=TAG,
                revision=REVISION,
                output_dir=self.root / "verified",
            )

    def test_unexpected_bundle_file_fails_closed(self):
        incoming = self.create_all_bundles()
        (incoming / "macos-arm64" / "surprise.txt").write_text("not declared")

        with self.assertRaisesRegex(release_artifact.ArtifactError, "unexpected"):
            release_artifact.verify_bundles(
                input_root=incoming,
                required_producers=REQUIRED_PRODUCERS,
                release_tag=TAG,
                revision=REVISION,
                output_dir=self.root / "verified",
            )

    def test_wrong_release_identity_fails_closed(self):
        incoming = self.create_all_bundles()

        with self.assertRaisesRegex(release_artifact.ArtifactError, "release_tag"):
            release_artifact.verify_bundles(
                input_root=incoming,
                required_producers=REQUIRED_PRODUCERS,
                release_tag="v2026.08.8",
                revision=REVISION,
                output_dir=self.root / "verified",
            )

    def test_required_producer_arguments_must_exactly_match_policy(self):
        incoming = self.create_all_bundles()
        with self.assertRaisesRegex(release_artifact.ArtifactError, "release policy"):
            release_artifact.verify_bundles(
                input_root=incoming,
                required_producers=["macos-arm64"],
                release_tag=TAG,
                revision=REVISION,
                output_dir=self.root / "verified",
            )

    def test_policy_requires_exact_three_platform_producer_set(self):
        self.assertEqual(set(release_artifact.PRODUCER_POLICIES), set(REQUIRED_PRODUCERS))

    def test_draft_release_assets_must_match_exactly(self):
        verified = self.root / "verified"
        verified.mkdir()
        (verified / "asset.dmg").write_bytes(b"one")
        response = self.root / "release.json"
        response.write_text(
            json.dumps(
                self.draft_release(
                    [
                        {
                            "name": "asset.dmg",
                            "size": 3,
                            "state": "uploaded",
                            "digest": "sha256:"
                            + release_artifact.sha256(verified / "asset.dmg"),
                        }
                    ]
                )
            )
        )

        release_artifact.verify_github_release(
            release_json=response,
            verified_dir=verified,
            release_tag=TAG,
            revision=REVISION,
            expected_release_id=12345,
        )

        data = json.loads(response.read_text())
        data["assets"][0]["size"] = 4
        response.write_text(json.dumps(data))
        with self.assertRaisesRegex(release_artifact.ArtifactError, "exactly match"):
            release_artifact.verify_github_release(
                release_json=response,
                verified_dir=verified,
                release_tag=TAG,
                revision=REVISION,
                expected_release_id=12345,
            )

    def test_github_digest_is_required(self):
        verified = self.root / "verified"
        verified.mkdir()
        asset = verified / "asset.dmg"
        asset.write_bytes(b"one")
        response = self.root / "release.json"

        for digest_entry in ({}, {"digest": None}):
            with self.subTest(digest_entry=digest_entry):
                response.write_text(
                    json.dumps(
                        self.draft_release(
                            [
                                {
                                    "name": asset.name,
                                    "size": asset.stat().st_size,
                                    "state": "uploaded",
                                    **digest_entry,
                                }
                            ]
                        )
                    )
                )
                with self.assertRaisesRegex(release_artifact.ArtifactError, "digest"):
                    release_artifact.verify_github_release(
                        release_json=response,
                        verified_dir=verified,
                        release_tag=TAG,
                        revision=REVISION,
                        expected_release_id=12345,
                    )

    def test_case_colliding_release_names_fail_closed(self):
        with self.assertRaisesRegex(release_artifact.ArtifactError, "case-colliding"):
            release_artifact.require_unique_portable_names(
                ["LSDJ.dmg", "lsdj.DMG"], "release filename"
            )

    def test_github_digest_is_verified_when_present(self):
        verified = self.root / "verified"
        verified.mkdir()
        asset = verified / "asset.dmg"
        asset.write_bytes(b"one")
        response = self.root / "release.json"
        response.write_text(
            json.dumps(
                self.draft_release(
                    [
                        {
                            "name": asset.name,
                            "size": asset.stat().st_size,
                            "state": "uploaded",
                            "digest": "sha256:" + "0" * 64,
                        }
                    ]
                )
            )
        )

        with self.assertRaisesRegex(release_artifact.ArtifactError, "digest"):
            release_artifact.verify_github_release(
                release_json=response,
                verified_dir=verified,
                release_tag=TAG,
                revision=REVISION,
                expected_release_id=12345,
            )

    def test_draft_identity_cannot_be_redirected_to_a_replacement(self):
        replacement = self.draft_release([], id=67890)

        with self.assertRaisesRegex(release_artifact.ArtifactError, "Release ID"):
            release_artifact.require_draft_release_identity(
                data=replacement,
                release_tag=TAG,
                revision=REVISION,
                expected_release_id=12345,
            )

    def test_draft_identity_requires_the_exact_source_revision(self):
        for updates in (
            {"target_commitish": "b" * 40},
            {"body": "Source revision: " + "b" * 40},
        ):
            with self.subTest(updates=updates):
                with self.assertRaisesRegex(
                    release_artifact.ArtifactError, "source revision"
                ):
                    release_artifact.require_draft_release_identity(
                        data=self.draft_release([], **updates),
                        release_tag=TAG,
                        revision=REVISION,
                        expected_release_id=12345,
                    )

    def test_public_release_is_never_accepted_for_pre_publish_verification(self):
        verified = self.root / "verified"
        verified.mkdir()
        response = self.root / "release.json"
        response.write_text(json.dumps({"tag_name": TAG, "draft": False, "assets": []}))

        with self.assertRaisesRegex(release_artifact.ArtifactError, "remain a draft"):
            release_artifact.verify_github_release(
                release_json=response,
                verified_dir=verified,
                release_tag=TAG,
                revision=REVISION,
                expected_release_id=12345,
            )


class WorkflowContractTest(unittest.TestCase):
    def test_release_workflow_keeps_one_least_privilege_publisher(self):
        workflow = (REPO_ROOT / ".github/workflows/macos-release.yml").read_text()

        self.assertEqual(workflow.count("contents: write"), 1)
        self.assertEqual(len(re.findall(r"^  publish:$", workflow, re.MULTILINE)), 1)
        self.assertIn("needs.produce_macos.result == 'success'", workflow)
        self.assertIn("needs.produce_linux.result == 'success'", workflow)
        self.assertIn("needs.produce_windows.result == 'success'", workflow)
        self.assertIn("--required-producer macos-arm64", workflow)
        self.assertIn("--required-producer linux-x64", workflow)
        self.assertIn("--required-producer windows-x64", workflow)
        self.assertIn("runs-on: ubuntu-22.04", workflow)
        self.assertEqual(workflow.count("environment:\n      name: windows-release"), 1)
        self.assertRegex(workflow, r"(?m)^on:\n  push:\n    tags:$")
        self.assertNotIn("pull_request:", workflow)
        self.assertNotIn("workflow_dispatch:", workflow)

    def test_exact_three_producers_feed_the_publisher(self):
        workflow = (REPO_ROOT / ".github/workflows/macos-release.yml").read_text()
        expected_jobs = {"produce_macos", "produce_linux", "produce_windows"}
        expected_artifacts = {
            "release-macos-arm64",
            "release-linux-x64",
            "release-windows-x64",
        }
        expected_producers = {"macos-arm64", "linux-x64", "windows-x64"}

        producer_jobs = set(
            re.findall(r"(?m)^  (produce_[a-z]+):$", workflow)
        )
        self.assertEqual(producer_jobs, expected_jobs)

        publisher = workflow[workflow.index("  publish:") :]
        required_results = set(
            re.findall(r"needs\.(produce_[a-z]+)\.result == 'success'", publisher)
        )
        downloaded_artifacts = set(
            re.findall(r"(?m)^          name: (release-[a-z0-9-]+)$", publisher)
        )
        required_producers = set(
            re.findall(r"--required-producer ([a-z0-9-]+)", publisher)
        )

        self.assertEqual(required_results, expected_jobs)
        self.assertEqual(downloaded_artifacts, expected_artifacts)
        self.assertEqual(required_producers, expected_producers)

    def test_release_is_verified_before_the_draft_becomes_public(self):
        workflow = (REPO_ROOT / ".github/workflows/macos-release.yml").read_text()

        create = workflow.index("CREATE_RESPONSE=")
        verify = workflow.index("verify-github-release")
        publish = workflow.index("--method PATCH")
        self.assertLess(create, verify)
        self.assertLess(verify, publish)

    def test_failed_draft_cleanup_is_bound_to_the_created_release_id(self):
        workflow = (REPO_ROOT / ".github/workflows/macos-release.yml").read_text()
        cleanup = workflow[
            workflow.index("cleanup_draft()") : workflow.index("trap cleanup_draft")
        ]

        self.assertIn("releases/$DRAFT_RELEASE_ID", cleanup)
        self.assertIn("--expected-release-id", cleanup)
        self.assertIn("verify-draft-identity", cleanup)
        self.assertIn("--method DELETE", cleanup)
        self.assertNotIn("releases/tags/", cleanup)
        self.assertNotIn("gh release delete", cleanup)

    def test_official_actions_are_immutably_pinned(self):
        for relative in (
            ".github/workflows/ci.yml",
            ".github/workflows/macos-release.yml",
        ):
            workflow = (REPO_ROOT / relative).read_text()
            uses = re.findall(r"^\s+uses: ([^\s#]+)", workflow, re.MULTILINE)
            self.assertTrue(uses)
            for action in uses:
                with self.subTest(workflow=relative, action=action):
                    self.assertRegex(action, r"^[^@]+@[0-9a-f]{40}$")

    def test_windows_ci_has_no_forced_bash_steps(self):
        workflow = (REPO_ROOT / ".github/workflows/ci.yml").read_text()

        shared = workflow[: workflow.index("  linux_appimage:")]
        self.assertNotIn("shell: bash", shared)
        self.assertIn("if: runner.os == 'Linux'", shared)


if __name__ == "__main__":
    unittest.main()
