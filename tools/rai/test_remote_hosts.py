import unittest

from remote_hosts import parse_hosts, ping_ms, remote_hosts, scp_target


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


if __name__ == '__main__':
    unittest.main()
