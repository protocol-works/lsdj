"""Model-manager backend (issue #43): dynamic Magenta discovery, SA3 readiness
states, and the sidecar model-tooling JSON progress contract.

The tooling tests stub the upstream `mrt models` command callbacks so the reuse
path — click output → JSON progress, sys.exit → error event — is exercised
without HuggingFace or weights.
"""

import json
import pathlib

import pytest

from lsdj import engine, runtime_paths, sa3, sidecar


# --- Dynamic Magenta discovery --------------------------------------------


def _model(models_dir: pathlib.Path, name: str, *, complete: bool) -> None:
    folder = models_dir / name
    folder.mkdir(parents=True)
    (folder / f"{name}.mlxfn").write_text("")
    if complete:
        (folder / f"{name}_state.safetensors").write_text("")


def test_available_models_discovers_any_complete_folder(tmp_path, monkeypatch):
    monkeypatch.setattr("magenta_rt.paths.models_dir", lambda: tmp_path)
    _model(tmp_path, "mrt2_small", complete=True)
    _model(tmp_path, "half", complete=False)  # missing safetensors → hidden
    _model(tmp_path, "custom_x", complete=True)  # unknown name → still shown
    assert engine.available_models() == ["custom_x", "mrt2_small"]


def test_available_models_empty_when_dir_absent(tmp_path, monkeypatch):
    monkeypatch.setattr("magenta_rt.paths.models_dir", lambda: tmp_path / "nope")
    assert engine.available_models() == []


# --- SA3 readiness states --------------------------------------------------


def _checkout(root: pathlib.Path, *, venv: bool, warmed: bool) -> None:
    mlx = root / "optimized" / "mlx"
    mlx.mkdir(parents=True)
    if venv:
        python = runtime_paths.venv_python(mlx / ".venv", platform="darwin")
        python.parent.mkdir(parents=True)
        python.write_text("")
        (mlx / "scripts").mkdir()
        (mlx / "scripts" / "sa3_mlx.py").write_text("")
    if warmed:
        (mlx / sa3.WARMED_STAMP).write_text("")


@pytest.mark.parametrize(
    "venv,warmed,expected",
    [
        (False, False, sa3.STATE_VENV_MISSING),  # checkout dir, no venv
        (True, False, sa3.STATE_NOT_WARMED),  # venv, no stamp
        (True, True, sa3.STATE_READY),  # venv + stamp
    ],
)
def test_readiness_classifies_a_checkout(tmp_path, venv, warmed, expected):
    root = tmp_path / "co"
    _checkout(root, venv=venv, warmed=warmed)
    result = sa3.readiness(
        env={"SA3_MLX_HOME": str(root)},
        home=tmp_path / "home",
        platform_name="darwin",
        machine="arm64",
    )
    assert result["state"] == expected
    assert result["checkout"] == str(root)


def test_readiness_missing_when_no_checkout(tmp_path):
    result = sa3.readiness(env={}, home=tmp_path)
    assert result["state"] == sa3.STATE_MISSING
    assert result["checkout"] is None


# --- Sidecar model-tooling JSON progress contract --------------------------


def _patch_download(monkeypatch, fake) -> None:
    from magenta_rt.cli import models_commands as mc

    monkeypatch.setattr(mc.models.commands["download"], "callback", fake)


def _lines(captured: str) -> list[dict]:
    return [json.loads(line) for line in captured.splitlines() if line.strip()]


def test_download_emits_per_file_progress_then_done(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr("magenta_rt.paths.magenta_home", lambda: tmp_path)

    def fake_download(name, download_path, source):
        import click

        click.echo(f"  Downloading {name}/state.safetensors …")

    _patch_download(monkeypatch, fake_download)
    sidecar.run_model_tooling(download_model="mrt2_small")

    lines = _lines(capsys.readouterr().out)
    events = [line["event"] for line in lines]
    assert events[0] == "stage"
    assert {"event": "file", "file": "mrt2_small/state.safetensors"} in lines
    assert events[-1] == "done"


def test_download_failure_becomes_an_error_event(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr("magenta_rt.paths.magenta_home", lambda: tmp_path)

    def fake_download(name, download_path, source):
        import sys

        import click

        click.echo("Error: no weights", err=True)
        sys.exit(1)

    _patch_download(monkeypatch, fake_download)
    with pytest.raises(SystemExit):
        sidecar.run_model_tooling(download_model="mrt2_small")

    lines = _lines(capsys.readouterr().out)
    # A structured error rather than a bare exit — and it carries the tooling's
    # actual reason, not just an exit code, so the no-terminal UI is actionable.
    assert lines[-1]["event"] == "error"
    assert "no weights" in lines[-1]["message"]
