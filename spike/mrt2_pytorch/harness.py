"""Two-deck MRT2 benchmark harness for issue #109.

This module deliberately has no LSDJ production imports. The real adapter loads
the pinned Hugging Face snapshot with ``local_files_only=True``; the dry adapter
lets CI exercise scheduling, topology, ring accounting, and result schemas with
no model, GPU, or network.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import math
import multiprocessing
import os
import pathlib
import platform
import queue
import statistics
import subprocess
import sys
import time
import traceback
from collections.abc import Iterable
from typing import Any

SAMPLE_RATE = 48_000
CHANNELS = 2
FRAME_SECONDS = 0.04
DECKS = (0, 1)
DEFAULT_PREBUFFER_SECONDS = 1.5

SOURCE_REPOSITORY = "https://github.com/multimodalart/magenta-realtime-torch.git"
SOURCE_REVISION = "6d076baa3df3b10448876c400521a015a5137c59"
MODEL_REVISIONS = {
    "mrt2_base": (
        "magenta-community/magenta-realtime-2",
        "92087988d05d0fe38b11f021f0b0d00a75afb86b",
    ),
    "mrt2_small": (
        "magenta-community/magenta-realtime-2-small",
        "7037d99551c84ac5c6afb7f1a5e58c65e7233dbb",
    ),
}
PROCESSOR_REPOSITORY = "magenta-community/magenta-rt-musiccoca-torch"
PROCESSOR_REVISION = "236c488e38aa98643805514996934d705668298b"


@dataclasses.dataclass
class RingBudget:
    """Event-level proxy for LSDJ's per-deck 1.5 second prebuffer gate.

    The production engine counts callback blocks that find a primed ring short.
    The harness cannot observe the device callback, so it records starvation
    intervals and transitions. Both are labelled proxies in the JSON output.
    """

    prebuffer_seconds: float
    fill_seconds: float = 0.0
    primed: bool = False
    last_time: float | None = None
    playback_started_at: float | None = None
    underrun_events: int = 0
    underrun_seconds: float = 0.0
    fill_min_seconds: float | None = None
    fill_max_seconds: float = 0.0

    def advance(self, now: float) -> None:
        if self.last_time is None:
            self.last_time = now
            return
        elapsed = max(0.0, now - self.last_time)
        self.last_time = now
        if not self.primed:
            return
        before = self.fill_seconds
        self.fill_seconds = max(0.0, before - elapsed)
        self.fill_min_seconds = (
            self.fill_seconds
            if self.fill_min_seconds is None
            else min(self.fill_min_seconds, self.fill_seconds)
        )
        if elapsed > before:
            self.underrun_seconds += elapsed - before
            if before > 0.0 or self.underrun_events == 0:
                self.underrun_events += 1

    def push(self, audio_seconds: float, now: float) -> None:
        self.advance(now)
        self.fill_seconds += max(0.0, audio_seconds)
        self.fill_max_seconds = max(self.fill_max_seconds, self.fill_seconds)
        if not self.primed and self.fill_seconds >= self.prebuffer_seconds:
            self.primed = True
            self.playback_started_at = now
            self.fill_min_seconds = self.fill_seconds

    def ready_for_generation(self, target_seconds: float, chunk_seconds: float) -> bool:
        if not self.primed:
            return True
        return self.fill_seconds <= max(0.0, target_seconds - chunk_seconds)

    def result(self, origin: float) -> dict[str, Any]:
        return {
            "prebuffer_seconds": self.prebuffer_seconds,
            "primed": self.primed,
            "time_to_prime_seconds": (
                None
                if self.playback_started_at is None
                else round(self.playback_started_at - origin, 6)
            ),
            "fill_final_seconds": round(self.fill_seconds, 6),
            "fill_min_seconds": (
                None
                if self.fill_min_seconds is None
                else round(self.fill_min_seconds, 6)
            ),
            "fill_max_seconds": round(self.fill_max_seconds, 6),
            "underrun_proxy_events": self.underrun_events,
            "underrun_proxy_seconds": round(self.underrun_seconds, 6),
        }


def percentile(values: Iterable[float], percent: float) -> float | None:
    ordered = sorted(values)
    if not ordered:
        return None
    if len(ordered) == 1:
        return ordered[0]
    rank = (len(ordered) - 1) * percent / 100.0
    low = math.floor(rank)
    high = math.ceil(rank)
    if low == high:
        return ordered[low]
    return ordered[low] + (ordered[high] - ordered[low]) * (rank - low)


def latency_summary(values: list[float]) -> dict[str, Any]:
    return {
        "count": len(values),
        "mean_ms": None if not values else round(statistics.fmean(values) * 1_000, 3),
        "p50_ms": _milliseconds(percentile(values, 50)),
        "p95_ms": _milliseconds(percentile(values, 95)),
        "p99_ms": _milliseconds(percentile(values, 99)),
        "max_ms": _milliseconds(max(values) if values else None),
    }


def _milliseconds(value: float | None) -> float | None:
    return None if value is None else round(value * 1_000, 3)


def _rss_bytes() -> int | None:
    """Current worker RSS without adding a benchmark-only dependency."""

    if sys.platform.startswith("linux"):
        try:
            for line in pathlib.Path("/proc/self/status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) * 1_024
        except (OSError, ValueError, IndexError):
            return None
    if sys.platform == "win32":
        try:
            import ctypes
            from ctypes import wintypes

            class ProcessMemoryCounters(ctypes.Structure):
                _fields_ = [
                    ("cb", wintypes.DWORD),
                    ("PageFaultCount", wintypes.DWORD),
                    ("PeakWorkingSetSize", ctypes.c_size_t),
                    ("WorkingSetSize", ctypes.c_size_t),
                    ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                    ("PagefileUsage", ctypes.c_size_t),
                    ("PeakPagefileUsage", ctypes.c_size_t),
                ]

            counters = ProcessMemoryCounters()
            counters.cb = ctypes.sizeof(counters)
            ok = ctypes.windll.psapi.GetProcessMemoryInfo(
                ctypes.windll.kernel32.GetCurrentProcess(),
                ctypes.byref(counters),
                counters.cb,
            )
            return int(counters.WorkingSetSize) if ok else None
        except (AttributeError, OSError):
            return None
    try:
        import resource

        value = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        return int(value if sys.platform == "darwin" else value * 1_024)
    except (ImportError, OSError):
        return None


def _gpu_snapshot(worker_pids: set[int]) -> dict[str, Any] | None:
    """Best-effort NVIDIA metadata. Raw rows survive driver schema differences."""

    try:
        gpu = subprocess.run(
            [
                "nvidia-smi",
                "--query-gpu=index,name,uuid,driver_version,memory.total,memory.used,temperature.gpu,pstate,power.draw",
                "--format=csv,noheader,nounits",
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        )
        apps = subprocess.run(
            [
                "nvidia-smi",
                "--query-compute-apps=pid,used_memory",
                "--format=csv,noheader,nounits",
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (FileNotFoundError, subprocess.SubprocessError):
        return None
    worker_memory_mib = 0.0
    matching_rows: list[str] = []
    for row in apps.stdout.splitlines():
        cells = [cell.strip() for cell in row.split(",")]
        try:
            pid = int(cells[0])
            used = float(cells[1])
        except (ValueError, IndexError):
            continue
        if pid in worker_pids:
            matching_rows.append(row)
            worker_memory_mib += used
    return {
        "gpu_rows": gpu.stdout.splitlines(),
        "matching_compute_rows": matching_rows,
        "worker_vram_mib": worker_memory_mib,
    }


@dataclasses.dataclass(frozen=True)
class RunConfig:
    backend: str
    topology: str
    frames: int
    duration_seconds: float
    prebuffer_seconds: float
    target_ahead_seconds: float
    model: str
    acceleration: str
    guidance: bool
    dry_latency_ms: float
    startup_timeout_seconds: float
    worker_timeout_seconds: float
    seed: int
    prompt_change_seconds: float

    @property
    def chunk_seconds(self) -> float:
        return self.frames * FRAME_SECONDS


class DryAdapter:
    def __init__(self, config: RunConfig):
        self.delay = config.dry_latency_ms / 1_000.0
        self.states: dict[int, int] = {}

    def metadata(self) -> dict[str, Any]:
        return {"adapter": "dry-run", "accelerator": "simulated"}

    def reset(self) -> None:
        self.states.clear()

    def generate(self, deck: int, controls: dict[str, Any], frames: int) -> int:
        time.sleep(self.delay)
        self.states[deck] = self.states.get(deck, 0) + frames
        return frames * round(SAMPLE_RATE * FRAME_SECONDS)

    def device_memory(self) -> dict[str, int | None]:
        return {
            "cuda_allocated_bytes": None,
            "cuda_reserved_bytes": None,
            "cuda_peak_allocated_bytes": None,
        }


class UpstreamAdapter:
    """Thin, snapshot-pinned adapter around upstream's Transformers API."""

    def __init__(self, config: RunConfig):
        import torch
        from huggingface_hub import snapshot_download
        from transformers import AutoModel

        if not torch.cuda.is_available():
            raise RuntimeError("PyTorch reports no CUDA accelerator")
        model_repo, model_revision = MODEL_REVISIONS[config.model]
        # Offline-only is intentional: installer/acquisition is a separate concern.
        model_path = snapshot_download(
            repo_id=model_repo,
            revision=model_revision,
            local_files_only=True,
        )
        processor_path = snapshot_download(
            repo_id=PROCESSOR_REPOSITORY,
            revision=PROCESSOR_REVISION,
            local_files_only=True,
        )
        self.torch = torch
        self.model = (
            AutoModel.from_pretrained(
                model_path,
                trust_remote_code=True,
                dtype=torch.bfloat16,
                local_files_only=True,
            )
            .to("cuda")
            .eval()
        )
        self.model.load_processor(processor_path, device="cuda")
        if config.acceleration == "torch-compile":
            self.model.compile_steps()
        elif config.acceleration != "eager":
            raise ValueError(f"unsupported acceleration mode {config.acceleration!r}")
        self.guidance = config.guidance
        self.states: dict[int, Any] = {}
        self.style_tokens: dict[
            tuple[tuple[str, ...], tuple[float, ...]], list[int]
        ] = {}
        self.torch.cuda.reset_peak_memory_stats()

    def metadata(self) -> dict[str, Any]:
        torch = self.torch
        props = torch.cuda.get_device_properties(torch.cuda.current_device())
        return {
            "adapter": "transformers-remote-code",
            "torch_version": torch.__version__,
            "torch_cuda_runtime": torch.version.cuda,
            "cudnn_version": torch.backends.cudnn.version(),
            "cuda_device": props.name,
            "cuda_capability": list(torch.cuda.get_device_capability()),
            "cuda_total_memory_bytes": props.total_memory,
        }

    def reset(self) -> None:
        self.states.clear()

    def _tokens(self, controls: dict[str, Any]) -> list[int]:
        prompts = tuple(controls["prompts"])
        weights = tuple(float(value) for value in controls["weights"])
        key = prompts, weights
        if key not in self.style_tokens:
            self.style_tokens[key] = self.model.processor.layer(prompts, weights)
        return self.style_tokens[key]

    def generate(self, deck: int, controls: dict[str, Any], frames: int) -> int:
        torch = self.torch
        torch.cuda.synchronize()
        audio, state = self.model.generate(
            style=self._tokens(controls),
            notes=controls["notes"],
            drums=controls["drums"],
            cfg_musiccoca=controls["cfg_musiccoca"],
            cfg_notes=controls["cfg_notes"],
            cfg_drums=controls["cfg_drums"],
            temperature=controls["temperature"],
            top_k=controls["top_k"],
            frames=frames,
            seed=controls["seed"],
            state=self.states.get(deck),
            guidance=self.guidance,
        )
        torch.cuda.synchronize()
        if getattr(audio, "ndim", None) != 2 or audio.shape[1] != CHANNELS:
            raise RuntimeError(f"upstream returned invalid audio shape {audio.shape!r}")
        self.states[deck] = state
        return int(audio.shape[0])

    def device_memory(self) -> dict[str, int]:
        torch = self.torch
        return {
            "cuda_allocated_bytes": torch.cuda.memory_allocated(),
            "cuda_reserved_bytes": torch.cuda.memory_reserved(),
            "cuda_peak_allocated_bytes": torch.cuda.max_memory_allocated(),
        }


