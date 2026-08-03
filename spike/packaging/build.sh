#!/usr/bin/env bash
# Spike B: freeze the LSDJ MLX inference path into a launchable sidecar.
#
# ONEDIR build (payload is hundreds of MB — onefile would be unworkable).
# Iterate by adding --collect-all / --copy-metadata / --hidden-import only as
# real import/load errors demand. Run ./build.sh then ./run.sh.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
VENV="$REPO/backend/.venv"
SP="$VENV/lib/python3.13/site-packages"
PYI="$VENV/bin/pyinstaller"

# The backend source (lsdj.engine) — imported, never copied/modified.
BACKEND="$REPO/backend"
# sequence_layers is vendored under magenta_rt with a hyphen dir and injected
# onto sys.path at runtime; point PyInstaller's analysis at it directly so the
# bare `import sequence_layers` resolves at build time too.
SEQLAYERS_DIR="$SP/magenta_rt/_vendor/sequence-layers"

cd "$HERE"
rm -rf build dist freeze_test.spec

"$PYI" \
  --noconfirm \
  --onedir \
  --name lsdj_infer \
  --console \
  --paths "$BACKEND" \
  --paths "$SEQLAYERS_DIR" \
  --hidden-import lsdj.engine \
  --collect-submodules lsdj \
  --collect-all mlx \
  --collect-all mlx_metal \
  --collect-submodules magenta_rt \
  --collect-submodules sequence_layers \
  --collect-submodules ai_edge_litert \
  --collect-binaries ai_edge_litert \
  --copy-metadata magenta_rt \
  freeze_test.py

# ---- THE WALL: mlx.metallib must sit next to the resolved dylib ----
# MLX's get_colocated_mtllib_path looks for mlx.metallib next to libmlx.dylib
# (which lives at _internal/mlx/lib/ after --collect-all). --collect-all already
# places it there, but PyInstaller can also resolve the lib next to the exe via
# @rpath; copy the metallib + dylibs alongside the binary as a belt-and-braces
# fix so resolution succeeds regardless of which path MLX takes.
DIST_BIN_DIR="$HERE/dist/lsdj_infer"
MLX_LIB="$SP/mlx/lib"
for f in mlx.metallib libmlx.dylib libjaccl.dylib; do
  if [ -f "$MLX_LIB/$f" ] && [ ! -f "$DIST_BIN_DIR/$f" ]; then
    cp "$MLX_LIB/$f" "$DIST_BIN_DIR/$f"
  fi
done

echo "=== build complete ==="
du -sh "$DIST_BIN_DIR"
