import unittest
from fork_diagnostic import disposition, evaluate

class ForkOutcomeTests(unittest.TestCase):
    fork = dict(account='a', previous='p', primary='x', alternative='y')
    def checkpoint(self, status='Recovery'):
        return dict(epoch=2, state_hash='checkpoint', entries=[dict(account='a', previous='p', height=3, hash='x', status=status)])
    def test_recovery_inclusion_is_terminal_without_finalization(self):
        self.assertEqual(disposition(self.fork, 'x', self.checkpoint())['outcome'], 'included')
        self.assertEqual(disposition(self.fork, 'y', self.checkpoint())['outcome'], 'unresolved')
    def test_finalized_conflict_is_a_discard_witness(self):
        got = disposition(self.fork, 'y', self.checkpoint('Finalized'))
        self.assertEqual(got['outcome'], 'safely_discarded')
        self.assertEqual(got['witness'], 'x')
    def test_absence_and_wrong_parent_are_not_discard(self):
        c=self.checkpoint('Finalized');c['entries'][0]['previous']='different'
        self.assertEqual(disposition(self.fork, 'y', c)['outcome'], 'unresolved')
        self.assertEqual(disposition(self.fork, 'x', None)['outcome'], 'unresolved')
    def test_every_node_and_checkpoint_consistency_are_required(self):
        rows, consistent=evaluate({'x':self.fork},{n:self.checkpoint('Finalized') for n in range(6)})
        self.assertTrue(consistent); self.assertTrue(all(r['terminated_on_all_nodes'] for r in rows))
        rows, consistent=evaluate({'x':self.fork},{0:self.checkpoint('Finalized')})
        self.assertFalse(consistent); self.assertFalse(any(r['terminated_on_all_nodes'] for r in rows))
