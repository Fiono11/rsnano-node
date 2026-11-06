import tempfile
import unittest
from pathlib import Path

from remote_hosts import cached_rtt_matrix, longest_round_trip_ms, parse_hosts, ping_ms, remote_hosts, scp_target


class ParseHosts(unittest.TestCase):
    def test_a_line_per_node_with_comments_and_local_nodes(self):
        hosts = parse_hosts('# PR0 is the controller\n10.128.0.2 local\n\n10.142.0.3  # us-east1\nfd00::7\n')
        self.assertEqual(hosts, [dict(address='10.128.0.2', remote=False),
                                 dict(address='10.142.0.3', remote=True),
                                 dict(address='fd00::7', remote=True)])
        self.assertEqual(remote_hosts(hosts + [dict(address='10.142.0.3', remote=True)]), ['10.142.0.3', 'fd00::7'])

    def test_rejects_names_and_unknown_words(self):
        with self.assertRaisesRegex(ValueError, 'line 1: node-1 is not an IP address'):
            parse_hosts('node-1\n')
        with self.assertRaisesRegex(ValueError, 'line 2'):
            parse_hosts('10.0.0.1\n10.0.0.2 remote\n')


class Helpers(unittest.TestCase):
    def test_brackets_ipv6_for_scp(self):
        self.assertEqual(scp_target('10.0.0.1', '/tmp/x'), '10.0.0.1:/tmp/x')
        self.assertEqual(scp_target('fd00::7', '/tmp/x'), '[fd00::7]:/tmp/x')

    def test_reads_the_average_of_linux_and_macos_ping(self):
        self.assertEqual(ping_ms('5 packets transmitted, 5 received, 0% packet loss, time 811ms\n'
                                 'rtt min/avg/max/mdev = 101.022/101.417/102.010/0.351 ms\n'), 101.417)
        self.assertEqual(ping_ms('round-trip min/avg/max/stddev = 0.040/0.081/0.130/0.031 ms\n'), 0.081)
        self.assertIsNone(ping_ms('5 packets transmitted, 0 received, 100% packet loss\n'))

    def test_longest_round_trip_skips_lost_pings_and_rounds_up(self):
        self.assertEqual(longest_round_trip_ms([[0.0, 124.2, None], [124.2, 0.0, 313.45], [None, 313.7, 0.0]]), 314)
        self.assertEqual(longest_round_trip_ms([[0.0, None], [None, 0.0]]), 0)


class CachedRttTest(unittest.TestCase):
    def test_measures_once_and_slices_to_the_run(self):
        calls = []

        def measure(hosts):
            calls.append(len(hosts))
            return [[float(10 * i + j) for j in range(len(hosts))] for i in range(len(hosts))]

        with tempfile.TemporaryDirectory() as temporary:
            hosts = Path(temporary) / 'hosts.txt'
            hosts.write_text('10.0.0.1\n10.0.0.2\n10.0.0.3\n')

            first = cached_rtt_matrix(hosts, 2, measure)
            again = cached_rtt_matrix(hosts, 3, measure)

            self.assertEqual(calls, [3])
            self.assertEqual(first, [[0.0, 1.0], [10.0, 11.0]])
            self.assertEqual(len(again), 3)

    def test_a_cache_smaller_than_the_run_is_measured_again(self):
        def measure(hosts):
            return [[0.0] * len(hosts) for _ in hosts]

        with tempfile.TemporaryDirectory() as temporary:
            hosts = Path(temporary) / 'hosts.txt'
            hosts.write_text('10.0.0.1\n10.0.0.2\n')
            hosts.with_suffix('.rtt.json').write_text('[[0.0]]')

            self.assertEqual(len(cached_rtt_matrix(hosts, 2, measure)), 2)


if __name__ == '__main__':
    unittest.main()
