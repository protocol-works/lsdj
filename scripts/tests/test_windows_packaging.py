import json
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).parents[2]
TAURI_ROOT = REPO_ROOT / "src-tauri"


class WindowsPackagingContractTest(unittest.TestCase):
    def test_nsis_is_current_user_and_blocks_downgrades(self):
        config = json.loads((TAURI_ROOT / "tauri.windows.conf.json").read_text())
        bundle = config["bundle"]
        windows = bundle["windows"]
        nsis = windows["nsis"]

        self.assertEqual(bundle["targets"], ["nsis"])
        self.assertEqual(nsis["installMode"], "currentUser")
        self.assertEqual(nsis["startMenuFolder"], "LSDJ")
        self.assertFalse(windows["allowDowngrades"])
        self.assertEqual(
            windows["webviewInstallMode"],
            {"type": "downloadBootstrapper", "silent": True},
        )

    def test_uninstall_data_removal_is_explicit_disclosed_and_scoped(self):
        hooks = (TAURI_ROOT / "windows/installer-hooks.nsh").read_text()

        self.assertIn('!define LSDJ_DATA_ROOT "$LOCALAPPDATA\\LSDJ"', hooks)
        self.assertIn(".lsdj-data-root", hooks)
        self.assertIn('StrCmp $R8 "works.protocol.lsdj"', hooks)
        self.assertIn("/PURGE-LSDJ-DATA", hooks)
        self.assertIn("${GetSize}", hooks)
        self.assertIn("Location: ${LSDJ_DATA_ROOT}", hooks)
        self.assertIn("Size: $R8 KiB", hooks)
        self.assertIn("StrCpy $LsdjDeleteData $DeleteAppDataCheckboxState", hooks)
        self.assertIn("StrCpy $DeleteAppDataCheckboxState 0", hooks)
        self.assertIn("${If} $LsdjDeleteData = 1", hooks)
        self.assertIn('RMDir /r "${LSDJ_DATA_ROOT}"', hooks)
        self.assertNotIn('RMDir /r "$LOCALAPPDATA"', hooks)

        language = (TAURI_ROOT / "windows/English.nsh").read_text()
        self.assertIn("path and size will be confirmed", language)

    def test_release_signing_uses_only_the_protected_provider_interface(self):
        release = json.loads(
            (TAURI_ROOT / "tauri.windows.release.conf.json").read_text()
        )
        sign = release["bundle"]["windows"]["signCommand"]
        self.assertEqual(sign["command"], "pwsh.exe")
        self.assertIn("../scripts/sign-windows.ps1", sign["args"])
        self.assertIn("%1", sign["args"])

        signer = (REPO_ROOT / "scripts/sign-windows.ps1").read_text()
        verifier = (REPO_ROOT / "scripts/verify-windows-signatures.ps1").read_text()
        for name in (
            "LSDJ_WINDOWS_SIGN_COMMAND_PATH",
            "LSDJ_WINDOWS_EXPECTED_CERTIFICATE_SHA1",
            "LSDJ_WINDOWS_EXPECTED_SUBJECT",
        ):
            self.assertIn(name, signer)
        self.assertIn("TimeStamperCertificate", verifier)
        self.assertIn("signtool.exe", verifier)
        self.assertIn("Get-Command 'signtool.exe'", signer)
        self.assertNotRegex(
            signer, r"(?i)certificate_base64|pfx_password|azure|digicert"
        )

    def test_hosted_ci_builds_unsigned_but_exercises_release_rejection(self):
        workflow = (REPO_ROOT / ".github/workflows/ci.yml").read_text()

        self.assertIn("-UnsignedDevelopment", workflow)
        self.assertIn("assert-windows-release-rejects-unsigned.ps1", workflow)
        self.assertIn("test-windows-installer.ps1", workflow)
        self.assertIn("windows-x64-unsigned-development", workflow)

        rejection = (
            REPO_ROOT / "scripts/assert-windows-release-rejects-unsigned.ps1"
        ).read_text()
        self.assertIn("Authenticode signature status is NotSigned", rejection)

    def test_release_producer_is_required_and_has_no_publish_permission(self):
        workflow = (REPO_ROOT / ".github/workflows/macos-release.yml").read_text()
        producer = workflow[
            workflow.index("  produce_windows:") : workflow.index("  publish:")
        ]

        self.assertIn("environment:\n      name: windows-release", producer)
        self.assertIn("verify-windows-release-install.ps1", producer)
        self.assertIn("--producer windows-x64", producer)
        self.assertNotIn("contents: write", producer)
        self.assertEqual(workflow.count("contents: write"), 1)
        self.assertRegex(workflow, r"(?m)^      - produce_windows$")
        self.assertIn("--required-producer windows-x64", workflow)

    def test_managed_runtime_feature_forbids_system_python_fallback(self):
        cargo = (TAURI_ROOT / "Cargo.toml").read_text()
        lib = (TAURI_ROOT / "src/lib.rs").read_text()
        sidecar = (TAURI_ROOT / "src/sidecar.rs").read_text()
        generation = (TAURI_ROOT / "src/generation.rs").read_text()

        self.assertRegex(cargo, r"(?m)^managed-runtime = \[\]$")
        self.assertIn('.join("backend")', lib)
        self.assertIn('.join("current")', lib)
        self.assertIn("LSDJ_MANAGED_BACKEND_REQUIRED", sidecar)
        self.assertIn("LSDJ_MANAGED_BACKEND_REQUIRED", generation)
        self.assertIn("app-managed backend runtime is not installed", sidecar)
        self.assertIn("app-managed backend runtime is not installed", generation)


if __name__ == "__main__":
    unittest.main()
