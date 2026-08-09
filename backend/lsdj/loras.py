"""Stable Audio 3 LoRA adapter registry — the read side (issue #66, ADR-0028).

Adapters live under the host-resolved asset root, one directory per adapter,
organised by the DiT family they ride:

    $SA3_LORAS_HOME/<base>/<slug>/

``base`` is ``small`` (the 1024-wide sm-sfx / sm-music DiTs) or ``medium``
(the 1536-wide track DiT). An adapter directory holds its ``.safetensors``
(plus the sibling ``adapter_config.json`` when the PEFT convention applies)
and the importer's ``lora.json`` manifest. The Rust shell owns the lifecycle
(import / validate / delete, mirroring the model manager, issue #43); this
module only reads the registry: the generate path resolves a client-supplied
adapter name to the directory handed to ``sa3_mlx.py`` as ``--lora``.
"""

import os
import pathlib
import re
import hashlib
import json
import stat

from . import runtime_paths

# The two DiT families an adapter can ride, and which generation kind uses
# which. sm-sfx and sm-music share one architecture, so a "small" adapter
# applies to both kinds; the medium DiT is the track engine (sa3.KINDS).
BASES = ("small", "medium")
KIND_BASES = {"sfx": "small", "music": "small", "track": "medium"}

# Trust-boundary bounds for the `--lora-strength` knob (mirrored by
# `controller.generate_audio`). 0 is the bit-exact bypass (ADR-0028); the
# spike measured 2.0 as already strong, so 4 is a guard rail, not a UX limit.
MIN_LORA_STRENGTH = 0.0
MAX_LORA_STRENGTH = 4.0

# Adapters per generation. The merge stacks linearly (ADR-0028), but an
# unbounded list is unbounded argv and load time — and past a few adapters
# the summed deltas swamp the base anyway. Mirrored by the LoraRack UI.
MAX_LORA_STACK = 4

# One path segment of an adapter name: no separators, no leading dot — the
# name a client sends can only ever address a directory INSIDE the registry.
_SLUG = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


class UnknownAdapter(Exception):
    """An adapter name that does not resolve to an installed adapter."""


def loras_dir(
    env: dict | None = None, home: pathlib.Path | None = None
) -> pathlib.Path:
    """The registry root explicitly supplied by the Rust host."""
    env = os.environ if env is None else env
    del home  # retained for API compatibility; platform paths come from Rust.
    root = runtime_paths.loras_home(env)
    if root is None:
        raise RuntimeError("LSDJ asset roots were not supplied by the desktop host")
    return root


def _is_linklike(path: pathlib.Path) -> bool:
    try:
        return path.is_symlink() or (
            hasattr(os.path, "isjunction") and os.path.isjunction(path)
        )
    except OSError:
        return True


def _contained(path: pathlib.Path, root: pathlib.Path, *, directory: bool) -> bool:
    try:
        if _is_linklike(path):
            return False
        mode = path.lstat().st_mode
        if directory and not stat.S_ISDIR(mode):
            return False
        if not directory and not stat.S_ISREG(mode):
            return False
        path.resolve(strict=True).relative_to(root.resolve(strict=True))
        return True
    except (OSError, ValueError):
        return False


def _verified_manifest(adapter_dir: pathlib.Path, root: pathlib.Path) -> bool:
    manifest_path = adapter_dir / "lora.json"
    if not manifest_path.exists():
        return True  # Preserve explicitly supported hand-placed adapters.
    if not _contained(manifest_path, root, directory=False):
        return False
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        files = manifest.get("files", [])
        if not files:
            return True  # Legacy imports predate the artifact inventory.
        for record in files:
            filename = record["filename"]
            if not _SLUG.fullmatch(filename):
                return False
            artifact = adapter_dir / filename
            if not _contained(artifact, root, directory=False):
                return False
            data = artifact.read_bytes()
            if len(data) != record["size"]:
                return False
            if hashlib.sha256(data).hexdigest() != record["sha256"].lower():
                return False
        return True
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError):
        return False


def _adapter_file(adapter_dir: pathlib.Path, root: pathlib.Path) -> pathlib.Path | None:
    """The adapter's .safetensors inside its directory, or None. The importer
    writes exactly one; tolerate a hand-placed dir the same way the runtime's
    `_resolve_path` does (one .safetensors, any name)."""
    if not _contained(adapter_dir, root, directory=True):
        return None
    hits = sorted(
        entry
        for entry in adapter_dir.iterdir()
        if _contained(entry, root, directory=False) and entry.suffix == ".safetensors"
    )
    if len(hits) != 1 or not _verified_manifest(adapter_dir, root):
        return None
    return hits[0]


def resolve(
    name: str, env: dict | None = None, home: pathlib.Path | None = None
) -> tuple[pathlib.Path, str]:
    """Resolve a client-supplied adapter name (``<base>/<slug>``) to its
    directory. Raises UnknownAdapter for anything that is not a well-formed
    name of an installed adapter — malformed names never touch the
    filesystem, so a name cannot escape the registry root."""
    base, _, slug = name.partition("/")
    if base not in BASES or not _SLUG.match(slug):
        raise UnknownAdapter(f"unknown adapter {name!r}")
    root = loras_dir(env, home)
    if not _contained(root, root, directory=True):
        raise UnknownAdapter(f"unknown adapter {name!r}")
    base_dir = root / base
    adapter_dir = base_dir / slug
    if (
        not _contained(base_dir, root, directory=True)
        or _adapter_file(adapter_dir, root) is None
    ):
        raise UnknownAdapter(f"unknown adapter {name!r}")
    return adapter_dir, base