def _controls(deck: int, changed: bool, onset: bool, seed: int) -> dict[str, Any]:
    notes = [-1] * 128
    if changed:
        notes[60 + deck * 7] = 2 if onset else 1
    return {
        "prompts": (
            ["warm disco funk", "analog synth bass"]
            if not changed
            else ["broken beat percussion", "ambient pads"]
        ),
        "weights": [0.7, 0.3] if not changed else [0.55, 0.45],
        "temperature": 1.1 if not changed else 0.95,
        "top_k": 50 if not changed else 64,
        "cfg_musiccoca": 1.6 if not changed else 2.0,
        "cfg_notes": 2.4,
        "cfg_drums": 4.0,
        "notes": notes,
        "drums": [-1] if deck == 0 else [0],
        "seed": seed + deck,
    }


def _worker_main(
    worker_id: int,
    request_queue: Any,
    result_queue: Any,
    config: RunConfig,
) -> None:
    started = time.perf_counter()
    try:
        adapter = (
            DryAdapter(config)
            if config.backend == "dry-run"
            else UpstreamAdapter(config)
        )
        result_queue.put(
            {
                "type": "ready",
                "worker": worker_id,
                "pid": os.getpid(),
                "startup_seconds": time.perf_counter() - started,
                "rss_bytes": _rss_bytes(),
                "metadata": adapter.metadata(),
                "device_memory": adapter.device_memory(),
            }
        )
        while True:
            request = request_queue.get()
            action = request["action"]
            if action == "shutdown":
                result_queue.put(
                    {
                        "type": "stopped",
                        "worker": worker_id,
                        "rss_bytes": _rss_bytes(),
                        "device_memory": adapter.device_memory(),
                    }
                )
                return
            if action == "reset":
                adapter.reset()
                result_queue.put({"type": "reset", "worker": worker_id})
                continue
            if action != "generate":
                raise ValueError(f"unknown worker action {action!r}")
            generated_at = time.perf_counter()
            sample_frames = adapter.generate(
                request["deck"], request["controls"], request["frames"]
            )
            result_queue.put(
                {
                    "type": "chunk",
                    "worker": worker_id,
                    "deck": request["deck"],
                    "sequence": request["sequence"],
                    "control_change": request["control_change"],
                    "latency_seconds": time.perf_counter() - generated_at,
                    "sample_frames": sample_frames,
                    "rss_bytes": _rss_bytes(),
                    "device_memory": adapter.device_memory(),
                }
            )
    except BaseException as error:
        result_queue.put(
            {
                "type": "error",
                "worker": worker_id,
                "pid": os.getpid(),
                "error": f"{type(error).__name__}: {error}",
                "traceback": traceback.format_exc(),
            }
        )


