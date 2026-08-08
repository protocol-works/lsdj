# Issue #113 — Windows 11 x64 release qualification

This checklist is intentionally unchecked where physical hardware, a selected
signing provider, or current security definitions are required. Hosted CI
evidence must not be substituted for these items.

## Release identity and installer operations

- [ ] Select an Authenticode provider/certificate and record the exact subject,
  leaf thumbprint, timestamp service, legal owner, and expected Explorer
  publisher text.
- [ ] Configure a protected `windows-release` Environment with separate approval,
  no administrator bypass, and a least-privilege CI identity.
- [ ] Document provider credential/key storage, access review, rotation, expiry,
  compromise, incident, and revocation procedures; perform one revocation drill.
- [ ] Confirm the protected provider exposes the reviewed one-file signing wrapper
  contract used by `scripts/sign-windows.ps1`.
- [ ] On a clean Windows 11 x64 account, verify the final NSIS installer, installed
  app, uninstaller, and every executable payload have the exact signer and a
  trusted timestamp.
- [ ] Install, upgrade, attempt a downgrade, uninstall with preservation, and
  uninstall with explicit data removal. Record screenshots of the disclosed
  `%LOCALAPPDATA%\LSDJ` path and measured size.
- [ ] Repeat installation under a profile containing spaces and non-ASCII text,
  with Windows long-path support disabled.
- [ ] Confirm Start menu behavior, window/titlebar, file/folder dialogs,
  notifications, opener/trash behavior, packaged resources, update/restart, and
  WebView2 present/missing/offline failure cases.

## Hardware record

Record exact Windows build, CPU, RAM, GPU, VRAM, NVIDIA driver, PyTorch/CUDA
runtime, audio device/driver, WASAPI rate/format/buffer, MIDI devices, FLX4
firmware/driver, security state, LSDJ revision, and model/runtime revisions.

- [ ] Establish and document the minimum NVIDIA GPU, VRAM, driver, CPU, RAM, and
  free-disk floor from measured results.
- [ ] Run both MRT2 decks for at least ten minutes at 25 frames / approximately
  one second. Require zero engine-reported underruns and capture p50/p95/p99
  generation latency, queue depth, temperature, and throttling.
- [ ] Run both armed decks for at least ten minutes at 5 frames / approximately
  200 ms with the same zero-underrun and telemetry gate.
- [ ] Validate default-device selection/change, WASAPI shared-mode 48 kHz and
  non-48 kHz devices, stereo output, FLX4 four-channel master/cue, removal,
  renegotiation, and sleep/resume.
- [ ] Validate FLX4 WinMM naming, transport, mixer, jog wheels, performance pads,
  LEDs, required SysEx, hotplug/reconnect, and actionable device-contention errors.
- [ ] Run Stable Audio music, SFX, audio-to-audio, continuation, inpainting,
  Small/Medium, LoRA, cancellation, and long-duration validation while both decks
  remain active. Record CPU/RAM impact and deck telemetry.
- [ ] Confirm normal quit, forced host exit, worker crash, update, and uninstall
  leave no Python, model, or GPU worker descendants.

## Security and release response

- [ ] Test the release candidate against current Microsoft Defender definitions;
  record platform, engine, intelligence versions, detection result, and submission
  ID/disposition for any false-positive report.
- [ ] Exercise the documented signing-key compromise and bad-signature release
  stop path without publishing a release.
- [ ] Confirm all model services bind to `127.0.0.1`, the installer creates no
  firewall exception, and no public listener appears during first run or playback.
- [ ] Confirm a clean machine installs verified MRT2 and Stable Audio
  runtimes/models without system Python, Git, CUDA toolkit, WSL, compiler, or shell.
- [ ] Interrupt and corrupt each runtime/model download and promotion; the prior
  verified version must remain usable and diagnostics must identify recovery.
- [ ] Confirm the single publisher refuses missing, unsigned, invalid,
  untimestamped, duplicate, or unexpected Windows artifacts.

## External blockers

- Authenticode provider/certificate, exact publisher subject, timestamp service,
  protected CI identity, and credential lifecycle decisions.
- Physical Windows 11 x64 + supported NVIDIA host with current drivers.
- Pioneer/AlphaTheta DDJ-FLX4 plus representative WASAPI devices.
- Current Defender/SmartScreen observation on the signed release candidate.
- #110 production runtime installation and NVIDIA qualification.
- #111 TFLite Stable Audio runtime installation and parity qualification.
- #108 final notices/acknowledgement and repository licensing decisions.
