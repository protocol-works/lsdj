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
        self.assertIn('!define LSDJ_OWNER_ID "works.protocol.lsdj"', hooks)
        self.assertIn("NSIS_HOOK_PREINSTALL", hooks)
        self.assertIn("GetFullPathNameW", hooks)
        self.assertIn("CreateFileW", hooks)
        self.assertIn("LSDJ_FILE_FLAG_OPEN_REPARSE_POINT", hooks)
        self.assertIn("!define LSDJ_OWNER_ID_BYTES 19", hooks)
        self.assertIn("!define LSDJ_OWNER_ID_READ_BYTES 20", hooks)
        self.assertEqual(len("works.protocol.lsdj".encode("ascii")), 19)
        self.assertIn("System::Alloc 52", hooks)
        self.assertIn(
            "GetFileInformationByHandle(p R6, p R7)",
            hooks,
        )
        self.assertIn("System::Call '*$R7(&i4 .R8)'", hooks)
        self.assertIn("System::Free $R7", hooks)
        self.assertIn(
            "ReadFile(p R6, m .R8, i ${LSDJ_OWNER_ID_READ_BYTES}, *i .R7, p 0)",
            hooks,
        )
        self.assertNotIn("GetFileInformationByHandle(p R6, *(", hooks)
        self.assertNotIn('FileOpen $R5 "${LSDJ_DATA_MARKER}" r', hooks)
        marker_validator_start = hooks.index("!macro LSDJ_DEFINE_MARKER_VALIDATOR")
        marker_validator_end = hooks.index("!macroend", marker_validator_start)
        marker_validator = hooks[marker_validator_start:marker_validator_end]
        self.assertIn(
            "${OrIf} $R7 != ${LSDJ_OWNER_ID_BYTES}",
            marker_validator,
        )
        self.assertIn("LSDJ_FILE_ATTRIBUTE_REPARSE_POINT", hooks)
        self.assertIn("LsdjExistingLayoutIsRecognized", hooks)
        self.assertIn("Section -LsdjProbeDataRootBeforeTauri", hooks)
        self.assertIn("StrCpy $LsdjInstallRootState 1", hooks)
        self.assertIn("LsdjDataRootIsEmpty", hooks)
        self.assertIn("LsdjInstallTreeIsLinkFree", hooks)
        self.assertIn('CreateDirectory "${LSDJ_DATA_ROOT}"', hooks)
        self.assertIn("LsdjOwnedDataRootIsSafe", hooks)
        self.assertIn("LsdjTreeIsLinkFree", hooks)
        self.assertIn("LsdjDeleteTreeWithoutLinks", hooks)
        self.assertIn("IfErrors lsdj_install_tree_empty_candidate", hooks)
        self.assertIn("IfErrors lsdj_empty_recheck", hooks)
        self.assertIn("IfErrors lsdj_tree_empty_candidate", hooks)
        self.assertIn("IfErrors lsdj_delete_empty_candidate", hooks)
        for empty_recheck in (
            "lsdj_install_tree_empty_candidate:",
            "lsdj_empty_recheck:",
            "lsdj_tree_empty_candidate:",
            "lsdj_delete_empty_candidate:",
        ):
            recheck_start = hooks.index(empty_recheck)
            recheck = hooks[recheck_start : recheck_start + 700]
            self.assertIn("GetFileAttributesW", recheck)
            self.assertIn("LSDJ_FILE_ATTRIBUTE_REPARSE_POINT", recheck)
            self.assertIn("LSDJ_FILE_ATTRIBUTE_DIRECTORY", recheck)
        self.assertIn("/PURGE-LSDJ-DATA", hooks)
        self.assertIn("${GetSize}", hooks)
        self.assertIn("Location: ${LSDJ_DATA_ROOT}", hooks)
        self.assertIn("Size: $R8 KiB", hooks)
        self.assertIn("StrCpy $LsdjDeleteData $DeleteAppDataCheckboxState", hooks)
        self.assertIn("StrCpy $DeleteAppDataCheckboxState 0", hooks)
        self.assertIn("${If} $LsdjDeleteData = 1", hooks)
        preuninstall_start = hooks.index("!macro NSIS_HOOK_PREUNINSTALL")
        preuninstall_end = hooks.index("!macroend", preuninstall_start)
        preuninstall = hooks[preuninstall_start:preuninstall_end]
        self.assertIn(
            "preuninstall: owned root safe=$LsdjOwnedRootSafe tree safe=$LsdjTreeSafe",
            preuninstall,
        )
        self.assertIn("abort: unsafe data removal", preuninstall)
        self.assertIn("SetErrorLevel 2\n      Quit", preuninstall)
        self.assertNotIn('Abort "Refusing unsafe LSDJ data removal."', preuninstall)
        self.assertIn('DeleteRegKey SHCTX "${MANUPRODUCTKEY}"', hooks)
        self.assertNotIn("RMDir /r", hooks)
        self.assertNotIn('RMDir /r "$LOCALAPPDATA"', hooks)

        probe_start = hooks.index("Section -LsdjProbeDataRootBeforeTauri")
        probe_end = hooks.index("SectionEnd", probe_start)
        probe = hooks[probe_start:probe_end]
        preinstall_start = hooks.index("!macro NSIS_HOOK_PREINSTALL")
        create_start = hooks.index('CreateDirectory "${LSDJ_DATA_ROOT}"')
        self.assertLess(probe_start, probe_end)
        self.assertLess(probe_end, preinstall_start)
        self.assertLess(preinstall_start, create_start)
        self.assertNotIn("CreateDirectory", probe)
        preinstall_end = hooks.index("!macroend", preinstall_start)
        preinstall = hooks[preinstall_start:preinstall_end]
        self.assertIn("!define MUI_CUSTOMFUNCTION_GUIINIT LsdjRejectPassiveMode", hooks)
        passive_start = hooks.index("Function LsdjRejectPassiveMode")
        passive_end = hooks.index("FunctionEnd", passive_start)
        passive_callback = hooks[passive_start:passive_end]
        self.assertIn("!insertmacro LSDJ_REJECT_PASSIVE_MODE", passive_callback)
        passive_macro_start = hooks.index("!macro LSDJ_REJECT_PASSIVE_MODE")
        passive_macro_end = hooks.index("!macroend", passive_macro_start)
        passive_macro = hooks[passive_macro_start:passive_macro_end]
        self.assertIn(
            '${GetOptions} $CMDLINE "/P" $LsdjPassiveRequested', passive_macro
        )
        self.assertIn("SetErrorLevel 2", passive_macro)
        self.assertIn("Quit", passive_macro)
        self.assertLess(passive_end, preinstall_start)
        self.assertTrue(
            preinstall.lstrip().startswith(
                "!macro NSIS_HOOK_PREINSTALL\n  !insertmacro LSDJ_REJECT_PASSIVE_MODE"
            )
        )
        self.assertIn("${If} ${Silent}", preinstall)
        self.assertNotIn("$PassiveMode", preinstall)
        self.assertNotIn("${AndIf} $UpdateMode != 1", preinstall)
        self.assertIn(
            'ReadRegStr $LsdjInstalledVersion SHCTX "${UNINSTKEY}" "DisplayVersion"',
            preinstall,
        )
        self.assertIn(
            'ReadRegStr $LsdjRegistryEvidence SHCTX "${UNINSTKEY}" "UninstallString"',
            preinstall,
        )
        self.assertIn(
            '${FileExists} "$INSTDIR\\${MAINBINARYNAME}.exe"',
            preinstall,
        )
        self.assertIn(
            'nsis_tauri_utils::SemverCompare "$LsdjInstalledVersion" "lsdj-invalid-semver"',
            preinstall,
        )
        self.assertIn('${If} "$LsdjInstalledVersion" == ""', preinstall)
        self.assertNotIn('${If} $LsdjInstalledVersion = ""', preinstall)
        self.assertIn(
            'nsis_tauri_utils::SemverCompare "${VERSION}" "$LsdjInstalledVersion"',
            preinstall,
        )
        self.assertIn("${If} $LsdjVersionCompare = -1", preinstall)
        self.assertIn("${ElseIf} $LsdjVersionCompare != 0", preinstall)
        self.assertIn("${AndIf} $LsdjVersionCompare != 1", preinstall)
        self.assertIn("abort: invalid version comparison", preinstall)
        version_guard = preinstall[
            : preinstall.index("Call LsdjCanonicalDataRootIsValid")
        ]
        self.assertNotIn("$R6", version_guard)
        self.assertNotIn("$R7", version_guard)
        self.assertIn("SetErrorLevel 2", preinstall)
        self.assertLess(
            preinstall.index("SetErrorLevel 2"),
            preinstall.index("Call LsdjCanonicalDataRootIsValid"),
        )

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
        build = (REPO_ROOT / "scripts/build-windows-installer.ps1").read_text()
        lifecycle = (REPO_ROOT / "scripts/test-windows-installer.ps1").read_text()
        windows_doc = (REPO_ROOT / "docs/windows.md").read_text()
        hooks = (TAURI_ROOT / "windows/installer-hooks.nsh").read_text()

        self.assertIn("-UnsignedDevelopment", workflow)
        self.assertIn("assert-windows-release-rejects-unsigned.ps1", workflow)
        self.assertIn("test-windows-installer.ps1", workflow)
        self.assertIn("windows-x64-unsigned-development", workflow)
        self.assertIn("cargo install tauri-cli --version '=2.11.2' --locked", workflow)
        self.assertIn("LSDJ_CI_ADVERSARIAL_TESTS", build)
        self.assertIn("if ($UnsignedDevelopment)", build)
        self.assertIn('FileOpen $R5 "$TEMP\\lsdj-ci-installer.trace" a', hooks)
        self.assertIn("FileSeek $R5 0 END", hooks)
        self.assertIn("Var LsdjCiTraceHadErrors", hooks)
        self.assertIn("Var LsdjCiTraceMessage", hooks)
        self.assertIn('StrCpy $LsdjCiTraceMessage "${MESSAGE}"', hooks)
        trace_else = hooks.index("!else", hooks.index("!macro LSDJ_CI_TRACE MESSAGE"))
        trace_end = hooks.index("!endif", trace_else)
        self.assertEqual(
            hooks[trace_else:trace_end].count("FileOpen"),
            0,
            "Production trace macro must expand to no file operations.",
        )
        self.assertIn("Get-CiInstallerTrace", lifecycle)
        self.assertIn("Write-CiInstallerTrace", lifecycle)
        self.assertIn("CI installer trace:", lifecycle)
        self.assertIn("function Get-InstalledStateSnapshot", lifecycle)
        self.assertIn("function Get-UninstallRegistrySnapshot", lifecycle)
        self.assertIn("function Assert-CiInstallerTraceContract", lifecycle)
        self.assertIn("function Set-DisplayVersionEvidence", lifecycle)
        self.assertIn("function New-UninstallerWorkerCopy", lifecycle)
        self.assertIn("function Stop-UninstallerWorker", lifecycle)
        self.assertIn("function Invoke-ExpectedUninstallFailure", lifecycle)
        self.assertNotIn("Invoke-ExpectedFailure $uninstaller", lifecycle)
        copy_helper_start = lifecycle.index("function New-UninstallerWorkerCopy")
        stop_helper_start = lifecycle.index(
            "function Stop-UninstallerWorker", copy_helper_start
        )
        worker_helper_start = lifecycle.index(
            "function Invoke-ExpectedUninstallFailure"
        )
        copy_helper = lifecycle[copy_helper_start:stop_helper_start]
        stop_helper = lifecycle[stop_helper_start:worker_helper_start]
        worker_helper_end = lifecycle.index(
            "function Require-InstalledVersion", worker_helper_start
        )
        worker_helper = lifecycle[worker_helper_start:worker_helper_end]
        self.assertIn("} catch {", copy_helper)
        self.assertIn("Remove-Item -LiteralPath $workerPath", copy_helper)
        self.assertIn("$Process.Kill($true)", stop_helper)
        self.assertIn("$Process.WaitForExit($TimeoutMilliseconds)", stop_helper)
        self.assertIn(
            '-ArgumentList (@($ArgumentList) + "_?=$InstallDirectory")',
            worker_helper,
        )
        self.assertIn("$worker.WaitForExit(30000)", worker_helper)
        self.assertIn("$worker.ExitCode -ne 2", worker_helper)
        self.assertIn("} finally {", worker_helper)
        self.assertIn("Stop-UninstallerWorker -Process $worker", worker_helper)
        self.assertIn(
            "Remove-Item -LiteralPath $workerPath",
            worker_helper,
        )
        race_start = lifecycle.index(
            "Start-LifecycleScenario 'reject ownership-marker replacement"
        )
        race_end = lifecycle.index(
            "Start-LifecycleScenario 'reject purge with nested junction'",
            race_start,
        )
        race = lifecycle[race_start:race_end]
        self.assertIn('"_?=$dataRoot"\n        )', race)
        self.assertIn("$racedPurge.ExitCode -ne 2", race)
        self.assertIn("$racedPurge.WaitForExit(30000)", race)
        self.assertIn("} finally {", race)
        self.assertIn("Stop-UninstallerWorker -Process $racedPurge", race)
        self.assertGreaterEqual(race.count("} finally {"), 2)
        self.assertIn(
            "Remove-Item -LiteralPath $racedPurgeWorker",
            race,
        )
        unicode_worker_start = lifecycle.index(
            "Start-LifecycleScenario 'reject purge worker with spaces and Unicode"
        )
        unicode_worker_end = lifecycle.index(
            "Invoke-CheckedProcess $unicodeUninstaller @('/S')", unicode_worker_start
        )
        unicode_worker = lifecycle[unicode_worker_start:unicode_worker_end]
        self.assertIn("-FilePath $unicodeUninstaller", unicode_worker)
        self.assertIn("-InstallDirectory $unicodeInstall", unicode_worker)
        self.assertIn("Invoke-ExpectedUninstallFailure", unicode_worker)
        self.assertNotIn(".WaitForExit()", race)
        self.assertIn(
            "NSIS installed uninstallers are self-copy launchers", windows_doc
        )
        self.assertIn("final unquoted `_?=<install-directory>`", windows_doc)
        self.assertIn("fail-closed refusal (`2`)", windows_doc)
        self.assertIn("$lastRequiredIndex = -1", lifecycle)
        self.assertIn("[StringComparison]::Ordinal", lifecycle)
        self.assertIn(
            "Get-FileHash -LiteralPath $uninstaller -Algorithm SHA256", lifecycle
        )
        self.assertIn("Assert-InstalledStateSnapshotUnchanged", lifecycle)
        self.assertIn("MarkerAttributes", lifecycle)
        self.assertIn("MarkerLinkType", lifecycle)
        self.assertIn("MarkerTarget", lifecycle)
        self.assertIn("[IO.FileAttributes]::ReparsePoint", lifecycle)
        self.assertIn("-ExpectedExitCodes @(2)", lifecycle)
        for contract in (
            "pre-existing empty LocalAppData root",
            "foreign LocalAppData root",
            "root junction",
            "purge-time root junction",
            "marker reparse point",
            "NUL-extended ownership marker",
            "marker-replacement test",
            "nested directory reparse point",
            "reject unattended downgrade in update mode",
            "reject passive downgrade",
            "reject combined silent and passive mode",
            "same-version silent reinstall",
            "reject existing install with empty version metadata",
            "reject existing install with corrupt version metadata",
            "reject existing install with missing version metadata",
        ):
            self.assertIn(contract, lifecycle)
        self.assertIn("function Start-LifecycleScenario", lifecycle)
        self.assertIn("preinstall: version compare=-1", lifecycle)
        self.assertIn("preinstall: version compare=0", lifecycle)
        self.assertIn("preinstall: version compare=1", lifecycle)
        self.assertIn("validity=1", lifecycle)
        self.assertIn("validity=0", lifecycle)
        self.assertIn("abort: passive install unsupported", lifecycle)
        self.assertIn("preinstall: version evidence installed=", hooks)
        self.assertIn("Require-No-Workers", lifecycle)
        self.assertIn(
            "adopt recognized markerless legacy layout",
            lifecycle,
        )

        rejection = (
            REPO_ROOT / "scripts/assert-windows-release-rejects-unsigned.ps1"
        ).read_text()
        self.assertIn("Authenticode signature status is NotSigned", rejection)
        success_exit = rejection.rindex("exit 0")
        self.assertGreater(success_exit, rejection.index("if ($exitCode -eq 0)"))
        self.assertGreater(success_exit, rejection.index("if ($rendered -notmatch"))

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

    def test_managed_backend_feature_forbids_system_python_fallback(self):
        cargo = (TAURI_ROOT / "Cargo.toml").read_text()
        lib = (TAURI_ROOT / "src/lib.rs").read_text()
        sidecar = (TAURI_ROOT / "src/sidecar.rs").read_text()
        generation = (TAURI_ROOT / "src/generation.rs").read_text()

        self.assertRegex(cargo, r"(?m)^managed-backend = \[\]$")
        self.assertIn('.join("backend")', lib)
        self.assertIn('.join("current")', lib)
        self.assertIn("LSDJ_MANAGED_BACKEND_REQUIRED", sidecar)
        self.assertIn("LSDJ_MANAGED_BACKEND_REQUIRED", generation)
        self.assertIn("app-managed backend runtime is not installed", sidecar)
        self.assertIn("app-managed backend runtime is not installed", generation)


if __name__ == "__main__":
    unittest.main()
