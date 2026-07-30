"""Stable Audio 3 LoRA adapter registry — the read side (issue #66, ADR-0028).

Adapters live on disk under the app-owned data dir, one directory per
adapter, organised by the DiT family they ride:

    ~/Library/Application Support/LSDJai/sa3-loras/<base>/<slug>/

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
    """The registry root. $SA3_LORAS_HOME wins (tests, dev overrides);
    otherwise the app-owned data dir, beside the SA3 checkout. Mirrors the
    Rust `loras::loras_dir`."""
    env = os.environ if env is None else env
    home = pathlib.Path.home() if home is None else home
    override = env.get("SA3_LORAS_HOME", "")
    if override:
        return pathlib.Path(override).expanduser()
    return home / "Library" / "Application Support" / "LSDJai" / "sa3-loras"


def _adapter_file(adapter_dir: pathlib.Path) -> pathlib.Path | None:
    """The adapter's .safetensors inside its directory, or None. The importer
    writes exactly one; tolerate a hand-placed dir the same way the runtime's
    `_resolve_path` does (one .safetensors, any name)."""
    if not adapter_dir.is_dir():
        return None
    hits = sorted(
        entry
        for entry in adapter_dir.iterdir()
        if entry.is_file() and entry.suffix == ".safetensors"
    )
    return hits[0] if len(hits) == 1 else None


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
    adapter_dir = loras_dir(env, home) / base / slug
    if _adapter_file(adapter_dir) is None:
        raise UnknownAdapter(f"unknown adapter {name!r}")
    return adapter_dir, base
