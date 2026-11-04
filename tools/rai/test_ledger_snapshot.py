import tempfile
import unittest
from pathlib import Path

from ledger_snapshot import check_manifest, prune_node_dir, replaced_data_dir


class LedgerSnapshotTest(unittest.TestCase):
    def test_a_matching_manifest_passes(self):
        check_manifest(dict(node_sha256='a', nodes=6, setup_seconds=9), dict(node_sha256='a', nodes=6))

    def test_another_binary_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, 'node_sha256'):
            check_manifest(dict(node_sha256='a', nodes=6), dict(node_sha256='b', nodes=6))

    def test_only_the_ledger_and_wallets_stay(self):
        with tempfile.TemporaryDirectory() as temporary:
            node = Path(temporary)
            for name in ('data.ldb', 'data.ldb-lock', 'wallets.ldb', 'config-node.toml', 'diagnostics.log', 'node.pid'):
                (node / name).write_text('')

            prune_node_dir(node)

            self.assertEqual(sorted(p.name for p in node.iterdir()), ['data.ldb', 'wallets.ldb'])

    def test_the_data_dir_is_replaced(self):
        command = ['nanospam', '--data-dir', '/a', '--prs', '6']
        self.assertEqual(replaced_data_dir(command, '/b'), ['nanospam', '--data-dir', '/b', '--prs', '6'])


if __name__ == '__main__':
    unittest.main()
