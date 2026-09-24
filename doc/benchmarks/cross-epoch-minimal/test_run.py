import unittest
import tempfile
import json
from pathlib import Path
from run import compare, quantile, settled, baseline_timeout, prune_run_data


class PerformanceGateTests(unittest.TestCase):
    def results(self, throughput=100, latency=10):
        return [dict(pair=i, label=label, complete=True,
                     goodput=100 if label == "baseline" else throughput,
                     p50_ms=10 if label == "baseline" else latency,
                     p95_ms=10 if label == "baseline" else latency,
                     p99_ms=10 if label == "baseline" else latency)
                for i in range(5) for label in ("baseline", "candidate")]

    def test_timeout_is_derived_only_from_baseline_wall_times(self):
        seconds, basis = baseline_timeout([100, 102.1, 99], 1.5, 240)
        self.assertEqual(seconds, 154)
        self.assertEqual(basis["baseline_count"], 3)
        self.assertEqual(baseline_timeout([], 1.5, 240)[0], 240)

    def test_failed_attempt_is_not_discarded(self):
        results = self.results()
        results[3]["complete"] = False
        self.assertEqual(compare(results, 5)["verdict"], "FAIL_COMPLETION")

    def test_goodput_regression_fails(self):
        self.assertEqual(compare(self.results(90), 5)["verdict"], "REGRESSION")

    def test_primary_latency_regression_fails(self):
        self.assertEqual(compare(self.results(latency=12), 5)["verdict"], "REGRESSION")

    def test_each_primary_percentile_can_fail_independently(self):
        for percentile in (50, 95):
            results = self.results()
            for result in results:
                if result["label"] == "candidate": result[f"p{percentile}_ms"] = 12
            self.assertEqual(compare(results, 5)["verdict"], "REGRESSION")

    def test_p99_alone_is_diagnostic(self):
        results = self.results()
        for result in results:
            if result["label"] == "candidate": result["p99_ms"] = 100
        comparison = compare(results, 5)
        self.assertEqual(comparison["verdict"], "PASS")
        self.assertEqual(comparison["diagnostic_only"]["p99_ratio_ci95"], [10, 10])

    def test_equal_performance_passes(self):
        self.assertEqual(compare(self.results(), 5)["verdict"], "PASS")

    def test_histogram_percentile_uses_observation_counts(self):
        self.assertEqual(quantile({"1": 99, "1000": 1}, .99), 1)
        self.assertEqual(quantile({"1": 99, "1000": 1}, 1), 1000)

    def test_lagging_peer_is_not_network_wide_completion(self):
        states = [{"block_count": {"count": "100", "cemented": str(n)},
                   "final_state": {"hash": "same", "pending": "0"}} for n in [100, 99]]
        self.assertFalse(settled(states))
        states[1]["block_count"]["cemented"] = "100"
        self.assertTrue(settled(states))

    def test_settlement_failure_is_not_a_performance_pass(self):
        results = self.results()
        results[3]["settled_consistent"] = False
        self.assertEqual(compare(results, 5)["verdict"], "FAIL_SETTLEMENT")




class DataCleanupTests(unittest.TestCase):
    def test_success_keeps_evidence_and_config_but_removes_data(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            node = root / "data" / "pr0"
            node.mkdir(parents=True)
            (node / "data.ldb").write_bytes(b"test database")
            (node / "config-node.toml").write_text("test = true\n")
            for name in ("result.json", "rpc.json", "run.log"):
                (root / name).write_text("{}")
            result = dict(complete=True, settled_consistent=True)
            prune_run_data(root, result)
            self.assertFalse((root / "data").exists())
            self.assertTrue((root / "result.json").exists())
            self.assertEqual((root / "saved-config/pr0/config-node.toml").read_text(), "test = true\n")
            self.assertEqual(len(json.loads((root / "data-cleanup.json").read_text())["files"]), 2)
            prune_run_data(root, result)  # repeat is harmless

    def test_running_process_or_missing_evidence_preserves_database(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "data").mkdir()
            database = root / "data/data.ldb"
            database.write_bytes(b"diagnostic evidence")
            prune_run_data(root, dict(cleanup_error="still running"))
            self.assertTrue(database.exists())
            with self.assertRaises(ValueError):
                prune_run_data(root, dict(complete=False, timed_out=True))
            self.assertTrue(database.exists())
            for name in ("result.json", "rpc.json", "run.log"):
                (root / name).write_text("{}")
            prune_run_data(root, dict(complete=False, timed_out=True))
            self.assertFalse(database.exists())


if __name__ == "__main__":
    unittest.main()
