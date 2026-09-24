import unittest
from run import compare, quantile


class PerformanceGateTests(unittest.TestCase):
    def results(self, throughput=100, latency=10):
        return [dict(pair=i, label=label, complete=True,
                     goodput=100 if label == "baseline" else throughput,
                     p99_ms=10 if label == "baseline" else latency)
                for i in range(5) for label in ("baseline", "candidate")]

    def test_failed_attempt_is_not_discarded(self):
        results = self.results()
        results[3]["complete"] = False
        self.assertEqual(compare(results, 5)["verdict"], "FAIL_COMPLETION")

    def test_goodput_regression_fails(self):
        self.assertEqual(compare(self.results(90), 5)["verdict"], "REGRESSION")

    def test_tail_latency_regression_fails(self):
        self.assertEqual(compare(self.results(latency=12), 5)["verdict"], "REGRESSION")

    def test_equal_performance_passes(self):
        self.assertEqual(compare(self.results(), 5)["verdict"], "PASS")

    def test_histogram_percentile_uses_observation_counts(self):
        self.assertEqual(quantile({"1": 99, "1000": 1}, .99), 1)
        self.assertEqual(quantile({"1": 99, "1000": 1}, 1), 1000)


if __name__ == "__main__":
    unittest.main()
