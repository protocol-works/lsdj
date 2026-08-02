#!/usr/bin/env bash
# Build a Developer ID-signed, notarized, stapled macOS app + DMG and fail if
# any artifact would be rejected by Gatekeeper. This is deliberately separate
# from the ad-hoc-signed developer build produced by `just tauri-build`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
APP_PATH="$REPO_ROOT/src-tauri/target/release/bundle/macos/LSDJai.app"
DMG_DIR="$REPO_ROOT/src-tauri/target/release/bundle/dmg"
RELEASE_VERSION="${LSDJ_RELEASE_VERSION:-}"
EXPECTED_BUNDLE_ID="works.protocol.lsdj"
EXPECTED_TEAM_ID="A293544336"

fail() {
  echo "macOS release: $*" >&2
  exit 1
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

IDENTITY_LINE="$({ security find-identity -v -p codesigning || true; } | grep -F "$SIGNING_IDENTITY" | head -1)"
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

echo "macOS release: building frontend"
just --justfile "$REPO_ROOT/justfile" --working-directory "$REPO_ROOT" build

echo "macOS release: building, signing, notarizing, and stapling"
(
  cd "$REPO_ROOT/src-tauri"
  if [ -n "$RELEASE_VERSION" ]; then
    cargo tauri build --ci --config "{\"version\":\"$RELEASE_VERSION\"}"
  else
    cargo tauri build --ci
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
DMG_PATH="$(find "$DMG_DIR" -maxdepth 1 -type f -name 'LSDJai_*.dmg' -print | sort | tail -1)"
[ -n "$DMG_PATH" ] || fail "no LSDJai DMG was produced in $DMG_DIR"

echo "macOS release: verifying app signature and notarization ticket"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"
APP_SIGNATURE="$(codesign -dvvv "$APP_PATH" 2>&1)"
printf '%s\n' "$APP_SIGNATURE" | grep -q '^Authority=Developer ID Application:' || fail \
  "app is not signed with Developer ID Application"
printf '%s\n' "$APP_SIGNATURE" | grep -q "^TeamIdentifier=$EXPECTED_TEAM_ID$" || fail \
  "app is not signed by Apple team $EXPECTED_TEAM_ID"
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

DMG_APP="$MOUNT_POINT/LSDJai.app"
[ -d "$DMG_APP" ] || fail "mounted DMG does not contain LSDJai.app"
codesign --verify --deep --strict --verbose=2 "$DMG_APP"
xcrun stapler validate "$DMG_APP"
spctl --assess --type execute --verbose=4 "$DMG_APP"

cleanup
trap - EXIT

echo "macOS release: verified distributable DMG at $DMG_PATH"
