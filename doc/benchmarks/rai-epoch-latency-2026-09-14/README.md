# RAI epoch-to-epoch latency benchmark, 2026-09-14

Goal: minimum latency and maximum throughput of non-fork elections, stable across epochs. Fork
termination and epoch-close duration are secondary.

Host: 8-core Apple M1 (4 performance + 4 efficiency cores), macOS 14.6.1, six PRs plus nanospam on one
machine. Workload for every run:

```sh
nanospam --prs 6 --no-prio --fork-percentage 5 --blocks 50000 --accounts 50000 --rate R \
  --epoch-terminated-elections 25000 --closed-epochs 2 --close-timeout 240
```

Baseline = branch `rai_close_epochs` at `cfd2d8a34` (release, `rai_protocol`). Final = the same commit plus
the uncommitted changes listed below. Fresh node data was deleted after every run. Latencies are
publication to PR0 WebSocket outcome, milliseconds; "e0"/"e1" are epochs 0 and 1 (25,000 terminated
elections each).

## Result

| Rate | Build | Non-fork finalization e0 mean / p95 | e1 mean / p95 | e1 / e0 | Node CPU while publishing |
|---:|---|---:|---:|---:|---:|
| 1000 | baseline | 105 / 168 | 521 / 887 | 5.0 | 95% |
| 1000 | final | 98 / 124 | 123 / 184 | 1.3 | 58% |
| 1000 | final, `--vote-generator-delay-ms 50` | 58 / 76 | 79 / 136 | 1.4 | 68% |
| 1500 | final | 108 / 154 | 173 / 360 | 1.6 | 69% |
| 1500 | final, `--vote-generator-delay-ms 50` | 73 / 137 | 313 / 1070 | 4.3 | 91% |
| 2000 | final | 216 / 469 | 2010 / 3407 | 9.3 | 97% |
| 2000 | final, `--vote-generator-delay-ms 50` | 259 / 478 | 2709 / 4649 | 10.5 | 89% |

Yesterday's runs of the same commit (see `../rai-reconciliation-2026-09-14`) had e1 means of 1,137 ms at
1000, 20,945 ms at 1500 and 7,322 ms at 2000 blocks/s. Confirmations per second reported by nanospam over
its 60 s window stay at ~790 for every run because the window includes setup; while publishing, PR0
confirms ~940/s at 1000 and ~1,550/s at 2000 blocks/s.

At 2000 blocks/s the host is out of CPU: elections start at ~1,950/s but confirm at ~1,550/s, so the backlog
and therefore latency grow for the whole of epoch 1 (windows: 0.9 s, 1.6 s, 2.4 s, 3.0 s). That is capacity,
not an epoch effect; epoch 0 only looks better because the backlog is still small.

## What caused the epoch-1 degradation

Profiles of PR0 (`sample`, 5 s at 1 ms) in each phase plus per-5 s deltas of the node's stats counters:

1. **Closer tick scan.** `EpochCloser::tick` (every 100 ms) computed close readiness on every tick, even
   with no drain in progress: it built a SipHash set of every election of the epoch plus a scan of the
   signing state, while holding the vote-state mutex and the AEC read lock. The cost grew through the epoch
   and doubled in epoch 1 because both maps keep the closed epoch's entries. Vote signing, recovery replies
   and the scheduler all queued behind it (26% of PR0 CPU plus lock waits in epoch 1).
2. **Solicitation feedback loop.** `ConfirmReqSender` requested every election on its first Active tick.
   Under RAI each requested hash was pushed to both vote generators and re-verified through LMDB (~70 us per
   hash; 450,505 generated hashes for 50,000 blocks on PR0, i.e. nine per block) although every PR already
   pushes its votes proactively. Slower elections were solicited more, which cost more CPU, which made
   elections slower: requested hashes per second went from ~2,000 in epoch 0 to ~6,000 in epoch 1.
3. **AEC voter rescans.** The 20 ms voter tick created a Final vote target for every notarized fork, the
   signing filter rejected it as unfinalizable, and it came back on the next tick (32% of PR0 CPU in epoch 1
   once the other costs were gone).
4. **Queue caps.** The inbound message queue (64 messages per channel) dropped 2-6% of publishes on every
   PR because nanospam sends bursts over four connections; the vote processor queue (256 votes per PR)
   overflowed at 2000 blocks/s during the close commit.

