# Windows 11 x64 packaging and operations

Windows is a release candidate, not yet a supported LSDJ platform. The software
packaging and hosted-CI contracts in this document do not replace the unchecked
NVIDIA, WASAPI, MIDI, FLX4, sleep/resume, and Defender qualification in
[`windows-release-checklist.md`](windows-release-checklist.md).

## Installer contract

LSDJ produces one x64 NSIS `-setup.exe` for Windows 11. It installs for the
current user and does not request administrator privileges or add a firewall
rule. The installer creates an LSDJ Start menu entry, records calendar-version
metadata derived from the protected `vYYYY.MM.N` release tag, permits upgrades,
and refuses downgrades. MSI, Microsoft Store, portable ZIP, Windows 10, Windows
on ARM, and per-machine installation are outside this release.

Application files and app-managed files use a deliberately shallow
`%LOCALAPPDATA%\LSDJ` tree so Python environments and model paths continue to
work when Windows long-path support is disabled:

- `config` — LSDJ settings;
- `data` — generated songs, samples, and user registries;
- `cache` — reproducible cache data;
- `assets` — verified model weights and managed runtimes;
- `staging` — interrupted candidates on the same filesystem as `assets`;
- `backend\current\lsdj_backend.exe` — the stable launcher atomically promoted
  by the #110/#111 runtime work.

The packaged shell enables the `managed-runtime` feature. If the verified
launcher is absent, decks and generation report that the managed runtime is not
installed. They never fall through to a system Python, `uv`, Git, a CUDA toolkit,
WSL, or a shell command. The launcher is a narrow packaging seam; #110 and #111
remain responsible for installing and selecting the PyTorch MRT2 and TFLite
Stable Audio implementations behind it.

## Upgrade and uninstall

An upgrade replaces application payloads in place and preserves the entire
app-managed tree. A normal uninstall removes the app binary, declared packaged
resources, registry entry, and shortcuts, while preserving downloaded models,
runtimes, settings, and user data.

The graphical uninstaller offers an unchecked option to remove the preserved
data. If selected, it calculates the tree size and presents a second confirmation
showing `%LOCALAPPDATA%\LSDJ` and the measured KiB before deletion. Removal is
allowed only while the installer-owned `.lsdj-data-root` marker is present. The
marker must also contain LSDJ's exact application identifier; a same-named or
empty file is not sufficient. The equivalent explicit automation switch is
`/PURGE-LSDJ-DATA`; `/S` alone always preserves data. There is intentionally no
broad or caller-supplied recursive target.

## WebView2

Windows 11 normally receives the evergreen WebView2 Runtime with Windows. LSDJ's
NSIS installer still uses Tauri's `downloadBootstrapper` mode when the runtime is
missing. This keeps the installer small and lets Microsoft's evergreen runtime
receive security updates independently.

If WebView2 is already present, installation works offline. If it is absent, the
bootstrapper needs a network connection; download or installation failure aborts
with an actionable message instead of installing an app that cannot open. Users
may install Microsoft's WebView2 Evergreen Runtime separately and retry. Moving
to Tauri's roughly 127 MB `offlineInstaller` mode is a future reviewed release
policy change, not an automatic fallback.

## Local services and firewall

The Rust host allocates ephemeral ports and every model service binds only to
`127.0.0.1`. The installer opens no inbound port, adds no public-network binding,
and creates no Windows Firewall exception. Child processes live in a Windows Job
Object and are terminated as a tree on quit or host failure. Hosted CI exercises
the process contract without model hardware; abnormal exit with real CUDA work
remains a physical-machine gate.

## Authenticode release gate

Unsigned development installers are produced only in pull-request CI and are
labelled `windows-x64-unsigned-development`. They are not release inputs. CI runs
the release verifier against them and requires rejection.

A protected `windows-release` Environment gates the release producer. The repo
defines a provider-neutral, non-shell interface:

- `WINDOWS_SIGN_COMMAND_PATH` maps to
  `LSDJ_WINDOWS_SIGN_COMMAND_PATH`, an absolute protected path to a reviewed
  wrapper that accepts exactly one file path;
