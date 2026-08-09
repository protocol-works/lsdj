# Linux AppImage support

LSDJ's Linux target is Ubuntu 22.04 or newer on x86_64 with a supported NVIDIA
GPU. The AppImage packaging, desktop configuration, XDG storage contract, and
fail-closed release producer are implemented as part of issue #112. A public
Linux release remains gated on the production PyTorch MRT2 backend (#110), the
portable Stable Audio TFLite backend (#111), the model licensing flow (#108),
and the real-hardware checklist below.

Passing hosted CI proves that the shell compiles, the AppImage extracts, its
desktop metadata/resources are present, and it can open in a virtual X11
session with paths containing spaces and Unicode. It does **not** qualify an
NVIDIA driver, PipeWire/ALSA device, Wayland compositor, FLX4, suspend/resume,
or model performance.

## Install and verify

Download the `.AppImage` and the release's `SHA256SUMS.txt`/producer metadata
from the same GitHub Release. Verify the checksum before launch, then make the
file executable and open it:

```sh
sha256sum --check linux-x64-SHA256SUMS.txt
chmod +x LSDJ_*.AppImage
./LSDJ_*.AppImage
```

The AppImage is the only supported Linux package. `.deb`, `.rpm`, Flatpak,
Snap, ARM64, AMD/Intel GPU acceleration, and JACK-specific integration are not
part of the supported target. Other distributions may work, but are community
configurations until separately qualified.

The application package does not invoke or require a system Python, `uv`, Git,
a shell, or a CUDA toolkit. Model adapters are installed into app-owned storage
from pinned, checksum-verified artifacts. If a managed adapter is absent or
invalid, the corresponding service reports unavailable instead of falling back
to a command from `PATH`. MRT2 and Stable Audio are resolved independently from
their own verified service manifests, so one missing runtime does not redirect
or downgrade the other service. A compatible NVIDIA **driver** is still
required for MRT2; the minimum version and VRAM floor remain unset until the
#110 hardware qualification records measured results.

If FUSE is unavailable, AppImage's standard extract-and-run mode is a useful
diagnostic fallback:

```sh
APPIMAGE_EXTRACT_AND_RUN=1 ./LSDJ_*.AppImage
```

The release gate still tests ordinary AppImage packaging; extract-and-run is
not a substitute for the clean-machine qualification.

## Storage and first run

Rust resolves the roots once and passes them explicitly to every service. The
default Linux layout is:

| Purpose | Default path |
| --- | --- |
| Configuration | `$XDG_CONFIG_HOME/lsdj` or `~/.config/lsdj` |
| Durable data | `$XDG_DATA_HOME/lsdj` or `~/.local/share/lsdj` |
| Models/runtimes | `$XDG_DATA_HOME/lsdj/assets` |
| Same-filesystem install staging | `$XDG_DATA_HOME/lsdj/staging` |
| Disposable cache | `$XDG_CACHE_HOME/lsdj` or `~/.cache/lsdj` |

The model manager owns first download, verification, installation, update,
rollback, cancellation, and recovery. Downloads that require upstream terms or
credentials remain blocked until #108's current-revision acknowledgement flow
authorizes them. A failed or interrupted update must leave the prior verified
runtime usable.

## Audio: ALSA and PipeWire

The Rust audio host uses CPAL's ALSA backend. On a PipeWire desktop, the
distribution's PipeWire ALSA compatibility layer routes those streams; a
PulseAudio desktop follows the same ALSA-facing application path. LSDJ does not
invoke `pw-*`, `pactl`, `aplay`, or another external audio utility.

The in-app `platform_diagnostics` response records whether `/dev/snd`, the
PipeWire socket, and the Pulse socket are visible. It reports evidence only; a
socket's presence is not a successful audio-device test. The following stable
advisory codes are intended for localized UI/support surfaces:

- `linux.audio.alsaDevicesMissing`
- `linux.session.notDetected`
- `linux.distribution.notSupported`

Default-device changes, 44.1/48 kHz conversion, stereo and FLX4 four-channel
routing, device removal, PipeWire restart, and suspend/resume must all be
verified on real systems before release.

## MIDI and device permissions

Linux MIDI uses ALSA sequencer through `midir`. Port matching preserves the raw
ALSA name used to open the device while normalizing case, punctuation, and ALSA
client/port suffixes for FLX4/DDJ-400 identification.

`platform_diagnostics` reports `/dev/snd/seq` as `available`,
`permissionDenied`, or `missing` without opening a sequencer client. Its stable
advisory codes are:

- `linux.midi.sequencerPermissionDenied`
- `linux.midi.sequencerMissing`

Ubuntu desktop sessions normally grant sound-device access through logind/udev.
If access is denied, first reconnect the controller and sign out/in so the
session ACL can refresh. On a system administered through the traditional
`audio` group, an administrator may add the user to that group and require a
new login. Do not install a blanket world-writable udev rule. A custom rule, if
the distribution truly needs one, must match the controller's measured vendor
and product IDs and grant active-session `uaccess`; record that rule and the
`udevadm info` evidence in the qualification report.

## Desktop integration

The Linux overlay uses the desktop's normal decorated titlebar and produces a
single Audio/Music `.desktop` entry and icon. Tauri's native dialog, opener, and
trash integrations remain scoped through Rust; the webview receives no general
filesystem/opener permission. Hosted CI verifies the packaged entry point,
resource layout, executable bits, ELF dependency inventory, and a virtual-X11
launch using isolated XDG roots with spaces and non-ASCII characters.

Real Wayland and Xorg sessions must still validate window behavior, native file
and folder dialogs, opener/trash behavior, notifications used by the app,
multi-monitor/scale behavior, and clean shutdown. No notification behavior is
claimed merely because the package launches in Xvfb.

## Diagnostics and support bundle facts

The `platform_diagnostics` command exposes:

- OS/architecture and Ubuntu support classification;
- detected Wayland/X11 session type;
- the resolved config/data/cache/assets/staging roots;
- ALSA, PipeWire/Pulse socket, and MIDI-sequencer evidence;
- `developerFallbackAllowed` (always `false` in the AppImage); and
- runtime mode (`managed` for the Linux package).

These facts contain no tokens and do not execute external diagnostic tools.
Model/runtime revisions, NVIDIA driver/VRAM, generation latency, queue depth,
and underruns belong to the #110/#111 service diagnostics once those adapters
are integrated.

See [the Linux qualification checklist](linux-qualification-checklist.md) for
the evidence required before calling the platform supported.
