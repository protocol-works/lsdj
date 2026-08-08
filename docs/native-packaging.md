# Native packaging (Phase 2 part 6)

How LSDJ ships as a signed, notarized macOS `.app`/`.dmg` with the frozen
Python backend bundled and the model weights kept external. This is **build
engineering** — the research risk was retired by Spike B
([`docs/spike-packaging.md`](spike-packaging.md), the PyInstaller MLX freeze) and
Spike C ([`docs/spike-c-midi.md`](spike-c-midi.md), the Tauri MIDI app) — so the
steps below are reproducible on a Mac with an Apple Developer ID and are also
enforced by the protected release workflow.

## 1. Freeze the backend runtime

```sh
just setup                 # backend .venv with pyinstaller + inference deps
just freeze-backend        # → src-tauri/sidecar-dist/lsdj_backend/ (~1.1 GB)
```

`scripts/freeze-sidecar.sh` is the production form of the Spike B recipe. Its
entry point (`backend/lsdj/frozen.py`) dispatches one shared dependency tree to
the deck sidecar, Magenta model tooling, or FastAPI generation server. ONEDIR
(onefile is unworkable at this size); the metallib is copied next to the exe (the
Spike B "wall"). The 4.3 GB weights are **not** frozen — they stay external (§4).

## 2. Bundle the sidecar into the app

The release-only Tauri overlay declares the frozen ONEDIR as a **resource** (a
directory, not a single `externalBin`, because the payload is a tree of dylibs):

```jsonc
// src-tauri/tauri.release.conf.json → "bundle"
"resources": { "sidecar-dist/lsdj_backend": "lsdj_backend" }
```

`just tauri-release` merges that overlay and enables the `bundled-backend` Cargo
feature. During Tauri setup the shell resolves
`resource_dir()/lsdj_backend/lsdj_backend`, fails startup if it is absent, and
uses the exact path for decks, model tooling, and the generation API. In dev,
point all three at the freeze directly instead of bundling:

```sh
LSDJ_BACKEND_BIN="$PWD/src-tauri/sidecar-dist/lsdj_backend/lsdj_backend" \
  just tauri-dev
```

Ordinary `just tauri-build`/`just tauri-dev` builds do not merge the release
overlay, so they remain lightweight source-tree builds and retain the existing
`LSDJ_SIDECAR_CMD` / `LSDJ_GENERATION_CMD` development overrides.

## 3. Codesign + notarize (Developer ID)

The bundle ships hardened-runtime entitlements
([`src-tauri/entitlements.plist`](../src-tauri/entitlements.plist): JIT for
WKWebView + MLX/LLVM, library validation disabled for the frozen backend's
dylibs). Its permanent bundle identifier is `works.protocol.lsdj`, and release
verification rejects an artifact signed by any Apple team other than Daniel
Peter's Developer ID team (`A293544336`). There are two deliberately different
build recipes:

- `just tauri-build` makes an explicitly **ad-hoc-signed developer build**. It is
  structurally valid (including on Apple Silicon), but Gatekeeper may require a
  manual Privacy & Security override on another Mac. Do not publish this DMG.
- `just tauri-release` is the distributable build. It refuses to run without a
  Developer ID Application identity and notarization credentials, lets Tauri
  drive `codesign` + `notarytool`, and then verifies both the app and the exact
  copy inside the DMG with `codesign`, `stapler`, `spctl`, and `hdiutil`.

Configure the release build with:

```sh
uv sync --project backend --managed-python --python 3.13
export APPLE_SIGNING_IDENTITY="Developer ID Application: … (TEAMID)"
export APPLE_ID="you@example.com"
export APPLE_PASSWORD="app-specific-password"   # or APPLE_API_KEY/_ISSUER
export APPLE_TEAM_ID="TEAMID"
just freeze-backend
just tauri-release                               # → verified .app + .dmg
```

For App Store Connect API authentication, set `APPLE_API_ISSUER`,
`APPLE_API_KEY`, and (when the key is not in a standard notarytool search
location) `APPLE_API_KEY_PATH` instead of the Apple ID trio.

Do not distribute a plain `cargo tauri build`/`just tauri-build` artifact. A
build with no configured identity otherwise leaves only the Mach-O linker's
ad-hoc signature on the executable: the DMG checksum can be perfectly valid
while the `.app` has no sealed-resource signature or notarization ticket.
Gatekeeper evaluates that quarantined app on the receiving Mac and may report
the misleading “app is damaged” error.

