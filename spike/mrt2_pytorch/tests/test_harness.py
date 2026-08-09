from __future__ import annotations

import unittest

from spike.mrt2_pytorch.harness import (
    PROCESSOR_REVISION,
    SOURCE_REVISION,
    RingBudget,
    RunConfig,
    latency_summary,
    percentile,
    run_benchmark,
)


class RingBudgetTests(unittest.TestCase):
    def test_prebuffer_shortfall_is_not_an_underrun(self) -> None:
        ring = RingBudget(1.5, last_time=0.0)

        ring.push(1.0, 0.5)
        ring.advance(4.0)

        self.assertFalse(ring.primed)
        self.assertEqual(ring.underrun_events, 0)
        self.assertEqual(ring.underrun_seconds, 0.0)

    def test_primed_ring_records_starvation_once_until_refilled(self) -> None:
        ring = RingBudget(1.5, last_time=0.0)
        ring.push(1.5, 0.0)

        ring.advance(2.0)
        ring.advance(3.0)

        self.assertTrue(ring.primed)
        self.assertEqual(ring.underrun_events, 1)
        self.assertAlmostEqual(ring.underrun_seconds, 1.5)


class SummaryTests(unittest.TestCase):
    def test_percentiles_interpolate_deterministically(self) -> None:
        self.assertEqual(percentile([1.0, 2.0, 3.0, 4.0], 50), 2.5)
        self.assertEqual(percentile([], 99), None)

    def test_latency_schema_reports_milliseconds(self) -> None:
        summary = latency_summary([0.001, 0.003])

        self.assertEqual(summary["count"], 2)
        self.assertEqual(summary["p50_ms"], 2.0)


class DryRunTests(unittest.TestCase):
    def _config(self, topology: str, frames: int) -> RunConfig:
        return RunConfig(
            backend="dry-run",
            topology=topology,
            frames=frames,
            duration_seconds=0.12,
            prebuffer_seconds=0.04,
            target_ahead_seconds=max(0.04, frames * 0.04),
            model="mrt2_small",
            acceleration="eager",
            guidance=True,
            dry_latency_ms=1.0,
            startup_timeout_seconds=10.0,
            worker_timeout_seconds=10.0,
            seed=109,
            prompt_change_seconds=0.02,
        )

    def test_shared_worker_exercises_two_independent_deck_states(self) -> None:
        result = run_benchmark(self._config("shared-worker", 5))

        self.assertEqual(len(result["workers"]), 1)
        self.assertGreater(result["decks"]["0"]["latency"]["count"], 0)
        self.assertGreater(result["decks"]["1"]["latency"]["count"], 0)
        self.assertTrue(result["decks"]["0"]["ring"]["primed"])

    def test_per_deck_topology_reports_two_processes_and_exact_pins(self) -> None:
        result = run_benchmark(self._config("per-deck", 25))

        self.assertEqual(len(result["workers"]), 2)
        self.assertEqual(result["pins"]["source_revision"], SOURCE_REVISION)
        self.assertEqual(result["pins"]["processor"]["revision"], PROCESSOR_REVISION)


if __name__ == "__main__":
    unittest.main()