def _wait_for_messages(
    result_queue: Any,
    expected_type: str,
    count: int,
    timeout: float,
) -> list[dict[str, Any]]:
    deadline = time.monotonic() + timeout
    messages = []
    while len(messages) < count:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(
                f"timed out waiting for {count} {expected_type!r} messages"
            )
        message = result_queue.get(timeout=remaining)
        if message["type"] == "error":
            raise RuntimeError(
                f"worker failed: {message['error']}\n{message['traceback']}"
            )
        if message["type"] != expected_type:
            raise RuntimeError(
                f"expected worker message {expected_type!r}, got {message['type']!r}"
            )
        messages.append(message)
    return messages


def run_benchmark(config: RunConfig) -> dict[str, Any]:
    if config.topology not in {"shared-worker", "per-deck"}:
        raise ValueError(f"unknown topology {config.topology!r}")
    if config.frames not in {5, 25}:
        raise ValueError("issue #109 requires 5-frame or 25-frame runs")
    if config.target_ahead_seconds < config.prebuffer_seconds:
        raise ValueError("target ahead must be at least the prebuffer threshold")

    context = multiprocessing.get_context("spawn")
    result_queue = context.Queue()
    worker_ids = [0] if config.topology == "shared-worker" else [0, 1]
    request_queues = {worker: context.Queue() for worker in worker_ids}
    processes = {
        worker: context.Process(
            target=_worker_main,
            args=(worker, request_queues[worker], result_queue, config),
            name=f"mrt2-bench-{worker}",
        )
        for worker in worker_ids
    }
    launch_started = time.perf_counter()
    for process in processes.values():
        process.start()

    worker_for_deck = (
        {0: 0, 1: 0} if config.topology == "shared-worker" else {0: 0, 1: 1}
    )
    startup: list[dict[str, Any]] = []
    shutdown: list[dict[str, Any]] = []
    try:
        startup = _wait_for_messages(
            result_queue,
            "ready",
            len(worker_ids),
            config.startup_timeout_seconds,
        )
        ready_completed = time.perf_counter()

        # Warm each model process, then clear continuation state before measurement.
        for worker in worker_ids:
            deck = worker if config.topology == "per-deck" else 0
            request_queues[worker].put(
                {
                    "action": "generate",
                    "deck": deck,
                    "sequence": -1,
                    "frames": config.frames,
                    "controls": _controls(deck, False, False, config.seed),
                    "control_change": False,
                }
            )
        warmup = _wait_for_messages(
            result_queue,
            "chunk",
            len(worker_ids),
            config.worker_timeout_seconds,
        )
        for worker in worker_ids:
            request_queues[worker].put({"action": "reset"})
        _wait_for_messages(
            result_queue,
            "reset",
            len(worker_ids),
            config.worker_timeout_seconds,
        )

        origin = time.perf_counter()
        deadline = origin + config.duration_seconds
        rings = {
            deck: RingBudget(config.prebuffer_seconds, last_time=origin)
            for deck in DECKS
        }
        latencies = {deck: [] for deck in DECKS}
        change_latencies = {deck: [] for deck in DECKS}
        sample_frames = {deck: 0 for deck in DECKS}
        sequences = {deck: 0 for deck in DECKS}
        changed = {deck: False for deck in DECKS}
        onset_pending = {deck: False for deck in DECKS}
        inflight_workers: set[int] = set()
        rss_peak = {message["worker"]: message.get("rss_bytes") for message in startup}
        cuda_peaks = {worker: 0 for worker in worker_ids}
        gpu_samples: list[dict[str, Any]] = []
        next_gpu_sample = origin
        round_robin = 0

        while True:
            now = time.perf_counter()
            bounded_now = min(now, deadline)
            for ring in rings.values():
                ring.advance(bounded_now)

            elapsed = bounded_now - origin
            for deck in DECKS:
                if not changed[deck] and elapsed >= config.prompt_change_seconds:
                    changed[deck] = True
                    onset_pending[deck] = True

            if now < deadline:
                candidates = list(DECKS)
                if config.topology == "shared-worker":
                    candidates = [round_robin, 1 - round_robin]
                for deck in candidates:
                    worker = worker_for_deck[deck]
                    if worker in inflight_workers:
                        continue
                    if not rings[deck].ready_for_generation(
                        config.target_ahead_seconds, config.chunk_seconds
                    ):
                        continue
                    request_queues[worker].put(
                        {
                            "action": "generate",
                            "deck": deck,
                            "sequence": sequences[deck],
                            "frames": config.frames,
                            "controls": _controls(
                                deck,
                                changed[deck],
                                onset_pending[deck],
                                config.seed,
                            ),
                            "control_change": onset_pending[deck],
                        }
                    )
                    sequences[deck] += 1
                    inflight_workers.add(worker)
                    if config.topology == "shared-worker":
                        round_robin = 1 - deck

            worker_pids = {process.pid for process in processes.values() if process.pid}
            if now >= next_gpu_sample:
                sample = _gpu_snapshot(worker_pids)
                if sample is not None:
                    sample["elapsed_seconds"] = round(now - origin, 3)
                    gpu_samples.append(sample)
                next_gpu_sample = now + 1.0

            if now >= deadline and not inflight_workers:
                break
            try:
                message = result_queue.get(timeout=0.05)
            except queue.Empty:
                continue
            if message["type"] == "error":
                raise RuntimeError(
                    f"worker failed: {message['error']}\n{message['traceback']}"
                )
            if message["type"] != "chunk":
                raise RuntimeError(f"unexpected worker message {message['type']!r}")
            worker = message["worker"]
            deck = message["deck"]
            inflight_workers.discard(worker)
            completed = time.perf_counter()
            if completed <= deadline:
                audio_seconds = message["sample_frames"] / SAMPLE_RATE
                rings[deck].push(audio_seconds, completed)
                sample_frames[deck] += message["sample_frames"]
            latency = message["latency_seconds"]
            latencies[deck].append(latency)
            if message["control_change"]:
                change_latencies[deck].append(latency)
                onset_pending[deck] = False
            rss = message.get("rss_bytes")
            if rss is not None:
                rss_peak[worker] = max(rss_peak[worker] or 0, rss)
            allocated = message.get("device_memory", {}).get(
                "cuda_peak_allocated_bytes"
            )
            if allocated is not None:
                cuda_peaks[worker] = max(cuda_peaks[worker], allocated)

        ended = time.perf_counter()
        for ring in rings.values():
            ring.advance(deadline)

        stop_started = time.perf_counter()
        for worker in worker_ids:
            request_queues[worker].put({"action": "shutdown"})
        shutdown = _wait_for_messages(
            result_queue,
            "stopped",
            len(worker_ids),
            config.worker_timeout_seconds,
        )
        for process in processes.values():
            process.join(timeout=config.worker_timeout_seconds)
        shutdown_seconds = time.perf_counter() - stop_started

        vram_samples = [sample["worker_vram_mib"] for sample in gpu_samples]
        return {
            "schema_version": 1,
            "qualification": "synthetic" if config.backend == "dry-run" else "hardware",
            "config": dataclasses.asdict(config),
            "pins": {
                "source_repository": SOURCE_REPOSITORY,
                "source_revision": SOURCE_REVISION,
                "model": {
                    "repository": MODEL_REVISIONS[config.model][0],
                    "revision": MODEL_REVISIONS[config.model][1],
                },
                "processor": {
                    "repository": PROCESSOR_REPOSITORY,
                    "revision": PROCESSOR_REVISION,
                },
            },
            "host": {
                "platform": platform.platform(),
                "system": platform.system(),
                "release": platform.release(),
                "machine": platform.machine(),
                "python": platform.python_version(),
            },
            "workers": startup,
            "cold_start_wall_seconds": round(ready_completed - launch_started, 6),
            "warmup": warmup,
            "measurement_wall_seconds": round(ended - origin, 6),
            "shutdown_seconds": round(shutdown_seconds, 6),
            "shutdown": shutdown,
            "failure_domain": (
                "both decks share one process"
                if config.topology == "shared-worker"
                else "one process failure is isolated to one deck"
            ),
            "decks": {
                str(deck): {
                    "latency": latency_summary(latencies[deck]),
                    "control_change_latency": latency_summary(change_latencies[deck]),
                    "generated_audio_seconds": round(
                        sample_frames[deck] / SAMPLE_RATE, 6
                    ),
                    "generated_audio_to_wall_ratio": round(
                        sample_frames[deck] / SAMPLE_RATE / config.duration_seconds, 6
                    ),
                    "ring": rings[deck].result(origin),
                }
                for deck in DECKS
            },
            "memory": {
                "worker_rss_peak_bytes": rss_peak,
                "worker_cuda_peak_allocated_bytes": cuda_peaks,
                "nvidia_worker_vram_peak_mib": max(vram_samples)
                if vram_samples
                else None,
                "nvidia_samples": gpu_samples,
            },
            "notes": [
                "underrun_proxy_* is event-level 1.5 s ring simulation, not Rust engine telemetry",
                "a hardware qualification must also record the app's engine-reported underrun counter",
            ],
        }
    finally:
        for worker, process in processes.items():
            if process.is_alive():
                try:
                    request_queues[worker].put({"action": "shutdown"})
                    process.join(timeout=2)
                except (OSError, ValueError):
                    pass
            if process.is_alive():
                process.terminate()
                process.join(timeout=2)
        for request_queue in request_queues.values():
            request_queue.close()
        result_queue.close()


