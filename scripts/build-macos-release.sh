#!/usr/bin/env bash
# Build a Developer ID-signed, notarized, stapled macOS app + DMG and fail if
# any artifact would be rejected by Gatekeeper. This is deliberately separate
# from the ad-hoc-signed developer build produced by `just tauri-build`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
APP_PATH="$REPO_ROOT/src-tauri/target/release/bundle/macos/LSDJ.app"
DMG_DIR="$REPO_ROOT/src-tauri/target/release/bundle/dmg"
BACKEND_DIR="$REPO_ROOT/src-tauri/sidecar-dist/lsdj_backend"
BACKEND_BIN="$BACKEND_DIR/lsdj_backend"
RELEASE_CONFIG="$REPO_ROOT/src-tauri/tauri.release.conf.json"
ENTITLEMENTS="$REPO_ROOT/src-tauri/entitlements.plist"
RELEASE_VERSION="${LSDJ_RELEASE_VERSION:-}"
EXPECTED_BUNDLE_ID="works.protocol.lsdj"
EXPECTED_TEAM_ID="A293544336"
DEVELOPER_ID_REQUIREMENT="=anchor apple generic
  and certificate 1[field.1.2.840.113635.100.6.2.6] exists
  and certificate leaf[field.1.2.840.113635.100.6.1.13] exists
  and certificate leaf[subject.OU] = \"$EXPECTED_TEAM_ID\""

fail() {
  echo "macOS release: $*" >&2
  exit 1
}

fail_signature() {
  local path="$1"
  echo "macOS release: signature details for $path" >&2
  codesign -dvvv "$path" >&2 || true
  fail "$2: $path"
}

# Test the signed identity directly instead of parsing codesign's display text.
# Quiet consumers in a pipeline can cause SIGPIPE failures under pipefail.
verify_developer_id_signature() {
  local path="$1"
  shift
  if ! codesign --verify --strict --verbose=2 \
    --test-requirement "$DEVELOPER_ID_REQUIREMENT" "$@" "$path"; then
    fail_signature "$path" \
      "expected a valid Developer ID Application signature from Apple team $EXPECTED_TEAM_ID"
  fi
}

is_macho() {
  local description
  description="$(/usr/bin/file -b "$1")" || fail "could not inspect binary: $1"
  [[ "$description" == *Mach-O* ]]
}

verify_backend_layout() {
  local backend_bin="$1"
  [ -x "$backend_bin" ] || fail "bundled backend is missing or not executable: $backend_bin"
  [ -d "$(dirname "$backend_bin")/_internal" ] || fail \
    "bundled backend has no PyInstaller _internal directory"
  [ -f "$(dirname "$backend_bin")/_internal/mlx.metallib" ] || fail \
    "bundled backend has no metallib beside its internal MLX library"
}

# Tauri signs declared external binaries, but resources are copied as data. The
# PyInstaller ONEDIR tree lives in Resources so sign every Mach-O inside-out
# before Tauri seals and notarizes the outer app.
sign_backend_tree() {
  local backend_dir="$1"
  local candidate
  while IFS= read -r -d '' candidate; do
    [ "$candidate" = "$BACKEND_BIN" ] && continue
    if is_macho "$candidate"; then
      codesign --force --timestamp --sign "$SIGNING_IDENTITY" "$candidate"
    fi
  done < <(find "$backend_dir" -type f -print0)

  codesign --force \
    --options runtime \
    --timestamp \
    --entitlements "$ENTITLEMENTS" \
    --sign "$SIGNING_IDENTITY" \
    "$BACKEND_BIN"
}

verify_signed_backend_tree() {
  local backend_dir="$1"
  local candidate signature
  local macho_count=0
  while IFS= read -r -d '' candidate; do
    if is_macho "$candidate"; then
      macho_count=$((macho_count + 1))
      verify_developer_id_signature "$candidate"
      if [ "$candidate" = "$backend_dir/lsdj_backend" ]; then
        signature="$(codesign -dvvv "$candidate" 2>&1)" || fail_signature \
          "$candidate" "could not inspect backend executable signature"
        grep -q 'flags=.*runtime' <<< "$signature" || fail_signature \
          "$candidate" "backend executable is not signed with hardened runtime"
      fi
    fi
  done < <(find "$backend_dir" -type f -print0)
  [ "$macho_count" -gt 1 ] || fail "backend payload has no dependency binaries"
}

verify_app_backend() {
  local app_path="$1"
  local backend_dir="$app_path/Contents/Resources/lsdj_backend"
  verify_backend_layout "$backend_dir/lsdj_backend"
  verify_signed_backend_tree "$backend_dir"
}

[ "$(uname -s)" = "Darwin" ] || fail "must be built on macOS"

