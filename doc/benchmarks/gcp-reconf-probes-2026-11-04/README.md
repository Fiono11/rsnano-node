# One committee change across seven regions: diagnosis runs (Nov 4 2026)

RAI, node 33b60a81 (rai_multihost 078063f88), the binary of `../gcp-matrix-45k-2026-11-03`,
45,000 blocks, 10% forks, f = p = 1, 8 s epochs, six seats. A seventh VM, PR6 in Toronto,
starts as a standby (`--prs 7 --standby 1`). With `--rotation '0:2>6'` Montréal (PR2) hands
its weight to PR6 and the new committee counts from epoch 3. Logs and results are gzipped.

| Run | Byzantine member | Committee change | Rate | Result |
|---|---|---|---|---|
| `smoke-reconf-fork10-byz-rai` | yes | yes | 1000/s | **not settled**: closes 4, 3, 4, 7, 10, 18 s, epoch 6 never closes |
| `probe-standby-byz-1000` | yes | no | 1000/s | settled, forks resolved; p50 399, p95 581, p99 1902 ms; close max 16.3 s |
| `probe-reconf-honest-2000` | no | yes | 2000/s | **not settled**: epoch 2 closes in 18.6 s, epoch 3 never closes |
| `probe-reconf-honest-1000` | no | yes | 1000/s | **settled**, forks resolved; p50 366, p95 474, p99 589 ms; close max 3.9 s |

The seventh node alone does not cause the collapse; the committee change does at these
loads: honest at 2000/s and with a Byzantine member at 1000/s. Honest at 1000/s it completes
with no flood (elections put into requests 32-57 k per node, PR6 30 k `confirm_ack`s), so the
collapse depends on load. In all of them PR6's key replaces PR2's.

Per node, PR0 to PR6:

| Counter | `probe-standby-byz-1000` | `probe-reconf-honest-2000` | `probe-reconf-honest-1000` |
|---|---|---|---|
| elections put into requests | 90-461 k | 0.94-3.4 M (PR6 3.4 M) | 32-57 k |
| `confirm_req` received | 8.8-9.1 k | 67-75 k | 1.9-3.7 k |
| `confirm_ack` received | 38-226 k | 20-312 k, PR6 **1.32 M** | 10-37 k |
| vote processor overfill | 0-178 k | 55-268 k, PR6 **1.24 M** | 0-10 k |
| aggregator overfill (hashes) | 0-148 k | 1.1-14 M | 0 |

In `probe-reconf-honest-2000` the close of epoch 2 spends 16 s in failed rounds: PR0 needs
7.6 s to fetch and validate the round 0 leader's manifest, round 1 (led by PR6) passes
5.4 s with no value, round 2 decides. PR6 closes epoch 2 about 10 s after the others.

Not established: why the new member receives this many votes. These are single runs; the
counters are totals for a run, so they do not show when the flood starts.