- `WINDOWS_EXPECTED_CERTIFICATE_SHA1` maps to the exact approved leaf
  certificate thumbprint; and
- `WINDOWS_EXPECTED_SUBJECT` maps to the exact approved certificate subject and
  expected Windows publisher identity.

The selected provider must provision its credentialed wrapper after Environment
approval. The wrapper owns key access and timestamp-server configuration. The
repo never accepts a command string, PFX, password, or unverified subject. The
protected preflight also requires the Windows SDK `signtool.exe`. Every sign
operation immediately requires a valid Authenticode chain, exact leaf thumbprint
and subject, a timestamp certificate, and successful `signtool /pa` verification.
The installed app, uninstaller, executable payloads, and final NSIS installer are
verified again before the producer bundle is uploaded.

No provider, certificate, publisher subject, protected CI identity, key storage,
rotation process, or revocation process has been selected yet. Consequently the
release job intentionally fails at signing preflight today and no output from
this branch is represented as signed. The owner decisions and operational drill
are explicit gates in the release checklist.

The single publisher has no signing credentials. It requires the exact
`macos-arm64` and `windows-x64` producer set, recomputes sizes and SHA-256 hashes,
and refuses to create a public release if Windows production, signature
verification, or artifact verification is missing.

## Defender and SmartScreen response

Authenticode establishes publisher and file integrity; it does not guarantee
SmartScreen reputation or that Microsoft Defender and third-party products will
never flag a new build.

For a report:

1. Do not advise bypassing or disabling protection. Quarantine the artifact and
   record the LSDJ version, download URL, SHA-256, signature status, Windows
   build, Defender platform/engine/security-intelligence versions, and detection
   name.
2. Compare the file with the release index and verify the expected Authenticode
   subject, thumbprint, and timestamp. Treat any mismatch as a security incident;
   keep the release private or withdraw it and begin the provider's revocation
   procedure.
3. If identity and hashes match, reproduce on a clean Windows 11 system with
   current definitions and submit the exact artifact to Microsoft's malware
   analysis portal as a suspected false positive. Preserve the submission ID in
   the linked GitHub issue.
4. Publish the vendor disposition. Rebuild only from the protected tag workflow;
   never re-sign or replace a published asset by hand.

The final expected publisher text, support contact, and certificate incident
owner must be filled in after the provider decision and before Windows support is
announced.

## Diagnostics and known limitations

- Confirm the installer hash against `SHA256SUMS.txt` and `release-index.json`.
- In Explorer, open **Properties → Digital Signatures** and require the publisher
  documented for the release. Do not install if the signature is absent or
  invalid.
- Model/runtime data and partial-download staging live under
  `%LOCALAPPDATA%\LSDJ`; include sizes and runtime/model revisions in a report,
  but never attach model weights or credentials.
- Local service logs are bounded and credential-redacted. There is no remote
  service or firewall troubleshooting step because the supported binding is
  loopback only.
- The minimum NVIDIA GPU, VRAM, driver, PyTorch/CUDA runtime, CPU, RAM, and free
  disk are deliberately unspecified until measured qualification completes.
- WASAPI formats and device recovery, WinMM MIDI, FLX4 routing and LEDs,
  sleep/resume, and model performance are not qualified by hosted CI.

## Build and CI

`scripts/build-windows-installer.ps1` accepts an exact release-shaped tag and
derives Windows/Tauri version metadata. `-UnsignedDevelopment` passes Tauri's
`--no-sign` and is the only pull-request build mode. `-Release` loads the
sign-command configuration and fails before packaging when protected identity
configuration is unavailable.

Hosted `windows-2025` CI builds two unsigned versions, installs and upgrades
them, rejects a downgrade, verifies default preservation and explicit purge,
checks version metadata and the Start menu shortcut, and exercises a conservative
sub-`MAX_PATH` install location containing spaces and Unicode. These checks do
not claim hardware or antivirus qualification.
