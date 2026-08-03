# LSDJai task runner — `just` lists recipes, `just <recipe>` runs one.

default:
    @just --list

# One-time setup: backend deps, frontend deps + build. Model weights are NOT
# downloaded here — install them in-app from the settings drawer (the model
# manager, issue #43), which is the only install path. A dev's first
# `just tauri-dev` shows empty decks until a model is installed.
setup:
    # Production freezing must use uv's frameworkless python-build-standalone
    # distribution; otherwise Tauri's resource copy invalidates Python.framework.
    cd backend && uv sync --managed-python --python 3.13
    cd frontend && npm install
    cargo install tauri-cli
    just build

# Relocate existing model weights from the legacy ~/Documents/Magenta (or
# $MAGENTA_HOME) location — and a ~/Repos Stable Audio 3 clone — into the
# app-owned data dir (~/Library/Application Support/LSDJai), so model data is out
# of any iCloud-synced Documents folder. Same-volume moves are instant. The app
# also migrates the Magenta weights automatically on first launch; this is the
# manual equivalent and also covers Stable Audio 3. Idempotent — an item whose
# destination already exists is left alone.
migrate-models:
    #!/usr/bin/env bash
    set -euo pipefail
    old="${MAGENTA_HOME:-$HOME/Documents/Magenta}"
    new="$HOME/Library/Application Support/LSDJai"
    mkdir -p "$new"
    if [ -d "$old/magenta-rt-v2" ] && [ ! -e "$new/magenta-rt-v2" ]; then
      echo "moving magenta-rt-v2 → $new/"
      mv "$old/magenta-rt-v2" "$new/magenta-rt-v2"
    else
      echo "skip magenta-rt-v2 (source missing or destination exists)"
    fi
    if [ -e "$new/stable-audio-3" ]; then
      echo "skip stable-audio-3 (destination exists)"
    else
      moved=0
      for src in "$old/stable-audio-3" "$HOME/Repos/stable-audio-3"; do
        if [ -d "$src" ]; then
          echo "moving stable-audio-3 ($src) → $new/"
          mv "$src" "$new/stable-audio-3"
          moved=1
          break
        fi
      done
      [ "$moved" = 1 ] || echo "skip stable-audio-3 (no checkout found)"
    fi
    echo "done — model data now lives under $new"

# Build the frontend into frontend/dist (the Tauri webview loads it via
# tauri.conf's frontendDist; tauri-dev / tauri-build depend on this).
build:
    cd frontend && npm run build

# Native shell: run the full native app in dev — the Rust audio engine (cpal) +
# the per-deck Python inference sidecars + the sa3 generation server. The `build`
# dependency rebuilds frontend/dist first (the webview loads it via frontendDist);
# this must happen here, not in tauri.conf's beforeDevCommand, because Tauri runs
# that hook from the repo root and a fresh dist is required or the decks hang in
# 'Connecting'. Needs cargo-tauri (`cargo install tauri-cli@^2`) and the backend
# deps (`just setup`); install model weights in-app from the settings drawer. The
# default `uv run` sidecar/generation commands use the backend project dir;
# override each command in dev, or point both at a freeze with LSDJ_BACKEND_BIN.
tauri-dev: build
    cd src-tauri && cargo tauri dev

# Freeze the shared Python backend into an ONEDIR binary for bundling
# (src-tauri/sidecar-dist/lsdj_backend). It serves deck inference, model tooling,
# and the generation API from one dependency tree. The production form of Spike B; see
# docs/native-packaging.md. Needs `just setup` (backend .venv + pyinstaller).
freeze-backend:
    ./scripts/freeze-sidecar.sh

# Compatibility alias for the original packaging recipe name.
freeze-sidecar: freeze-backend

# Native shell developer bundle: build the app/DMG into
# src-tauri/target/release/bundle/. The config applies an explicit ad-hoc
# signature so the app bundle is structurally valid on Apple Silicon. This is
# useful for local testing only; use `just tauri-release` for anything sent to
# another Mac.
tauri-build: build
    cd src-tauri && cargo tauri build

# Distributable macOS build. Fails closed unless a Developer ID Application
# identity and Apple notarization credentials are configured, then verifies the
# app and the copy inside the DMG with codesign, stapler, spctl, and hdiutil.
tauri-release:
    ./scripts/build-macos-release.sh

# Create and push the next protected vYYYY.MM.N tag from a clean, current main.
# Remote calendar-version tags are the ledger; no version file or bump is needed.
# The tag starts the macOS signing workflow and its Engineering approval gate.
release:
    ./scripts/create-release.sh

# All tests: backend pytest + frontend vitest + the Rust engine/shell.
test:
    cd backend && uv run pytest
    cd frontend && npm run test
    cd src-tauri && cargo test --workspace

# Lint + format check + type-check, all three stacks. (No `cargo fmt --check`:
# the Rust follows a hand-style like the frontend, not rustfmt — clippy is the
# gate.)
lint:
    cd backend && uv run ruff format --check .
    cd backend && uv run ruff check .
    cd frontend && npm run lint
    cd frontend && npx tsc -b
    cd src-tauri && cargo clippy --workspace --all-targets -- -D warnings

# Apply formatting.
format:
    cd backend && uv run ruff format .

# Everything a PR must pass: lint + tests.
check: lint test