The AEC size (5,000) was never reached in any run (`election_scheduler/activate_full` stayed 0), so
raising it does not help. The machine saturates before the container does.

## Changes

- `node/src/consensus/epoch_closer.rs`, `vote_generators.rs`, `active_elections_container.rs`: readiness is
  computed only while draining; the local FIRST list is frozen when the drain begins (no FIRST vote can be
  signed in a draining epoch); `pending_epoch_drain` examines only undecided elections; candidate object
  validity is cached once established.
- `node/src/consensus/confirm_req_sender.rs`: under RAI the first confirmation request for an election waits
  one base latency (1 s); later requests keep the existing interval.
- `node/src/consensus/vote_generation/request_aggregator.rs`: hashes whose signed votes the recovery reply
  just delivered are not regenerated.
- `node/src/consensus/vote_generation/aec_voter.rs`, `kudzu_vote_state.rs`: the vote state tracks roots with
  more than one notarized candidate; the voter creates no Final target for them.
- `node/src/consensus/active_elections/root_container.rs`, `active_elections_container.rs`: elections of a
  closed epoch leave the scheduler buckets at the close.
- `node/src/consensus/vote_processor.rs`: `max_pr_queue` 4096 under RAI (was 256).
- Vote-path maps switched from SipHash to FxHash (vote router, per-vote results, election candidate and
  vote maps, terminated set, certificate recovery).
- `tools/nanospam/src/setup/node_config_factory.rs`: `[node.message_processor] max_queue = 1024`.

Unit validation: 1,612 default-library tests, 688 RAI node tests, 600 default node tests and 36 nanospam
tests pass. Two legacy integration tests in `node/tests/tests/vote_cache.rs` fail under `rai_protocol`
both before and after these changes.

## Rounds

Each round adds to the previous one; `d50` = `--vote-generator-delay-ms 50`. "close0 4th/6th" = seconds
from the last PR reaching the count until the 4th and 6th PR closed epoch 0, with the round number.

```
run          cps  mean |  e0 fin mean/p95 |  e1 fin mean/p95 | e1/e0 | fork e0/e1 |  close0 4th/6th |          close1 | pub drop vote drop | cpu%/node
base1000     791   331 |     105 /    168 |     521 /    887 |   5.0 |  247 /  741 |       11/11s r2 |         8/8s r2 |    10576         0 |        95
fix1000      792   268 |      92 /    134 |     416 /    887 |   4.5 |  251 /  565 |         8/8s r1 |         6/8s r1 |    14700         0 |        92
fix2-1000    790   121 |     101 /    127 |     134 /    199 |   1.3 | 1122 / 1220 |       24/40s r3 |       10/30s r3 |    16859         0 |        76
fix3-1000    790   118 |      96 /    121 |     130 /    331 |   1.4 | 1051 / 1277 |       10/10s r2 |       29/44s r3 |        0         0 |        69
fix4-1000    791   103 |      93 /    117 |     106 /    144 |   1.1 | 1031 / 1207 |       10/17s r2 |       44/54s r3 |        0         0 |        65
fix5-1000    791   106 |      96 /    121 |     109 /    145 |   1.1 | 1073 / 1179 |        8/10s r1 |      95/100s r5 |        0         0 |        61
fix6-1000    791   114 |      98 /    124 |     123 /    184 |   1.3 | 1182 / 1249 |       13/16s r2 |     160/165s r5 |        0         0 |        58
fix6d50-1000  789    71 |      58 /     76 |      79 /    136 |   1.4 | 1016 / 1141 |       22/22s r3 |       44/54s r4 |        0         0 |        68
fix2-1500    789   215 |     111 /    180 |     270 /    798 |   2.4 | 1081 / 1234 |       10/10s r2 |       22/22s r3 |     7322         0 |        87
fix3-1500    790   169 |     110 /    161 |     201 /    395 |   1.8 | 1065 / 1232 |       10/53s r2 |         0/5s r3 |        0         0 |        82
fix4-1500    790   202 |     116 /    206 |     243 /    617 |   2.1 | 1109 / 1298 |       10/10s r2 |       22/25s r3 |        0         0 |        86
fix5-1500    789   221 |     110 /    159 |     277 /    729 |   2.5 | 1130 / 1279 |       12/15s r2 |       23/25s r3 |        0         0 |        84
fix6-1500    789   151 |     108 /    154 |     173 /    360 |   1.6 | 1060 / 1195 |        8/10s r1 |      90/101s r5 |        0         0 |        69
fix4d50-1500  791   172 |      73 /    129 |     234 /    452 |   3.2 | 1057 / 1417 |         9/9s r2 |       22/26s r3 |        0         0 |        82
fix6d50-1500  791   226 |      73 /    137 |     313 /   1070 |   4.3 | 1080 / 1319 |       10/11s r2 |       10/17s r2 |        0         0 |        91
fix2-2000    787  2122 |     256 /    571 |    3549 /   6516 |  13.9 | 1367 / 5157 |       11/57s r2 |      -33/31s r1 |     6086      3108 |        88
fix3-2000    790  1501 |     257 /    675 |    2303 /   3948 |   9.0 | 1451 / 3827 |       11/16s r1 |         4/9s r0 |        0         0 |       104
fix4-2000    791  1328 |     176 /    368 |    2014 /   2858 |  11.5 | 1752 / 3503 |       27/27s r3 |        1/11s r0 |        0         0 |        82
fix5-2000    792  1602 |     275 /    606 |    2429 /   4153 |   8.8 | 1451 / 3950 |       10/11s r1 |         3/4s r1 |        0         0 |       103
fix6-2000    790  1318 |     216 /    469 |    2010 /   3407 |   9.3 | 1382 / 3255 |       13/13s r2 |        4/17s r1 |        0         0 |        97
fix6d50-2000  790  1716 |     259 /    478 |    2709 /   4649 |  10.5 | 1650 / 3854 |       17/58s r2 |        5/18s r3 |        0         0 |        89
```

