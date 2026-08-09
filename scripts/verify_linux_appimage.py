#!/usr/bin/env python3
"""Verify an LSDJ x86_64 AppImage and emit deterministic package audit data."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path


class AppImageError(RuntimeError):
    """The produced AppImage violated the Linux package contract."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AppImageError(message)


def safe_packaged_file(root: Path, path: Path, kind: str) -> Path:
    """Resolve a regular file without allowing a package-root escape."""
    root = root.resolve(strict=True)
    if path.is_symlink():
        target = path.readlink()
        require(not target.is_absolute(), f"unsafe {kind}: absolute symlink")
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(root)
    except (OSError, ValueError):
        raise AppImageError(f"unsafe {kind}: {path}") from None
    require(resolved.is_file(), f"unsafe {kind}: {path}")
    return resolved


def desktop_entries(root: Path, path: Path) -> dict[str, str]:
    path = safe_packaged_file(root, path, "desktop entry")
    entries: dict[str, str] = {}
    section = ""
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            continue
        if section != "Desktop Entry" or "=" not in raw_line:
            continue
        # Preserve the raw value. In particular, do not normalize quotes,
        # escapes, or surrounding whitespace in the security-sensitive Exec
        # field before the canonical policy below sees it.
        key, value = raw_line.split("=", 1)
        if key in entries:
            raise AppImageError(f"duplicate desktop key: {key}")
        entries[key] = value
    return entries


def desktop_exec(value: str) -> str:
    """Require the exact launcher emitted by this Tauri bundle configuration."""
    require(
        value == "lsdj-app",
        "desktop Exec must be the canonical value lsdj-app",
    )
    return value


def needed_libraries(binary: Path) -> list[str]:
    readelf = shutil.which("readelf")
    require(readelf is not None, "readelf is required for the build-time ELF audit")
    result = subprocess.run(
        [readelf, "--dynamic", str(binary)],
        check=True,
        capture_output=True,
        text=True,
    )
    needed = sorted(set(re.findall(r"\(NEEDED\).*?\[(.+?)\]", result.stdout)))
    require(needed, "packaged executable has no ELF NEEDED entries")
    lowered = {library.casefold() for library in needed}
    forbidden = {
        library
        for library in lowered
        if "python" in library
        or library.startswith("libcuda.")
        or library.startswith("libcudart.")
    }
    require(
        not forbidden,
        "AppImage shell must not link system Python/CUDA libraries: "
        + ", ".join(sorted(forbidden)),
    )
    return needed


def verify_extracted(root: Path, libraries: list[str]) -> dict:
    require(root.is_dir() and not root.is_symlink(), "missing extracted AppImage root")
    app_run = safe_packaged_file(root, root / "AppRun", "AppRun entry point")
    if os.name != "nt":
        require(
            app_run.stat().st_mode & stat.S_IXUSR != 0,
            "AppRun entry point is not executable",
        )

    desktop_files = sorted(root.glob("*.desktop"))
    require(len(desktop_files) == 1, "AppImage must contain exactly one desktop entry")
    desktop = desktop_entries(root, desktop_files[0])
    require(desktop.get("Type") == "Application", "desktop Type must be Application")
    require(desktop.get("Name") == "LSDJ", "desktop Name must be LSDJ")
    desktop_exec(desktop.get("Exec", ""))
    categories = {item for item in desktop.get("Categories", "").split(";") if item}
    require(
        bool(categories & {"Audio", "AudioVideo", "Music"}),
        "desktop entry must advertise an audio/music category",
    )

    binary = root / "usr/bin/lsdj-app"
    require(
        binary.is_file() and not binary.is_symlink(),
        "AppImage has no safe lsdj-app binary",
    )
    # Windows does not preserve POSIX mode bits in the shared pure-layout unit
    # test. Production verification runs on Linux, where this remains required.
    if os.name != "nt":
        require(
            binary.stat().st_mode & stat.S_IXUSR != 0,
            "packaged lsdj-app binary is not executable",
        )
    require(any(root.glob("*.png")), "AppImage root has no desktop icon")

    return {
        "architecture": "x86_64",
        "desktop": {
            "categories": sorted(categories),
            "exec": desktop["Exec"],
            "file": desktop_files[0].name,
            "icon": desktop.get("Icon"),
            "name": desktop["Name"],
        },
        "elfNeeded": sorted(libraries),
        "platform": "linux",
        "schemaVersion": 1,
    }


def verify_appimage(appimage: Path) -> dict:
    require(
        appimage.is_file() and not appimage.is_symlink(),
        f"AppImage must be a regular non-symlink file: {appimage}",
    )
    require(appimage.suffix == ".AppImage", "artifact must end in .AppImage")
    require(appimage.stat().st_size > 0, "AppImage must not be empty")
    require(
        appimage.stat().st_mode & stat.S_IXUSR != 0,
        "AppImage must have its executable bit set",
    )

    with tempfile.TemporaryDirectory(prefix="lsdj-appimage-") as temporary:
        extraction_dir = Path(temporary)
        result = subprocess.run(
            [str(appimage.resolve()), "--appimage-extract"],
            cwd=extraction_dir,
            check=False,
            capture_output=True,
            text=True,
        )
        require(
            result.returncode == 0,
            f"AppImage extraction failed ({result.returncode}): {result.stderr[-2000:]}",
        )
        root = extraction_dir / "squashfs-root"
        binary = root / "usr/bin/lsdj-app"
        libraries = needed_libraries(binary)
        audit = verify_extracted(root, libraries)
        audit["artifact"] = appimage.name
        return audit


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("appimage", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    audit = verify_appimage(args.appimage)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(audit, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )


if __name__ == "__main__":
    try:
        main()
    except (AppImageError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit(f"Linux package verification failed: {error}") from error
