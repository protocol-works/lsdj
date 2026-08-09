import ctypes
import json
import pathlib

import pytest

from lsdj import gpu_broker
from lsdj.gpu_broker import (
    BrokerCancelled,
    BrokerError,
    BrokerTimeout,
    GpuBroker,
    Priority,
)


class FakeWindowsCall:
    def __init__(self, callback):
        self.callback = callback
        self.argtypes = None
        self.restype = None

    def __call__(self, *args):
        return self.callback(*args)


class FakeKernel32:
    def __init__(
        self,
        *,
        handle=41,
        exit_code=gpu_broker.STILL_ACTIVE,
        query_succeeds=True,
    ):
        self.handle = handle
        self.exit_code = exit_code
        self.query_succeeds = query_succeeds
        self.closed = []
        self.OpenProcess = FakeWindowsCall(self._open_process)
        self.GetExitCodeProcess = FakeWindowsCall(self._get_exit_code)
        self.CloseHandle = FakeWindowsCall(self._close_handle)

    def _open_process(self, access, inherit, pid):
        assert access == gpu_broker.PROCESS_QUERY_LIMITED_INFORMATION
        assert inherit is False
        assert pid > 0
        return self.handle

    def _get_exit_code(self, handle, destination):
        assert handle == self.handle
        destination._obj.value = self.exit_code
        return self.query_succeeds

    def _close_handle(self, handle):
        self.closed.append(handle)
        return True


def install_fake_windows(monkeypatch, kernel, *, last_error=0):
    monkeypatch.setattr(
        ctypes, "WinDLL", lambda *_args, **_kwargs: kernel, raising=False
    )
    monkeypatch.setattr(ctypes, "get_last_error", lambda: last_error, raising=False)


def broker(tmp_path: pathlib.Path) -> GpuBroker:
    return GpuBroker(tmp_path / "gpu-broker", poll_seconds=0.001)


@pytest.mark.parametrize(
    ("exit_code", "expected"),
    [(gpu_broker.STILL_ACTIVE, True), (0, False)],
)
def test_windows_liveness_queries_exit_state_and_closes_handle(
    monkeypatch, exit_code, expected
):
    kernel = FakeKernel32(exit_code=exit_code)
    install_fake_windows(monkeypatch, kernel)

    assert gpu_broker._windows_pid_alive(1234) is expected
    assert kernel.closed == [kernel.handle]


@pytest.mark.parametrize(
    ("last_error", "expected"),
    [(gpu_broker.ERROR_INVALID_PARAMETER, False), (5, True), (12345, True)],
)
def test_windows_open_failure_only_prunes_a_definitively_invalid_pid(
    monkeypatch, last_error, expected
):
    kernel = FakeKernel32(handle=0)
    install_fake_windows(monkeypatch, kernel, last_error=last_error)

    assert gpu_broker._windows_pid_alive(1234) is expected
    assert kernel.closed == []


def test_windows_failed_exit_query_fails_closed_and_closes_handle(monkeypatch):
    kernel = FakeKernel32(query_succeeds=False)
    install_fake_windows(monkeypatch, kernel)

    assert gpu_broker._windows_pid_alive(1234) is True
    assert kernel.closed == [kernel.handle]


@pytest.mark.parametrize(
    ("error", "expected"),
    [(ProcessLookupError(), False), (PermissionError(), True), (OSError(), True)],
)
def test_posix_liveness_semantics_are_preserved(monkeypatch, error, expected):
    def fail(_pid, _signal):
        raise error

    monkeypatch.setattr(gpu_broker.os, "kill", fail)
    assert gpu_broker._pid_alive(gpu_broker.os.getpid() + 1000) is expected


def test_sa3_lease_is_bounded_by_measured_capacity(tmp_path):
    service = broker(tmp_path)
    with pytest.raises(BrokerTimeout):
        service.acquire(
            "sa3",
            Priority.SA3_BACKGROUND,
            reservation_bytes=8,
            capacity_bytes=7,
            timeout_seconds=0.01,
        )
    assert service.diagnostics()["waiters"] == []


def test_mrt2_waiter_preempts_sa3_at_the_next_callback(tmp_path):
    service = broker(tmp_path)
    sa3 = service.acquire(
        "sa3",
        Priority.SA3_BACKGROUND,
        reservation_bytes=8,
        capacity_bytes=16,
        timeout_seconds=1,
    )
    state = service.diagnostics()
    state["waiters"].append(
        {
            "token": "mrt2-waiter",
            "service": "mrt2",
            "priority": int(Priority.MRT2_REALTIME),
            "reservation_bytes": 0,
            "pid": sa3.pid,
        }
    )
    service._write_state(state)

    assert service.should_yield(sa3) is True
    service.release(sa3)
    assert service.diagnostics()["leases"] == []


def test_active_mrt2_blocks_background_generation(tmp_path):
    service = broker(tmp_path)
    realtime = service.acquire(
        "mrt2",
        Priority.MRT2_REALTIME,
        reservation_bytes=0,
        capacity_bytes=0,
        timeout_seconds=1,
    )
    with pytest.raises(BrokerTimeout):
        service.acquire(
            "sa3",
            Priority.SA3_BACKGROUND,
            reservation_bytes=1,
            capacity_bytes=16,
            timeout_seconds=0.01,
        )
    service.release(realtime)


def test_cancelled_waiter_is_removed(tmp_path):
    service = broker(tmp_path)
    realtime = service.acquire(
        "mrt2",
        Priority.MRT2_REALTIME,
        reservation_bytes=0,
        capacity_bytes=0,
        timeout_seconds=1,
    )
    with pytest.raises(BrokerCancelled):
        service.acquire(
            "sa3",
            Priority.SA3_BACKGROUND,
            reservation_bytes=1,
            capacity_bytes=16,
            timeout_seconds=1,
            cancelled=lambda: True,
        )
    assert service.diagnostics()["waiters"] == []
    service.release(realtime)


def test_dead_process_records_are_recovered(tmp_path):
    service = GpuBroker(
        tmp_path / "gpu-broker", poll_seconds=0.001, pid_alive=lambda pid: pid != 99
    )
    service.root.mkdir(parents=True)
    service.state_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "waiters": [],
                "leases": [
                    {
                        "token": "dead",
                        "service": "sa3",
                        "priority": int(Priority.SA3_BACKGROUND),
                        "reservation_bytes": 8,
                        "pid": 99,
                    }
                ],
            }
        )
    )
    lease = service.acquire(
        "mrt2",
        Priority.MRT2_REALTIME,
        reservation_bytes=0,
        capacity_bytes=0,
        timeout_seconds=1,
    )
    assert [item["token"] for item in service.diagnostics()["leases"]] == [lease.token]


def test_tampered_or_unbounded_state_fails_closed(tmp_path):
    service = broker(tmp_path)
    service.root.mkdir(parents=True)
    service.state_path.write_text("{}")
    with pytest.raises(BrokerError, match="unsupported schema"):
        service.diagnostics()
