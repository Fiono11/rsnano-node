# Kudzu step 1 (`rai_protocol`), 6 PRs, 2000 blocks/s, 45k blocks (2026-09-17)

Benchmark of the Kudzu voting rules (Protocol 1 of the Kudzu paper) replacing the legacy
tally-based election, behind the `rai_protocol` feature. Same command and machine as the
develop baseline in `../baseline-develop-2000-45k-2026-09-17`.

## Setup

| | |
|---|---|
| Base commit | `ff2b32710` (develop) + uncommitted step-1 changes |
| Build | `cargo build --release -p rsnano_cli -p nanospam --features rsnano_cli/rai_protocol` |
| Command | `nanospam --prs 6 --no-prio --blocks 45000 --accounts 45000 --rate 2000 --no-kill` |
| Node config | nanospam defaults (AEC size 5000, bounded backlog off, LMDB nosync_unsafe, voting on) |
| Thresholds | f = p = 19 % of the online weight: certificate (n − f − p) 62 %, fast (n − p) 81 %, many (f + p + 1) 38 % + 1 raw. With 6 equal PRs: 4 for a certificate, 5 for fast finalization |
| Machine | Apple M1, 8 cores (4P+4E), 16 GB; idle before start |
| Total wall time incl. node start-up / wallet setup | 64 s (baseline 79 s) |

## Headline (nanospam)

| Metric | Kudzu step 1 | develop baseline |
|---|---|---|
| Blocks confirmed | 45,000 / 45,000 | 45,000 / 45,000 |
| Time publish → last confirmation | 23.56 s | 24.51 s |
| Confirmation rate | **1910 cps** | 1835 cps |
| Average confirmation time | **110 ms** | 205 ms |
| Steady state (t ≥ 5 s) | ~1,850–2,000 cps, ~95–105 ms | ~1,900–2,000 cps, ~190–200 ms |

## Per-second timeline (nanospam status line)

| t (s) | confirmed | cps | avg conf (ms) |
|---|---|---|---|
| 1 | 142 | 154 | 102 |
| 2 | 2,422 | 2,276 | 111 |
| 3 | 4,877 | 2,328 | 184 |
| 4 | 7,745 | 2,863 | 162 |
| 5 | 9,631 | 1,884 | 105 |
| 6 | 11,562 | 1,927 | 96 |
| 7 | 13,567 | 2,002 | 99 |
| 8 | 15,428 | 1,859 | 95 |
| 9 | 17,336 | 1,906 | 98 |
| 10 | 19,333 | 1,994 | 96 |
| 11 | 21,223 | 1,887 | 97 |
| 12 | 23,087 | 1,853 | 95 |
| 13 | 25,020 | 1,930 | 120 |
| 14 | 26,939 | 1,917 | 104 |
| 15 | 28,621 | 1,680 | 102 |
| 16 | 30,755 | 2,129 | 120 |
| 17 | 32,669 | 1,912 | 101 |
| 18 | 34,625 | 1,953 | 101 |
| 19 | 36,500 | 1,873 | 102 |
| 20 | 38,421 | 1,919 | 101 |
| 21 | 40,383 | 1,959 | 95 |
| 22 | 42,318 | 1,933 | 95 |
| 23 | 44,281 | 1,960 | 105 |

## Node state at exit (RPC)

All six PRs: `count` 45053, `cemented` 45053, `unchecked` 0, `confirmation_active.unconfirmed` 0.

## Node counters at exit (`stats` counters, dir=in)

| counter | PR0 | PR1 | PR2 | PR3 | PR4 | PR5 | baseline (PR0) |
|---|---|---|---|---|---|---|---|
| `active_elections.started` | 45,072 | 45,108 | 45,114 | 45,118 | 45,133 | 45,140 | 45,229 |
| `active_elections.confirmed` | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 |
| `active_elections.terminated` | 45,057 | 45,056 | 45,060 | 45,053 | 45,058 | 45,064 | – |
| `active_elections.settled` | 44,481 | 44,913 | 44,983 | 45,016 | 45,040 | 44,726 | – |
| `active_elections.finalized_fast` | 44,172 | 44,555 | 44,739 | 44,751 | 44,811 | 44,470 | – |
| `active_elections.finalized_final` | 880 | 497 | 313 | 301 | 241 | 582 | – |
| `active_elections_dropped.priority` | 20 | 56 | 62 | 66 | 81 | 88 | 176 |
| `election.vote` | 240,159 | 239,368 | 238,691 | 238,502 | 238,331 | 238,393 | 489,855 |
| `election.generate_vote_normal` (First) | 44,795 | 45,014 | 44,903 | 45,042 | 45,049 | 44,910 | – |
| `election.generate_vote_final` | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 | – |
| `election.generate_vote_notar` / `_timeout` | 0 | 0 | 0 | 0 | 0 | 0 | – |
| `election.confirmation_request` | 12,569 | 12,035 | 12,463 | 12,644 | 12,450 | 12,772 | 26,340 |
| `election_vote.replayed` | 7,110 | 3,290 | 3,111 | 3,261 | 2,547 | 4,866 | 4,330 |
| `message.confirm_ack` | 3,755 | 3,674 | 3,678 | 3,708 | 3,704 | 3,678 | 3,862 |
| `message.confirm_req` | 608 | 534 | 542 | 545 | 523 | 546 | 891 |
| `confirming_set.cemented` | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 |

Observations:

- 98–99.5 % of the elections were fast finalized (five or six First votes), the rest by a
  finalization certificate. Every election terminated by a notarization certificate; no timeout or
  Notar votes were generated (non-fork traffic).
- Vote traffic halved versus the baseline: no periodic non-final re-votes and one voting round for
  almost every block.
- Elections evicted from the AEC (`active_elections_dropped.priority`) stayed low, so the
  per-bucket cap variance described in the baseline README did not show; it still applies.
- `settled` < `terminated`: an election whose certificate includes a non-first vote (e.g. a Final vote
  that arrived before that rep's First vote) is not provably settled until it is finalized.

## Notes on the implementation exercised here

Two rules turned out to be essential for the tail of a run (without them ~150 roots per node
stayed one final vote short): a node whose election fast finalizes still broadcasts its exit
FinalVote (Protocol 1, line 11) even though the election is erased at once, and terminated but not
yet finalized elections keep being solicited so that the legacy final-vote-on-request path serves
replicas which missed a First vote.

## Files

- `run.log` – full nanospam log (ANSI stripped) followed by per-node `block_count`,
  `confirmation_active`, `stats` counters and objects, and data-dir sizes.
- `run_one.sh` – the script used (identical to the baseline's).
