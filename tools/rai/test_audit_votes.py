#!/usr/bin/env python3
"""Tests of audit_votes.py: python3 -m unittest tools/rai/test_audit_votes.py"""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from audit_votes import CLOSE_FLAG, audit, instance  # noqa: E402

VOTER = 'AA' * 32
ROOT = '01' * 32
X = '0A' * 32
Y = '0B' * 32


def signed(kind, epoch, *pairs, voter=VOTER):
    hashes = ' '.join(f'{root}:{hash_}' for root, hash_ in pairs)
    return f'SIGNED voter={voter} kind={kind} epoch={epoch} ts=1 {hashes}\n'


OPENED = 'OPENED pid=1 t=1\n'


class AuditTest(unittest.TestCase):
    def test_a_clean_log_has_no_conflict(self):
        result = audit({'pr1': [OPENED, signed('first', 0, (ROOT, X)), signed('first', 0, (ROOT, X)),
                                signed('final', 0, (ROOT, X))]})
        self.assertEqual(result['conflicts'], 0)
        self.assertEqual(result['nodes']['pr1'], dict(lifetimes=1, votes=3, hashes=3, torn_lines=0))

    def test_two_first_votes_in_one_epoch_conflict(self):
        result = audit({'pr1': [OPENED, signed('first', 0, (ROOT, X)), signed('abstain', 0, (ROOT, Y))]})
        self.assertEqual(result['conflicts'], 1)
        self.assertFalse(result['examples'][0]['across_restart'])

    def test_a_conflict_across_a_restart_is_marked(self):
        result = audit({'pr1': [OPENED, signed('final', 2, (ROOT, X)), OPENED, signed('final', 2, (ROOT, Y))]})
        self.assertEqual(result['across_restart'], 1)
        self.assertEqual(result['examples'][0]['lifetimes'], [1, 2])

    def test_other_epochs_kinds_and_rounds_are_separate(self):
        close0 = CLOSE_FLAG | (1 << 16) | 0
        close1 = CLOSE_FLAG | (1 << 16) | 1
        result = audit({'pr1': [OPENED, signed('first', 0, (ROOT, X)), signed('first', 1, (ROOT, Y)),
                                signed('final', 0, (ROOT, Y)), signed('notar', 0, (ROOT, Y)),
                                signed('first', close0, (ROOT, X)), signed('first', close1, (ROOT, Y))]})
        self.assertEqual(result['conflicts'], 0)
        self.assertEqual(instance(close1), 'close(1,1)')

    def test_a_torn_last_line_is_ignored(self):
        torn = signed('first', 0, (ROOT, Y)).rstrip('\n')
        result = audit({'pr1': [OPENED, signed('first', 0, (ROOT, X)), torn]})
        self.assertEqual(result['conflicts'], 0)
        self.assertEqual(result['nodes']['pr1']['torn_lines'], 1)


if __name__ == '__main__':
    unittest.main()
