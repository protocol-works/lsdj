#!/usr/bin/env bash
# Build and verify LSDJ's Ubuntu 22.04-compatible x86_64 AppImage. Model
# runtimes/weights are external, verified, and app-managed; this shell exists
# only on the release builder and is not part of the installed runtime.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LINUX_CONFIG="$REPO_ROOT/src-tauri/tauri.linux.conf.json"
RELEASE_VERSION="${LSDJ_RELEASE_VERSION:-}"
AUDIT_PATH="${LSDJ_LINUX_AUDIT_PATH:-$REPO_ROOT/src-tauri/target/release/bundle/appimage/linux-package-audit.json}"

fail() {
  echo "Linux release: $*" >&2
  exit 1
}

[ "$(uname -s)" = "Linux" ] || fail "must be built on Linux"
[ "$(uname -m)" = "x86_64" ] || fail "must be built for x86_64"

GLIBC_DESCRIPTION="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
[[ "$GLIBC_DESCRIPTION" =~ ^glibc\ ([0-9]+)\.([0-9]+)$ ]] || fail \
  "an auditable glibc build host is required"
GLIBC_MAJOR="${BASH_REMATCH[1]}"
GLIBC_MINOR="${BASH_REMATCH[2]}"
if (( GLIBC_MAJOR > 2 || (GLIBC_MAJOR == 2 && GLIBC_MINOR > 35) )); then
  fail "build host glibc $GLIBC_MAJOR.$GLIBC_MINOR is newer than Ubuntu 22.04's 2.35 floor"
fi

VERSION_ARGS=()
if [ -n "$RELEASE_VERSION" ]; then
  [[ "$RELEASE_VERSION" =~ ^v?([0-9]{4})\.(0[1-9]|1[0-2])\.([1-9][0-9]*)$ ]] || fail \
    "LSDJ_RELEASE_VERSION must look like vYYYY.MM.N with a positive release number"
  RELEASE_YEAR=$((10#${BASH_REMATCH[1]}))
  RELEASE_MONTH=$((10#${BASH_REMATCH[2]}))
  RELEASE_NUMBER=$((10#${BASH_REMATCH[3]}))
  RELEASE_VERSION="${RELEASE_YEAR}.${RELEASE_MONTH}.${RELEASE_NUMBER}"
  VERSION_ARGS=(--config "{\"version\":\"$RELEASE_VERSION\"}")
fi

echo "Linux release: building frontend"
npm run build --prefix "$REPO_ROOT/frontend"

echo "Linux release: building AppImage on glibc $GLIBC_MAJOR.$GLIBC_MINOR"
(
  cd "$REPO_ROOT/src-tauri"
  cargo tauri build --ci \
    --features managed-runtime \
    --config "$LINUX_CONFIG" \
    "${VERSION_ARGS[@]}"
)

shopt -s nullglob
APPIMAGES=("$REPO_ROOT"/src-tauri/target/release/bundle/appimage/*.AppImage)
[[ "${#APPIMAGES[@]}" -eq 1 ]] || fail \
  "expected exactly one AppImage, found ${#APPIMAGES[@]}"

python3 "$REPO_ROOT/scripts/verify_linux_appimage.py" \
  "${APPIMAGES[0]}" \
  --output "$AUDIT_PATH"

echo "Linux release: verified ${APPIMAGES[0]}"
echo "Linux release: native dependency audit $AUDIT_PATH"