The release script re-signs every Mach-O in the PyInstaller tree with the
Developer ID before Tauri seals the outer app, applies hardened-runtime
entitlements to the backend executable, and verifies the team identity again in
both the built app and mounted DMG. First launch runs a one-time Gatekeeper scan
of the ~1.1 GB bundle (Spike B measured ~23 s cold, ~1 s thereafter);
notarization is what keeps that a one-time cost rather than a per-launch block.

The frozen backend must use uv-managed Python 3.13. The macOS runner also ships
a framework build of Python, but PyInstaller preserves that `Python.framework`
as a symlinked bundle while Tauri's resource copier expands the links. That
post-signing layout change makes notarization reject the framework. The freeze
script therefore requires uv's frameworkless python-build-standalone runtime
and verifies that the payload contains `libpython3.13.dylib` instead.

### GitHub Actions release

The release-tag workflow at
[`.github/workflows/macos-release.yml`](../.github/workflows/macos-release.yml)
runs only after a protected `v*` tag passes a secret-free validation job and an
Environment reviewer approves the exact tagged commit. The signing job uses
GitHub's Apple Silicon `macos-15` runner. Before materializing credentials it
creates the locked Python environment, freezes the backend, and smoke-tests both
frozen CLI modes. It then imports the Developer ID certificate
into a randomly-passworded temporary Keychain, writes the App Store Connect API
key to a mode-0600 temporary file, runs `just tauri-release`, uploads only the
verified DMG, and deletes the signing material even when the build fails. The
secret-bearing release step never executes the frozen application code. Every
referenced action is pinned to an immutable commit, and the tested Tauri CLI is
pinned to `2.11.2`; Dependabot keeps the action pins current through reviewed
pull requests.

Create a release from a clean, up-to-date `main` checkout with:

```sh
just release
```

Remote `vYYYY.MM.N` tags are the version ledger, so there is no separate “latest
version” file to maintain and no bump argument. The command uses the current UTC
year and month, fetches `origin/main` and all release tags, increments the
highest release number for that month, refuses a dirty, detached, non-`main`, or
stale checkout, and asks before creating and pushing an annotated tag. For
example, the first two August 2026 releases are `v2026.08.1` and `v2026.08.2`;
the counter resets to `1` in September.

The tag supplies the Tauri bundle version for that build, normalized to the
three-component Apple form (`v2026.08.1` becomes `2026.8.1`). After validation,
Engineering approval, signing, notarization, and verification, a separate job
with no Apple credentials publishes the single verified DMG as a GitHub Release
with generated notes. Its write-capable token is isolated from the signing job.
Repository release immutability is enabled, so the published tag and DMG cannot
later be moved, replaced, or deleted under the same version.

Create a GitHub Environment named **`macos-release`** under **Settings →
Environments** and configure all of these protections before adding secrets:

1. Set **Deployment branches and tags** to **Selected branches and tags** and
   allow tags matching `v*` only.
2. Add `@protocol-works/engineering` under **Required reviewers**.
3. Leave **Prevent self-review** disabled. The Engineering member who pushes the
   release tag may approve it, but the explicit Environment approval still makes
   secret access visible and intentional.
4. Disable administrator bypass for the Environment.
5. In the `main` branch ruleset, require pull requests and one Code Owner
   approval. Grant `@protocol-works/engineering` **pull-request-only** bypass:
   team members may merge their own PRs, but cannot push directly to `main`.
   [`.github/CODEOWNERS`](../.github/CODEOWNERS) assigns the release workflow,
   release scripts, Tauri config, and entitlements to that team, so non-team
   changes still require Engineering approval.
6. Under **Settings → Actions → General**, keep the workflow token at **Read
   repository contents permission** and do not enable sending secrets or
   write-capable tokens to workflows from forked pull requests.

