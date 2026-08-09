import json

import pytest

from lsdj.mrt2 import (
    MODEL_SNAPSHOTS,
    PYTORCH_CUDA_RUNTIME,
    UNVERIFIED_OPT_IN,
    RuntimeUnavailable,
    runtime_manifest,
    select_runtime,
)
from lsdj.sidecar import main


def test_platform_default_is_explicit_and_never_cpu():
    mac = select_runtime("auto", platform="darwin", env={})
    assert (mac.name, mac.accelerator) == ("mlx", "metal")

    for platform in ("linux", "win32"):
        selected = select_runtime(
            "auto", platform=platform, env={UNVERIFIED_OPT_IN: "1"}
        )
        assert selected.name == PYTORCH_CUDA_RUNTIME
        assert selected.accelerator == "cuda"
        assert selected.experimental is True


def test_pytorch_runtime_fails_closed_until_hardware_is_qualified():
    with pytest.raises(RuntimeUnavailable, match="two-deck hardware results"):
        select_runtime("pytorch-cuda", platform="linux", env={})


def test_runtime_platform_mismatches_are_clear():
    with pytest.raises(RuntimeUnavailable, match="macOS-only"):
        select_runtime("mlx", platform="win32", env={})
    with pytest.raises(RuntimeUnavailable, match="Linux and Windows"):
        select_runtime(
            "pytorch-cuda",
            platform="darwin",
            env={UNVERIFIED_OPT_IN: "1"},
        )
    with pytest.raises(RuntimeUnavailable, match="unsupported platform"):
        select_runtime("auto", platform="freebsd", env={})


def test_manifest_keeps_every_external_dependency_immutable():
    manifest = runtime_manifest()
    assert manifest["cpu_fallback"] is False
    assert manifest["release_ready"] is False
    assert manifest["topology"] == "shared-worker-two-state"
    assert manifest["topology_implemented"] is True
    pins = [manifest["adapter_reference"]["revision"], manifest["processor"]["revision"]]
    pins.extend(model["revision"] for model in manifest["models"].values())
    assert all(len(pin) == 40 for pin in pins)
    assert manifest["models"] == MODEL_SNAPSHOTS
    assert manifest["adapter_reference"]["role"].endswith("not executed by LSDJ")
    assert manifest["runtime_candidate"]["lock_status"] == "hash_locked_uninstalled"
    assert set(manifest["runtime_candidate"]["locks"]) == {
        "linux-x86_64",
        "windows-x86_64",
    }


def test_runtime_info_cli_is_model_free_and_machine_readable(capsys):
    main(["--runtime-info"])
    payload = json.loads(capsys.readouterr().out)
    assert payload["runtime"] == "pytorch-cuda"
    assert payload["release_ready"] is False
