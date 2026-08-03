#!/usr/bin/env bash
# Freeze the complete LSDJ Python backend into one launchable ONEDIR runtime
# for bundling into the native app (Phase 2 part 6, ADR-0018/0019).
#
# This is the production form of the proven Spike B recipe (spike/packaging/
# build.sh, docs/spike-packaging.md) — the ONLY change is the entry point:
# backend/lsdj/frozen.py dispatches to the loopback-TCP sidecar/model tooling or
# the FastAPI generation server. The dependency closure (mlx, magenta_rt,
# sequence_layers, …) is shared. The current ONEDIR is ~1.1 GB after colocating
# the metallib at both loader-visible libmlx paths; weights remain external.
#
# Output: src-tauri/sidecar-dist/lsdj_backend/lsdj_backend (+ _internal/).
# Spawned by Rust for decks, model management, and (with
# `--generation-server`) the loopback generation API.
#
# Usage: scripts/freeze-sidecar.sh   (needs `just setup` — backend .venv with
# pyinstaller + the inference deps).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
VENV="$REPO/backend/.venv"
PYTHON="$VENV/bin/python"
PYTHON_MINOR="3.13"
SP="$VENV/lib/python$PYTHON_MINOR/site-packages"
PYI="$VENV/bin/pyinstaller"
BACKEND="$REPO/backend"
# sequence_layers is vendored under magenta_rt with a hyphen dir and injected
# onto sys.path at runtime; point PyInstaller's analysis at it directly.
SEQLAYERS_DIR="$SP/magenta_rt/_vendor/sequence-layers"
OUT="$REPO/src-tauri/sidecar-dist"

fail() {
  echo "backend freeze: $*" >&2
  exit 1
}

[ -x "$PYTHON" ] || fail \
  "backend environment is missing; run 'just setup' first"
[ -x "$PYI" ] || fail \
  "PyInstaller is missing from backend/.venv; run 'just setup' first"

ACTUAL_PYTHON_MINOR="$("$PYTHON" -c \
  'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
[ "$ACTUAL_PYTHON_MINOR" = "$PYTHON_MINOR" ] || fail \
  "Python $PYTHON_MINOR is required; backend/.venv uses $ACTUAL_PYTHON_MINOR"

# PyInstaller preserves a python.org-style Python.framework as a symlinked
# bundle. Tauri copies release resources as ordinary files, which changes that
# framework after it was signed and makes Apple's notarizer reject the alias
# binaries. uv's managed python-build-standalone distribution instead supplies
# a flat libpython dylib, which survives the resource copy unchanged.
PYTHON_FRAMEWORK="$("$PYTHON" -c \
  'import sysconfig; print(sysconfig.get_config_var("PYTHONFRAMEWORK") or "")')"
[ -z "$PYTHON_FRAMEWORK" ] || fail \
  "framework Python '$PYTHON_FRAMEWORK' cannot be packaged; recreate backend/.venv with uv-managed Python $PYTHON_MINOR"

# Starlette reaches python_multipart (the /api/generate multipart init-audio
# path, issue #54) through a guarded `try: import python_multipart` that
# PyInstaller's static analysis can miss; collect it explicitly so the frozen
# generation server can parse uploads instead of failing only on that path.
DIST_BIN_DIR="$OUT/lsdj_backend"
rm -rf "$DIST_BIN_DIR"
mkdir -p "$OUT"
"$PYI" \
  --noconfirm \
  --onedir \
  --name lsdj_backend \
  --console \
  --distpath "$OUT" \
  --paths "$BACKEND" \
  --paths "$SEQLAYERS_DIR" \
  --hidden-import lsdj.engine \
  --hidden-import lsdj.worker \
  --hidden-import lsdj.sidecar \
  --hidden-import lsdj.controller \
  --collect-submodules lsdj \
  --collect-submodules python_multipart \
  --collect-all mlx \
  --collect-all mlx_metal \
  --collect-submodules magenta_rt \
  --collect-submodules sequence_layers \
  --collect-submodules ai_edge_litert \
  --collect-binaries ai_edge_litert \
  --copy-metadata magenta_rt \
  --collect-all huggingface_hub \
  --collect-all fsspec \
  --hidden-import click \
  --copy-metadata huggingface_hub \
  --copy-metadata fsspec \
  "$BACKEND/lsdj/frozen.py"

INTERNAL="$DIST_BIN_DIR/_internal"
[ -f "$INTERNAL/libpython$PYTHON_MINOR.dylib" ] || fail \
  "frozen runtime has no frameworkless libpython$PYTHON_MINOR.dylib"
if [ -e "$INTERNAL/Python.framework" ] || [ -L "$INTERNAL/Python.framework" ] || \
   [ -e "$INTERNAL/Python" ] || [ -L "$INTERNAL/Python" ]; then
  fail "frozen runtime unexpectedly contains a symlinked Python.framework"
fi

# THE METALLIB WALL (Spike B): MLX's get_colocated_mtllib_path looks for
# mlx.metallib next to the path used to load libmlx.dylib. PyInstaller exposes
# that library both beside the executable and as `_internal/libmlx.dylib` (a
# symlink into mlx/lib), so colocate the payload in both places. Tauri's resource
# copier dereferences that symlink; the explicit _internal copy keeps the signed
# app working after bundling too.
MLX_LIB="$SP/mlx/lib"
for destination in "$DIST_BIN_DIR" "$INTERNAL"; do
  for f in mlx.metallib libmlx.dylib libjaccl.dylib; do
    if [ -f "$MLX_LIB/$f" ] && [ ! -f "$destination/$f" ]; then
      cp "$MLX_LIB/$f" "$destination/$f"
    fi
  done
done

echo "=== sidecar freeze complete ==="
du -sh "$DIST_BIN_DIR"
echo "Release bundle source: $DIST_BIN_DIR"
echo "For a dev run, set LSDJ_BACKEND_BIN=$DIST_BIN_DIR/lsdj_backend."
