"""Cross-process NVIDIA work admission for MRT2 and Stable Audio 3.

The audio decks have strict priority.  A Stable Audio lease is admitted only
when no MRT2 generation is running or waiting and its measured reservation fits
inside the caller-provided VRAM budget.  If MRT2 arrives while Stable Audio is
sampling, the SA3 callback observes the waiter and cancels its disposable child
process before MRT2 is admitted.

State lives below the app-owned cache root and is guarded with an OS file lock;
no daemon, shell command, system Python, or third-party lock package is needed.
Dead-process records are pruned on every operation, so a killed worker cannot
leave the GPU permanently reserved.
"""

from __future__ import annotations

import contextlib
import dataclasses
import enum
import json
import os
import pathlib
import tempfile
import time
import uuid
from collections.abc import Callable, Iterator
from typing import Any


SCHEMA_VERSION = 1
MAX_RECORDS = 32
DEFAULT_POLL_SECONDS = 0.05


class Priority(enum.IntEnum):
    SA3_BACKGROUND = 10
    MRT2_REALTIME = 100


class BrokerError(RuntimeError):
    """The broker state is invalid or work cannot be admitted safely."""


class BrokerCancelled(BrokerError):
    """The caller cancelled while waiting for the GPU."""


class BrokerTimeout(BrokerError):
    """The caller's bounded admission deadline expired."""


@dataclasses.dataclass(frozen=True)
class Lease:
    token: str
    service: str
    priority: Priority
    reservation_bytes: int
    pid: int


def _pid_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    if pid == os.getpid():
        return True
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        # Windows can reject signal 0 for an otherwise-live process.  Retaining
        # the record is safer than admitting overlapping GPU work.
        return True
    return True


@contextlib.contextmanager
def _os_file_lock(path: pathlib.Path) -> Iterator[None]:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_symlink():
        raise BrokerError("GPU broker lock path must not be a symlink")
    with path.open("a+b") as handle:
        handle.seek(0, os.SEEK_END)
        if handle.tell() == 0:
            handle.write(b"\0")
            handle.flush()
        handle.seek(0)
        if os.name == "nt":
            import msvcrt

            msvcrt.locking(handle.fileno(), msvcrt.LK_LOCK, 1)
            try:
                yield
            finally:
                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


