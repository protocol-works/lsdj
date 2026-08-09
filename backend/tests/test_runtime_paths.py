"""The Rust→Python storage and executable-layout contract."""

import pathlib

from lsdj import runtime_paths


def test_all_roots_preserve_spaces_and_non_ascii():
    env = {
        "LSDJ_CONFIG_HOME": "/profiles/DJ Name/音楽/config",
        "LSDJ_DATA_HOME": "/profiles/DJ Name/音楽/data",
        "LSDJ_CACHE_HOME": "/profiles/DJ Name/音楽/cache",
        "LSDJ_ASSETS_HOME": "/profiles/DJ Name/音楽/assets",
        "LSDJ_STAGING_HOME": "/profiles/DJ Name/音楽/staging",
    }
    assert runtime_paths.config_home(env) == pathlib.Path(env["LSDJ_CONFIG_HOME"])
    assert runtime_paths.data_home(env) == pathlib.Path(env["LSDJ_DATA_HOME"])
    assert runtime_paths.cache_home(env) == pathlib.Path(env["LSDJ_CACHE_HOME"])
    assert runtime_paths.assets_home(env) == pathlib.Path(env["LSDJ_ASSETS_HOME"])
    assert runtime_paths.staging_home(env) == pathlib.Path(env["LSDJ_STAGING_HOME"])
    assert (
        runtime_paths.sa3_home(env)
        == pathlib.Path(env["LSDJ_ASSETS_HOME"]) / "stable-audio-3"
    )
    assert (
        runtime_paths.loras_home(env)
        == pathlib.Path(env["LSDJ_ASSETS_HOME"]) / "sa3-loras"
    )


def test_backend_neutral_override_wins_without_home_guessing():
    env = {
        "LSDJ_ASSETS_HOME": "/host/assets",
        "SA3_HOME": "/custom/portable SA 3",
        "SA3_TFLITE_HOME": "/custom/TFLite SA 3",
        "SA3_MLX_HOME": "/custom/MLX SA 3",
    }
    assert runtime_paths.sa3_home(env) == pathlib.Path("/custom/portable SA 3")


def test_backend_specific_compatibility_overrides_win_without_home_guessing():
    env = {
        "LSDJ_ASSETS_HOME": "/host/assets",
        "SA3_TFLITE_HOME": "/custom/TFLite SA 3",
        "SA3_MLX_HOME": "/custom/MLX SA 3",
        "SA3_LORAS_HOME": "/custom/适配器",
    }
    assert runtime_paths.sa3_home(env) == pathlib.Path("/custom/TFLite SA 3")
    assert runtime_paths.loras_home(env) == pathlib.Path("/custom/适配器")
    assert runtime_paths.sa3_home({}) is None
    assert runtime_paths.loras_home({}) is None

    assert runtime_paths.sa3_home({"SA3_MLX_HOME": "/custom/MLX SA 3"}) == (
        pathlib.Path("/custom/MLX SA 3")
    )


def test_venv_interpreter_layout_is_platform_specific_and_structured():
    venv = pathlib.Path("/profiles/DJ Name/模型/.venv")
    assert (
        runtime_paths.venv_python(venv, platform="win32")
        == venv / "Scripts" / "python.exe"
    )
    assert runtime_paths.venv_python(venv, platform="linux") == venv / "bin" / "python"
    assert runtime_paths.venv_python(venv, platform="darwin") == venv / "bin" / "python"