Add these as **Environment secrets**, not repository secrets, so GitHub withholds
them until a required reviewer approves the `macos-release` job:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE_BASE64` | Base64 of the exported Developer ID Application `.p12` (certificate **and private key**) |
| `APPLE_CERTIFICATE_PASSWORD` | Strong export password protecting that `.p12` |
| `APPLE_API_KEY_BASE64` | Base64 of the App Store Connect `AuthKey_….p8` |
| `APPLE_API_KEY_ID` | App Store Connect API key ID |
| `APPLE_API_ISSUER` | App Store Connect API issuer UUID |

Create the base64 values locally without committing the source files or output:

```sh
openssl base64 -A -in DeveloperID.p12 | pbcopy
openssl base64 -A -in AuthKey_ABC123.p8 | pbcopy
```

The signing job derives `APPLE_SIGNING_IDENTITY` from the imported certificate,
so there is no identity variable to maintain. That secret-bearing job has only
`contents: read` permission, has no user-controlled inputs, refuses to run outside
`protocol-works/lsdj`, and is triggered only by a `v*` tag. The validation job
rejects malformed release tags and tags whose commit is not contained in
`main`. Repository rules restrict creation of matching tags to the Engineering
team and prevent an existing release tag from being updated or deleted.

No CI secret is impossible to extract: any approved workflow code that can use a
credential can deliberately transmit it. The security boundary is therefore the
protected Environment's explicit approval of the exact tagged `main` commit
before the release job receives its secrets. Self-approval removes two-person
separation, so never approve a release containing an unreviewed workflow, build
script, dependency/build-script change, or action-pin change. Prefer a
least-privilege App Store Connect API key, revoke/rotate both Apple credentials
after suspected exposure, and use GitHub-hosted ephemeral runners rather than a
persistent self-hosted Mac for this job.

## 4. First-run model install (the in-app model manager)

The weights live outside the bundle at `$MAGENTA_HOME/magenta-rt-v2` (default the
app-owned `~/Library/Application Support/LSDJ`; see [`CLAUDE.md`](../CLAUDE.md)).
There is no terminal install path — models install **in-app** from the settings
drawer (issue #43). The packaged app follows this flow on first run:

1. On launch, check whether `$MAGENTA_HOME/magenta-rt-v2/<model>` exists.
2. If absent, show the first-run download screen instead of the decks (the
   sidecars are not spawned until weights are present — a missing model is the
   existing graceful "sidecar spawn fails → silent deck" path, surfaced as UI).
3. Trigger the download via the frozen sidecar / `mrt` tooling and show progress.
4. On completion, spawn the sidecars and reveal the decks.

The check + the download orchestration reuse the `mrt models` CLI the backend
already wraps — no new inference code. Wiring this screen is tracked on the
checklist (it needs the live model tooling to verify).

**Realised by the in-app model manager (issue #43).** This first-run flow is now
the model manager (a settings-drawer panel), so the same machinery serves both a
fresh install and later top-ups:

- The packaged download runs the frozen backend in a non-deck mode —
  `lsdj_backend --init-resources` then `lsdj_backend --download-model <name>` — which
  reuses the `magenta_rt.cli.models_commands` code path and emits JSON progress
  the Rust shell relays to the UI. The init step is what makes a freshly
  downloaded model *loadable*: a model's two files (`<name>.mlxfn` +
  `<name>_state.safetensors`) are not enough without the shared
  `resources/musiccoca` + `resources/spectrostream`.
- That download path pulls `huggingface_hub` / `fsspec` / `click`, which the deck
  sidecar never imports, so they are collected explicitly in
  [`scripts/freeze-sidecar.sh`](../scripts/freeze-sidecar.sh) (`--collect-all
  huggingface_hub`, `--collect-all fsspec`, `--hidden-import click`). A missing
  collection only fails at runtime in the packaged app — hence the checklist
  item below, which static analysis cannot cover.
- Stable Audio 3 installs in-app too, into the app-owned assets dir. The Rust
  worker treats [`sa3-pin.json`](../sa3-pin.json) as its trust root: native HTTPS
  downloads the immutable source, `uv` runtime, and every model artifact;
  application-controlled SHA-256 values are checked before bounded native
  extraction or execution. Python dependencies come from the embedded
  hash-locked requirements file. Pin provenance and the reproducible release
  audit are recorded in [`docs/sa3-pin-audit.md`](sa3-pin-audit.md). The complete candidate is built and warmed in
  the host-resolved same-filesystem staging root, then atomically promoted while
  the previous verified install remains available for rollback. No in-app step
  invokes `bash`, `curl`, `tar`, `chmod`, shell activation, or a shell command
  string. Both families' weights move there with `just migrate-models`.
