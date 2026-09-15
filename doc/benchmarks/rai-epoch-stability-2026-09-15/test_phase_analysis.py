import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('phase_analysis', Path(__file__).with_name('phase_analysis.py'))
phase = importlib.util.module_from_spec(spec)
spec.loader.exec_module(phase)
US = 1_000_000
ORIGIN = 1_000_000_000


def fixture(publications, outcomes, cutoff=40, anchor_width=0):
    timeline = {'schema_version': 1, 'origin': 'first_publication', 'unit': 'microseconds',
                'first_publication_unix_us_lower_bound': ORIGIN,
                'first_publication_unix_us_upper_bound': ORIGIN + anchor_width,
                'offset_rounding_error_us': 1, 'observation_cutoff_offset_us': cutoff * US,
                'publications': [[int(t * US), fork] for t, fork in publications],
                'outcomes': [[index, epoch, *[None if v is None else int(v * US) for v in rest]]
                             for index, epoch, *rest in outcomes]}
    markers = [{'kind': kind, 'pid': 42, 'epoch': 0, 'unix_us': ORIGIN + when * US}
               for kind, when in zip(phase.MARKERS, (10, 20, 22))]
    return timeline, markers


class PhaseAnalysisTests(unittest.TestCase):
    def test_crossing_work_is_distinct_from_post_close_publications(self):
        timeline, markers = fixture(
            [(15, False), (5, False), (25, False), (17, False), (16, True), (18, False)],
            [(0, 1, 16, 24, .1, .2),
             (1, 0, 6, None, .1, None), (1, 1, 14, 24, .1, .2),
             (2, 1, 25.1, 26, .1, .2), (3, 1, 18, None, .1, None),
             (4, 1, 17, 23, .1, .2)])
        # Duplicate delivery must not inflate an already exported observation.
        timeline['outcomes'].append(timeline['outcomes'][0][:])
        report = phase.analyze_timeline(timeline, markers, 42)
        pub = report['publication_cohorts']
        receipt = report['receipt_phases']
        self.assertEqual(pub['during_closing']['fresh']['observed_roots'], 2)
        self.assertEqual(pub['after_complete']['fresh']['observed_roots'], 1)
        self.assertEqual(receipt['after_complete']['fresh']['finalization']['count'], 2)
        self.assertEqual(receipt['after_complete']['carried']['finalization']['count'], 1)
        self.assertAlmostEqual(receipt['after_complete']['fresh']['finalizations_per_observed_second'], 2 / 18)
        pending = report['work_crossing_boundary']['cleanup_complete']
        self.assertEqual(pending['fresh']['published_before_finalized_after']['observed_roots'], 1)
        self.assertEqual(pending['fresh']['published_before_without_finalization_by_cutoff'], 1)
        self.assertEqual(pending['carried']['published_before_finalized_after']['observed_roots'], 1)
        self.assertEqual(report['offered_publications']['during_closing']['without_any_observed_outcome'], 1)
        self.assertEqual(report['post_close_publication_tail']['definitely_post_complete_nonfork_publications'], 1)
        # A late receipt from an early publication must not enter a late latency cohort.
        matrix = report['publication_to_receipt_phases']
        crossing = next(row for row in matrix if row['cohort'] == 'fresh' and row['publication_phase'] == 'during_closing' and row['finalization_phase'] == 'after_complete')
        self.assertEqual(crossing['finalization']['mean_ms'], 9000)

    def test_empty_post_close_publication_tail_is_not_zero_capacity(self):
        timeline, markers = fixture([(15, False)], [(0, 1, 16, 25, .1, .2)], cutoff=60)
        report = phase.analyze_timeline(timeline, markers, 42)
        tail = report['post_close_publication_tail']
        self.assertEqual(tail['last_publication_minus_cleanup_complete_seconds_bounds'], [0, 0])
        self.assertEqual(tail['assessment'], 'no observed publications after cleanup complete')
        self.assertEqual(report['publication_cohorts']['after_complete']['fresh']['observed_roots'], 0)
        received = report['receipt_phases']['after_complete']['fresh']
        self.assertEqual(received['finalization']['count'], 1)
        self.assertAlmostEqual(received['finalizations_per_observed_second'], 1 / 38)
        self.assertIn('not steady capacity', received['rate_scope'])
        late = next(w for w in report['full_windows']['5s'] if w['start_seconds'] == 25)
        self.assertFalse(late['entirely_within_publication_span'])
        self.assertEqual(late['cohorts']['fresh']['receipt']['finalization']['count'], 1)
        self.assertEqual(late['cohorts']['fresh']['publication']['observed_roots'], 0)

    def test_anchor_uncertainty_and_cleanup_are_separate(self):
        timeline, markers = fixture([(19.999999, False), (21, False), (22, False)],
                                   [(0, 1, 20.5, 21, .1, .2),
                                    (1, 1, 21.1, 21.5, .1, .2),
                                    (2, 1, 22.1, 23, .1, .2)], anchor_width=2)
        report = phase.analyze_timeline(timeline, markers, 42)
        self.assertEqual(report['publication_cohorts']['boundary_uncertain']['fresh']['observed_roots'], 1)
        self.assertEqual(report['publication_cohorts']['post_persist_cleanup']['fresh']['observed_roots'], 1)
        self.assertEqual(report['publication_cohorts']['after_complete']['fresh']['observed_roots'], 1)
        self.assertEqual(report['receipt_phases']['post_persist_cleanup']['fresh']['finalization']['count'], 2)
        self.assertEqual(report['phase_durations']['post_persist_cleanup']['observed_duration_seconds'], 2)
        mixed = next(w for w in report['full_windows']['5s'] if w['start_seconds'] == 20)
        self.assertEqual(mixed['phase'], 'boundary_uncertain')

    def test_wrong_pid_and_conflicting_rows_are_rejected(self):
        timeline, markers = fixture([(15, False)], [(0, 1, 16, 17, .1, .2)])
        with self.assertRaisesRegex(ValueError, 'Expected one EPOCH_COUNT_REACHED'):
            phase.analyze_timeline(timeline, markers, 99)
        timeline['outcomes'].append([0, 1, 16 * US, 18 * US, 100_000, 200_000])
        with self.assertRaisesRegex(ValueError, 'Conflicting duplicate'):
            phase.analyze_timeline(timeline, markers, 42)

    def test_marker_parser_does_not_infer_legacy_pid(self):
        text = ('EPOCH_CLOSED {"epoch":0,"hash":"legacy"}\n'
                'prefix EPOCH_CLOSED {"epoch":0,"pid":42,"unix_us":123} trailing\n'
                'EPOCH_CLOSE_COMPLETE {broken}\n')
        records, malformed = phase.parse_markers(text)
        self.assertEqual(records, [{'kind': 'EPOCH_CLOSED', 'epoch': 0, 'pid': 42, 'unix_us': 123}])
        self.assertEqual(malformed, ['EPOCH_CLOSE_COMPLETE'])

    def test_local_recovery_can_still_be_network_closing_work(self):
        timeline, local = fixture([(15, False)], [(0, 1, 16, 25, .1, .2)])
        mapping = {str(pr): 42 + pr for pr in range(6)}
        markers = [dict(row, pid=pid) for pid in mapping.values() for row in local]
        for row in markers:
            if row['pid'] == 47:
                row['unix_us'] = ORIGIN + {'EPOCH_COUNT_REACHED': 9,
                                          'EPOCH_CLOSED': 28,
                                          'EPOCH_CLOSE_COMPLETE': 30}[row['kind']] * US
        local_report = phase.analyze_timeline(timeline, markers, 42)
        network = phase.analyze_network(timeline, markers, mapping)
        self.assertEqual(network['status'], 'complete')
        self.assertEqual(local_report['receipt_phases']['after_complete']['fresh']['finalization']['count'], 1)
        self.assertEqual(network['analysis']['receipt_phases']['during_closing']['fresh']['finalization']['count'], 1)
        self.assertEqual(network['analysis']['receipt_phases']['after_complete']['fresh']['finalization']['count'], 0)
        self.assertEqual(network['per_pr_markers']['5']['markers']['EPOCH_CLOSE_COMPLETE'][0]['unix_us'], ORIGIN + 30 * US)

    def test_missing_network_marker_is_unknown_not_local_fallback(self):
        timeline, markers = fixture([(15, False)], [(0, 1, 16, 25, .1, .2)])
        mapping = {str(pr): 42 + pr for pr in range(6)}
        incomplete = phase.analyze_network(timeline, markers, mapping)
        self.assertEqual(incomplete['status'], 'unknown')
        self.assertNotIn('analysis', incomplete)
        self.assertEqual(len(incomplete['missing_or_ambiguous']), 15)
        self.assertEqual(phase.analyze_network(timeline, markers, {'0': 42})['status'], 'unknown')


if __name__ == '__main__':
    unittest.main()