- fix1: closer tick scan.
- fix2: solicitation grace, no regeneration of replayed hashes, closed-epoch elections retired from buckets.
- fix3: voter Final targets of unfinalizable forks (as scheduler marking), vote and message queue caps.
- fix4: FxHash maps.
- fix5: only delivered replays count as replayed.
- fix6 (final): the voter pre-filter replaces the scheduler marking.

## Open items

- **Epoch-1 close on lean builds.** The close after the workload ends now takes 90-165 s (5 rounds) at 1000
  and 1500 blocks/s, versus 6-14 s on builds with higher CPU load. After the drain the six memberships differ
  by tens of members; one PR learns missing members at one per 5 s (the recovery-reply suppression window)
  while close rounds time out with doubling timers (3, 6, 12, 24, 48 s), so a node that proposes early with
  an incomplete membership pays exponentially. Close time tracks CPU headroom across every run (92% CPU: 6 s;
  58% CPU: 160 s). It does not affect the workload here, but a close longer than an epoch would block block
  admission two epochs later in a continuous run. Candidates: apply cached votes to late fork candidates,
  re-solicit missing members faster than the 5 s suppression, cap the round-timeout doubling.
- **Close commit stall.** `AecService::close_epoch` holds the AEC write lock for the LMDB commit of ~26,000
  members (two records each); votes wait ~0.2-0.5 s, visible as the p95 bump in the window that contains
  the close.
- **Scheduler convoy at saturation.** At 2000 blocks/s the block processor waits ~20% of its time for the
  scheduler mutex, which the scheduler loop holds while taking the AEC write lock.
- **Vote generator delay.** 50 ms instead of 100 ms cuts non-fork latency by ~40% at 1000 blocks/s. At 1500
  it improves epoch 0 (73 ms) but the extra vote messages push the nodes to 91% CPU and epoch 1 degrades to
  313 ms, and at 2000 everything gets worse; on this host it only pays at 1000 blocks/s. It is a config
  setting, not a code change.

## Files

`tables.md` has per-epoch and per-window tables for the baseline and final runs; `*.json` hold the
nanospam BENCHMARK_RESULT and EPOCH_PERFORMANCE_RESULT records, drain and close times per PR, saturation
counters and mean node CPU; `rounds.txt` is the table above.

Binary identities (final): `rsnano` SHA-256 `dea5469c1bfa949f1147dade23e9eeb38c168d7394212a06c596d6ca9f1599ec`,
`nanospam` SHA-256 `4c818db2348bee87ffb98aee4cb15fff618acf0a1259d04072330569bf13be83`.
