import unittest
from fork_diagnostic import disposition, evaluate, cached_checkpoint, retain_terminal_witnesses, installed_checkpoints_consistent

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
    def test_a_derived_final_winner_is_a_discard_witness_reported_apart(self):
        got = disposition(self.fork, 'y', self.checkpoint('FinalizedDerived'))
        self.assertEqual(got['outcome'], 'safely_discarded')
        self.assertEqual(got['witness_origin'], 'derived')
        got = disposition(self.fork, 'y', self.checkpoint('Finalized'))
        self.assertEqual(got['witness_origin'], 'certificate')

    def test_absence_and_wrong_parent_are_not_discard(self):
        c=self.checkpoint('Finalized');c['entries'][0]['previous']='different'
        self.assertEqual(disposition(self.fork, 'y', c)['outcome'], 'unresolved')
        self.assertEqual(disposition(self.fork, 'x', None)['outcome'], 'unresolved')
    def test_every_node_and_checkpoint_consistency_are_required(self):
        rows, consistent=evaluate({'x':self.fork},{n:self.checkpoint('Finalized') for n in range(6)})
        self.assertTrue(consistent); self.assertTrue(all(r['terminated_on_all_nodes'] for r in rows))
        rows, consistent=evaluate({'x':self.fork},{0:self.checkpoint('Finalized')})
        self.assertFalse(consistent); self.assertFalse(any(r['terminated_on_all_nodes'] for r in rows))

class CheckpointCacheTests(unittest.TestCase):
    def test_reuse_requires_both_installed_epoch_and_root(self):
        checkpoint = dict(epoch=2, state_hash='a', entries=[])
        cache = {(2, 'a'): checkpoint}
        self.assertIs(cached_checkpoint(dict(epoch='2', state_hash='a'), cache), checkpoint)
        for state in [dict(epoch='2'), dict(epoch='2', state_hash='b'), dict(epoch='3', state_hash='a'), {}]:
            self.assertIsNone(cached_checkpoint(state, cache))

class HistoricalTerminationTests(unittest.TestCase):
    def test_checkpoint_inclusion_stays_terminal_after_later_omission(self):
        witnesses = {}
        row = dict(hash='x', nodes={'0': {'outcome':'included', 'checkpoint_epoch':0}}, terminated_on_all_nodes=True)
        retain_terminal_witnesses([row], witnesses)
        later = dict(hash='x', nodes={'0': {'outcome':'unresolved', 'checkpoint_epoch':1}}, terminated_on_all_nodes=False)
        result = retain_terminal_witnesses([later], witnesses)[0]
        self.assertTrue(result['terminated_on_all_nodes'])
        self.assertEqual(result['nodes']['0']['checkpoint_epoch'], 0)
        self.assertTrue(result['historical_checkpoint_witness'])

class InstalledCheckpointConsistencyTests(unittest.TestCase):
    def test_historical_common_contents_do_not_mask_a_lagging_node(self):
        keys = {n: (2, 'new') for n in range(6)}
        self.assertTrue(installed_checkpoints_consistent(keys))
        keys[2] = (0, 'old')
        self.assertFalse(installed_checkpoints_consistent(keys))
        keys[2] = None
        self.assertFalse(installed_checkpoints_consistent(keys))
        del keys[2]
        self.assertFalse(installed_checkpoints_consistent(keys))
