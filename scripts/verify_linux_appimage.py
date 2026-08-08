#!/usr/bin/env python3
"""Verify an LSDJ x86_64 AppImage and emit deterministic package audit data."""

from __future__ import annotations

import argparse
import json
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


def desktop_entries(path: Path) -> dict[str, str]:
    require(path.is_file() and not path.is_symlink(), f"unsafe desktop entry: {path}")
    entries: dict[str, str] = {}
    section = ""
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            continue
        if section != "Desktop Entry" or "=" not in line:
            continue
        key, value = line.split("=", 1)
        if key in entries:
            raise AppImageError(f"duplicate desktop key: {key}")
        entries[key] = value
    return entries


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
    app_run = root / "AppRun"
    require(app_run.exists(), "AppImage has no AppRun entry point")

    desktop_files = sorted(root.glob("*.desktop"))
    require(len(desktop_files) == 1, "AppImage must contain exactly one desktop entry")
    desktop = desktop_entries(desktop_files[0])
    require(desktop.get("Type") == "Application", "desktop Type must be Application")
    require(desktop.get("Name") == "LSDJ", "desktop Name must be LSDJ")
    require("lsdj-app" in desktop.get("Exec", ""), "desktop Exec must launch lsdj-app")
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