if [ -n "$RELEASE_VERSION" ]; then
  [[ "$RELEASE_VERSION" =~ ^v?([0-9]{4})\.(0[1-9]|1[0-2])\.([1-9][0-9]*)$ ]] || fail \
    "LSDJ_RELEASE_VERSION must look like vYYYY.MM.N with a positive release number"
  RELEASE_YEAR=$((10#${BASH_REMATCH[1]}))
  RELEASE_MONTH=$((10#${BASH_REMATCH[2]}))
  RELEASE_NUMBER=$((10#${BASH_REMATCH[3]}))
  RELEASE_VERSION="${RELEASE_YEAR}.${RELEASE_MONTH}.${RELEASE_NUMBER}"
fi

SIGNING_IDENTITY="${APPLE_SIGNING_IDENTITY:-}"
[ -n "$SIGNING_IDENTITY" ] || fail \
  "APPLE_SIGNING_IDENTITY must name a Developer ID Application certificate"
[ "$SIGNING_IDENTITY" != "-" ] || fail \
  "ad-hoc signing is for local builds only; a distributable DMG needs Developer ID"

IDENTITIES="$(security find-identity -v -p codesigning || true)"
IDENTITY_LINE=""
while IFS= read -r IDENTITY_CANDIDATE; do
  if [[ "$IDENTITY_CANDIDATE" == *"$SIGNING_IDENTITY"* ]]; then
    IDENTITY_LINE="$IDENTITY_CANDIDATE"
    break
  fi
done <<< "$IDENTITIES"
[ -n "$IDENTITY_LINE" ] || fail \
  "APPLE_SIGNING_IDENTITY is not a valid code-signing identity in the keychain"
case "$IDENTITY_LINE" in
  *'"Developer ID Application:'*) ;;
  *) fail "the signing identity must be a Developer ID Application certificate" ;;
esac

APPLE_ID_AUTH=0
if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  APPLE_ID_AUTH=1
fi

API_KEY_AUTH=0
if [ -n "${APPLE_API_ISSUER:-}" ] && [ -n "${APPLE_API_KEY:-}" ]; then
  API_KEY_AUTH=1
  if [ -n "${APPLE_API_KEY_PATH:-}" ] && [ ! -f "$APPLE_API_KEY_PATH" ]; then
    fail "APPLE_API_KEY_PATH does not point to a readable App Store Connect key"
  fi
fi

[ "$APPLE_ID_AUTH" = 1 ] || [ "$API_KEY_AUTH" = 1 ] || fail \
  "set APPLE_ID + APPLE_PASSWORD + APPLE_TEAM_ID, or APPLE_API_ISSUER + APPLE_API_KEY (+ APPLE_API_KEY_PATH), for notarization"

verify_backend_layout "$BACKEND_BIN"

echo "macOS release: signing frozen backend runtime"
sign_backend_tree "$BACKEND_DIR"
verify_signed_backend_tree "$BACKEND_DIR"

echo "macOS release: building frontend"
just --justfile "$REPO_ROOT/justfile" --working-directory "$REPO_ROOT" build

echo "macOS release: building, signing, notarizing, and stapling"
(
  cd "$REPO_ROOT/src-tauri"
  if [ -n "$RELEASE_VERSION" ]; then
    cargo tauri build --ci \
      --features bundled-backend \
      --config "$RELEASE_CONFIG" \
      --config "{\"version\":\"$RELEASE_VERSION\"}"
  else
    cargo tauri build --ci \
      --features bundled-backend \
      --config "$RELEASE_CONFIG"
  fi
)

[ -d "$APP_PATH" ] || fail "expected app was not produced at $APP_PATH"
BUNDLE_ID="$(/usr/libexec/PlistBuddy \
  -c 'Print :CFBundleIdentifier' \
  "$APP_PATH/Contents/Info.plist")"
[ "$BUNDLE_ID" = "$EXPECTED_BUNDLE_ID" ] || fail \
  "app bundle identifier is $BUNDLE_ID; expected $EXPECTED_BUNDLE_ID"
if [ -n "$RELEASE_VERSION" ]; then
  BUNDLED_VERSION="$(/usr/libexec/PlistBuddy \
    -c 'Print :CFBundleShortVersionString' \
    "$APP_PATH/Contents/Info.plist")"
  [ "$BUNDLED_VERSION" = "$RELEASE_VERSION" ] || fail \
    "app version is $BUNDLED_VERSION; expected $RELEASE_VERSION from the release tag"
  BUNDLE_BUILD_VERSION="$(/usr/libexec/PlistBuddy \
    -c 'Print :CFBundleVersion' \
    "$APP_PATH/Contents/Info.plist")"
  [ "$BUNDLE_BUILD_VERSION" = "$RELEASE_VERSION" ] || fail \
    "app build version is $BUNDLE_BUILD_VERSION; expected $RELEASE_VERSION"
fi
DMG_PATH="$(find "$DMG_DIR" -maxdepth 1 -type f -name 'LSDJ_*.dmg' -print | sort | tail -1)"
[ -n "$DMG_PATH" ] || fail "no LSDJ DMG was produced in $DMG_DIR"

echo "macOS release: verifying app signature and notarization ticket"
verify_app_backend "$APP_PATH"
verify_developer_id_signature "$APP_PATH" --deep
xcrun stapler validate "$APP_PATH"
spctl --assess --type execute --verbose=4 "$APP_PATH"

echo "macOS release: verifying DMG checksum and the exact app copied into it"
hdiutil verify "$DMG_PATH"
ATTACH_OUTPUT="$(hdiutil attach -readonly -nobrowse "$DMG_PATH")"
MOUNT_POINT="$(printf '%s\n' "$ATTACH_OUTPUT" | awk -F '\t' '$0 ~ /Apple_HFS/ { print $NF }' | tail -1)"
[ -n "$MOUNT_POINT" ] || fail "could not find the mounted DMG volume"
cleanup() {
  hdiutil detach "$MOUNT_POINT" >/dev/null
}
trap cleanup EXIT

DMG_APP="$MOUNT_POINT/LSDJ.app"
[ -d "$DMG_APP" ] || fail "mounted DMG does not contain LSDJ.app"
verify_app_backend "$DMG_APP"
verify_developer_id_signature "$DMG_APP" --deep
xcrun stapler validate "$DMG_APP"
spctl --assess --type execute --verbose=4 "$DMG_APP"

cleanup
trap - EXIT

echo "macOS release: verified distributable DMG at $DMG_PATH"
