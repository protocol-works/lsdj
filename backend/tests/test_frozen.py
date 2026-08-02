"""The frozen backend's mode dispatcher keeps one dependency tree for all CLIs."""

from lsdj import controller, frozen, sidecar


def test_runtime_check_has_an_explicit_build_only_mode(monkeypatch):
    calls = []
    monkeypatch.setattr(frozen, "check_runtime", lambda: calls.append("checked"))

    frozen.main(["--check-runtime"])

    assert calls == ["checked"]


def test_default_mode_dispatches_to_sidecar(monkeypatch):
    calls = []
    monkeypatch.setattr(sidecar, "main", lambda argv: calls.append(argv))

    frozen.main(["--deck", "a", "--model", "mrt2_small", "--port", "4321"])

    assert calls == [["--deck", "a", "--model", "mrt2_small", "--port", "4321"]]


def test_generation_mode_consumes_only_its_dispatch_flag(monkeypatch):
    calls = []
    monkeypatch.setattr(controller, "main", lambda argv: calls.append(argv))

    frozen.main(["--generation-server", "--port", "4321"])

    assert calls == [["--port", "4321"]]