class GpuBroker:
    def __init__(
        self,
        root: pathlib.Path,
        *,
        poll_seconds: float = DEFAULT_POLL_SECONDS,
        clock: Callable[[], float] = time.monotonic,
        sleeper: Callable[[float], None] = time.sleep,
        pid_alive: Callable[[int], bool] = _pid_alive,
    ) -> None:
        if poll_seconds <= 0:
            raise ValueError("poll_seconds must be positive")
        self.root = root
        self.state_path = root / "state.json"
        self.lock_path = root / "state.lock"
        self.poll_seconds = poll_seconds
        self._clock = clock
        self._sleep = sleeper
        self._pid_alive = pid_alive

    def _empty_state(self) -> dict[str, Any]:
        return {"schema_version": SCHEMA_VERSION, "waiters": [], "leases": []}

    def _read_state(self) -> dict[str, Any]:
        if not self.state_path.exists():
            return self._empty_state()
        if self.state_path.is_symlink():
            raise BrokerError("GPU broker state path must not be a symlink")
        try:
            parsed = json.loads(self.state_path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
            raise BrokerError("GPU broker state is unreadable") from error
        if (
            not isinstance(parsed, dict)
            or parsed.get("schema_version") != SCHEMA_VERSION
        ):
            raise BrokerError("GPU broker state has an unsupported schema")
        for field in ("waiters", "leases"):
            records = parsed.get(field)
            if not isinstance(records, list) or len(records) > MAX_RECORDS:
                raise BrokerError(f"GPU broker {field} are invalid")
            for record in records:
                if not self._valid_record(record):
                    raise BrokerError(f"GPU broker {field} contain an invalid record")
        return parsed

    @staticmethod
    def _valid_record(record: Any) -> bool:
        return (
            isinstance(record, dict)
            and isinstance(record.get("token"), str)
            and 1 <= len(record["token"]) <= 64
            and isinstance(record.get("service"), str)
            and 1 <= len(record["service"]) <= 64
            and isinstance(record.get("priority"), int)
            and not isinstance(record["priority"], bool)
            and record["priority"] in {int(item) for item in Priority}
            and isinstance(record.get("reservation_bytes"), int)
            and not isinstance(record["reservation_bytes"], bool)
            and record["reservation_bytes"] >= 0
            and isinstance(record.get("pid"), int)
            and record["pid"] > 0
        )

    def _write_state(self, state: dict[str, Any]) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        if self.state_path.is_symlink():
            raise BrokerError("GPU broker state path must not be a symlink")
        descriptor, temporary_name = tempfile.mkstemp(
            prefix="state.", suffix=".tmp", dir=self.root
        )
        temporary = pathlib.Path(temporary_name)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                json.dump(state, handle, sort_keys=True, separators=(",", ":"))
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, self.state_path)
        finally:
            with contextlib.suppress(FileNotFoundError):
                temporary.unlink()

    def _prune(self, state: dict[str, Any]) -> None:
        for field in ("waiters", "leases"):
            state[field] = [
                record for record in state[field] if self._pid_alive(record["pid"])
            ]

    @contextlib.contextmanager
    def _locked_state(self) -> Iterator[dict[str, Any]]:
        with _os_file_lock(self.lock_path):
            state = self._read_state()
            self._prune(state)
            yield state
            self._write_state(state)

    @staticmethod
    def _record(lease: Lease) -> dict[str, Any]:
        return {
            "token": lease.token,
            "service": lease.service,
            "priority": int(lease.priority),
            "reservation_bytes": lease.reservation_bytes,
            "pid": lease.pid,
        }

    def acquire(
        self,
        service: str,
        priority: Priority,
        *,
        reservation_bytes: int,
        capacity_bytes: int,
        timeout_seconds: float,
        cancelled: Callable[[], bool] = lambda: False,
    ) -> Lease:
        if not service or len(service) > 64:
            raise ValueError("service must contain 1-64 characters")
        if reservation_bytes < 0 or capacity_bytes < 0:
            raise ValueError("GPU byte counts must not be negative")
        if timeout_seconds <= 0:
            raise ValueError("timeout_seconds must be positive")
        lease = Lease(
            token=uuid.uuid4().hex,
            service=service,
            priority=priority,
            reservation_bytes=reservation_bytes,
            pid=os.getpid(),
        )
        record = self._record(lease)
        deadline = self._clock() + timeout_seconds
        registered = False
        try:
            while True:
                if cancelled():
                    raise BrokerCancelled(f"{service} GPU request was cancelled")
                if self._clock() >= deadline:
                    raise BrokerTimeout(f"{service} timed out waiting for the GPU")
                with self._locked_state() as state:
                    if not registered:
                        if len(state["waiters"]) + len(state["leases"]) >= MAX_RECORDS:
                            raise BrokerError(
                                "GPU broker is at its bounded record limit"
                            )
                        state["waiters"].append(record)
                        registered = True
                    higher_waiting = any(
                        item["token"] != lease.token
                        and item["priority"] > int(priority)
                        for item in state["waiters"]
                    )
                    active_higher = any(
                        item["priority"] > int(priority) for item in state["leases"]
                    )
                    active_lower = any(
                        item["priority"] < int(priority) for item in state["leases"]
                    )
                    reserved = sum(
                        item["reservation_bytes"] for item in state["leases"]
                    )
                    fits = reservation_bytes <= max(0, capacity_bytes - reserved)
                    if (
                        not higher_waiting
                        and not active_higher
                        and not active_lower
                        and fits
                    ):
                        state["waiters"] = [
                            item
                            for item in state["waiters"]
                            if item["token"] != lease.token
                        ]
                        state["leases"].append(record)
                        return lease
                self._sleep(self.poll_seconds)
        except Exception:
            if registered:
                self._remove(lease.token)
            raise

    def _remove(self, token: str) -> None:
        with self._locked_state() as state:
            for field in ("waiters", "leases"):
                state[field] = [item for item in state[field] if item["token"] != token]

    def release(self, lease: Lease) -> None:
        self._remove(lease.token)

    @contextlib.contextmanager
    def hold(self, *args: Any, **kwargs: Any) -> Iterator[Lease]:
        lease = self.acquire(*args, **kwargs)
        try:
            yield lease
        finally:
            self.release(lease)

    def should_yield(self, lease: Lease) -> bool:
        with self._locked_state() as state:
            live = any(item["token"] == lease.token for item in state["leases"])
            if not live:
                raise BrokerError("GPU lease is no longer live")
            return any(
                item["priority"] > int(lease.priority) for item in state["waiters"]
            )

    def diagnostics(self) -> dict[str, Any]:
        with self._locked_state() as state:
            return json.loads(json.dumps(state))
