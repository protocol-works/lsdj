# Linux release qualification — issue #112

Hosted CI is not hardware qualification. Attach this completed record to issue
#112 for each proposed minimum configuration and for both a real Wayland and a
real Xorg session.

## Exact environment

- [ ] LSDJ tag and source revision:
- [ ] AppImage filename and SHA-256:
- [ ] Ubuntu version and kernel:
- [ ] Session type and desktop/compositor:
- [ ] CPU and RAM:
- [ ] GPU and VRAM:
- [ ] NVIDIA driver, PyTorch, CUDA runtime, MRT2 dependency/model revisions:
- [ ] Stable Audio/TFLite/model/LoRA revisions:
- [ ] Main/cue audio devices, sample formats/rates/channels, buffer frames:
- [ ] MIDI controller model, firmware, and raw ALSA port names:
- [ ] Relevant udev/session ACL configuration:

## Clean install and desktop

- [ ] On a clean Ubuntu 22.04+ x86_64 user account, checksum verification and
      executable-bit setup lead to a normal AppImage launch without developer
      tools, system Python, Git, a shell command, or a CUDA toolkit.
- [ ] Native titlebar/window behavior, file/folder dialogs, opener, Trash, and
      every notification used by LSDJ behave correctly.
- [ ] Repeat on Wayland and Xorg, including paths/home names with spaces and
      non-ASCII characters.
- [ ] Normal exit, forced app exit, and failed worker startup leave no worker
      descendants.

## Runtime/model installation

- [ ] First download shows exact revisions, terms/links, storage, backend, and
      driver compatibility before work begins.
- [ ] MRT2 and Stable Audio runtimes/models install from verified pins into XDG
      assets/staging roots; corrupt, interrupted, cancelled, and failed updates
      retain the previous verified version.
- [ ] Offline launch after successful installation does not invoke or require
      system Python, `uv`, Git, shell tools, a CUDA toolkit, or network access.
- [ ] Insufficient disk, RAM/VRAM, incompatible driver, authentication, and
      verification failures are actionable and redact credentials.

## Audio and lifecycle

- [ ] ALSA direct and PipeWire-ALSA paths enumerate and play the intended
      devices; record which path each device used.
- [ ] Validate default-device selection/change, 48 kHz and non-48 kHz devices,
      f32/i16/u16 where offered, mono/stereo/multichannel conversion, and
      unsupported-layout errors.
- [ ] FLX4 combined routing sends master to channels 1/2 and cue to 3/4; split
      main/cue routing also works.
- [ ] Unplug/replug, PipeWire restart, default-device change, suspend/resume,
      and app restart recover clearly without callback stalls.

## MIDI and FLX4

- [ ] FLX4 is usable without an unsafe blanket udev rule; record any required
      group/session ACL or device-specific `uaccess` rule.
- [ ] ALSA port-name variants normalize for matching while the raw port remains
      selectable and reconnects to the same device.
- [ ] Validate hotplug/reconnect, transport, mixer controls, jog wheels, pad
      modes, LEDs, position-query SysEx, and the in-app MIDI monitor.
- [ ] DDJ-400 remains best-effort regression evidence, not a release blocker.

## Sustained MRT2 performance

- [ ] Run both decks for at least 10 minutes at 25 frames (approximately 1 s).
- [ ] Run both armed decks for at least 10 minutes at 5 frames (approximately
      200 ms).
- [ ] Both runs have zero **engine-reported** underruns.
- [ ] Capture p50/p95/p99 generation latency, queue depth, audio buffer settings,
      CPU/RAM/VRAM, temperature, and throttling notes.

## Stable Audio parity while decks remain live

- [ ] Music and SFX generation.
- [ ] Audio-to-audio, continuation, and inpainting.
- [ ] Small and Medium models, positive/negative prompts, all exposed sampling
      controls, LoRA selection/application, preview, output naming, and corrupt
      output validation.
- [ ] Cancellation and long-duration validation, including the supported Medium
      maximum, without blocking the audio callback or causing deck underruns.
- [ ] Record CPU/RAM use and the queue/constrain/pause policy used to protect
      both live decks.

## Release decision

- [ ] #108 licensing/acknowledgement release gate complete.
- [ ] #110 production PyTorch MRT2 adapter complete and qualified.
- [ ] #111 Stable Audio TFLite adapter complete and qualified.
- [ ] Linux producer bundle, native dependency audit, checksum, and deterministic
      tag/revision metadata pass the single-publisher verification.
- [ ] Known limitations and measured minimum CPU/RAM/GPU/VRAM/driver/storage
      requirements are published.
- [ ] Issue #112 records an explicit go/no-go decision and links this evidence.
