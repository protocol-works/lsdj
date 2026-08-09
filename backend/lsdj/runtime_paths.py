"""Filesystem roots supplied by the native Rust host.

The desktop host is the only component that knows platform conventions. Python
services consume these explicit values and never reconstruct Windows, XDG, or
macOS locations from a user home directory. Compatibility variables remain for
the current upstream runtimes, but they are populated by the same Rust contract.
"""

import os
import pathlib
import sys
from collections.abc import Mapping


def _path(env: Mapping[str, str], name: str) -> pathlib.Path | None:
    value = env.get(name, "")
    return pathlib.Path(value) if value else None


def config_home(env: Mapping[str, str] | None = None) -> pathlib.Path | None:
    return _path(os.environ if env is None else env, "LSDJ_CONFIG_HOME")


def data_home(env: Mapping[str, str] | None = None) -> pathlib.Path | None:
    return _path(os.environ if env is None else env, "LSDJ_DATA_HOME")


def cache_home(env: Mapping[str, str] | None = None) -> pathlib.Path | None:
    return _path(os.environ if env is None else env, "LSDJ_CACHE_HOME")


def assets_home(env: Mapping[str, str] | None = None) -> pathlib.Path | None:
    return _path(os.environ if env is None else env, "LSDJ_ASSETS_HOME")


def staging_home(env: Mapping[str, str] | None = None) -> pathlib.Path | None:
    return _path(os.environ if env is None else env, "LSDJ_STAGING_HOME")


def sa3_home(env: Mapping[str, str] | None = None) -> pathlib.Path | None:
    env = os.environ if env is None else env
    neutral_override = _path(env, "SA3_HOME")
    if neutral_override is not None:
        return neutral_override
    tflite_override = _path(env, "SA3_TFLITE_HOME")
    if tflite_override is not None:
        return tflite_override
    override = _path(env, "SA3_MLX_HOME")
    if override is not None:
        return override
    assets = assets_home(env)
    return None if assets is None else assets / "stable-audio-3"


def loras_home(env: Mapping[str, str] | None = None) -> pathlib.Path | None:
    env = os.environ if env is None else env
    override = _path(env, "SA3_LORAS_HOME")
    if override is not None:
        return override
    assets = assets_home(env)
    return None if assets is None else assets / "sa3-loras"


def venv_python(venv: pathlib.Path, *, platform: str | None = None) -> pathlib.Path:
    """Return a venv interpreter as a path/argv item, never a shell string."""
    platform = sys.platform if platform is None else platform
    if platform == "win32":
        return venv / "Scripts" / "python.exe"
    return venv / "bin" / "python"
