import json
import pathlib

import pytest

from lsdj.gpu_broker import (
    BrokerCancelled,
    BrokerError,
    BrokerTimeout,
    GpuBroker,
    Priority,
)


def broker(tmp_path: pathlib.Path) -> GpuBroker:
    return GpuBroker(tmp_path / "gpu-broker", poll_seconds=0.001)


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