def _parse_csv_ints(value: str) -> list[int]:
    return [int(item.strip()) for item in value.split(",") if item.strip()]


def _parse_csv_strings(value: str) -> list[str]:
    return [item.strip() for item in value.split(",") if item.strip()]


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=("dry-run", "upstream"), default="dry-run")
    parser.add_argument(
        "--topologies", default="shared-worker,per-deck", help="comma-separated matrix"
    )
    parser.add_argument("--frames", default="25,5", help="comma-separated matrix")
    parser.add_argument("--duration-seconds", type=float, default=600.0)
    parser.add_argument(
        "--prebuffer-seconds", type=float, default=DEFAULT_PREBUFFER_SECONDS
    )
    parser.add_argument(
        "--target-ahead-seconds", type=float, default=DEFAULT_PREBUFFER_SECONDS
    )
    parser.add_argument("--model", choices=tuple(MODEL_REVISIONS), default="mrt2_small")
    parser.add_argument(
        "--acceleration", choices=("eager", "torch-compile"), default="eager"
    )
    parser.add_argument(
        "--token-cfg",
        action="store_true",
        help="use upstream token CFG instead of MLX-parity classifier-free guidance",
    )
    parser.add_argument("--dry-latency-ms", type=float, default=10.0)
    parser.add_argument("--startup-timeout-seconds", type=float, default=900.0)
    parser.add_argument("--worker-timeout-seconds", type=float, default=300.0)
    parser.add_argument("--seed", type=int, default=109)
    parser.add_argument("--prompt-change-seconds", type=float, default=30.0)
    parser.add_argument("--output", type=pathlib.Path)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    topologies = _parse_csv_strings(args.topologies)
    frames = _parse_csv_ints(args.frames)
    results = []
    for topology in topologies:
        for frame_count in frames:
            config = RunConfig(
                backend=args.backend,
                topology=topology,
                frames=frame_count,
                duration_seconds=args.duration_seconds,
                prebuffer_seconds=args.prebuffer_seconds,
                target_ahead_seconds=args.target_ahead_seconds,
                model=args.model,
                acceleration=args.acceleration,
                guidance=not args.token_cfg,
                dry_latency_ms=args.dry_latency_ms,
                startup_timeout_seconds=args.startup_timeout_seconds,
                worker_timeout_seconds=args.worker_timeout_seconds,
                seed=args.seed,
                prompt_change_seconds=min(
                    args.prompt_change_seconds, args.duration_seconds / 2
                ),
            )
            results.append(run_benchmark(config))
    document = {
        "schema_version": 1,
        "created_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "results": results,
    }
    rendered = json.dumps(document, indent=2, sort_keys=True)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered + "\n", encoding="utf-8")
    else:
        print(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
