# Performance baseline: develop, 6 PRs, 2000 blocks/s, 45k blocks (2026-09-17)

Reference run for the plain (non-RAI) node on `develop`. Compare future runs against these numbers.

## Setup

| | |
|---|---|
| Commit | `ff2b32710` (develop) |
| Build | `cargo build --release -p rsnano_cli -p nanospam` (no extra features) |
| Command | `nanospam --prs 6 --no-prio --blocks 45000 --accounts 45000 --rate 2000 --no-kill` |
| Node config | nanospam defaults (AEC size 5000, bounded backlog off, LMDB nosync_unsafe, voting on) |
| Machine | Apple M1, 8 cores (4P+4E), 16 GB; verified idle before start (`top -l 2` gate in `run_one.sh`) |
| Date | 2026-09-17 |
| Total wall time incl. node start-up / wallet setup | 79 s |

This was the third of three identical runs that day; the first run hit the AEC per-bucket cap during the
ramp-up (avg conf 330 ms, ~3,000 elections evicted per node), the other two did not. See “Known variance” below.

## Headline (nanospam)

| Metric | Value |
|---|---|
| Blocks confirmed | 45,000 / 45,000 |
| Time publish → last confirmation | 24.51s |
| Confirmation rate | 1835 cps (offered 2,000 bps) |
| Average confirmation time | 205 ms |
| Steady state (t ≥ 6 s) | ~1,900–2,000 cps, ~190–200 ms |

## Per-second timeline (nanospam status line)

| t (s) | confirmed | cps | avg conf (ms) |
|---|---|---|---|
| 1 | 19 | 22 | 176 |
| 2 | 231 | 211 | 200 |
| 3 | 2,009 | 1,777 | 173 |
| 4 | 4,972 | 2,960 | 288 |
| 5 | 7,378 | 2,402 | 286 |
| 6 | 9,345 | 1,965 | 189 |
| 7 | 11,185 | 1,838 | 191 |
| 8 | 13,161 | 1,973 | 191 |
| 9 | 15,103 | 1,939 | 200 |
| 10 | 17,043 | 1,938 | 186 |
| 11 | 18,923 | 1,878 | 193 |
| 12 | 20,901 | 1,975 | 199 |
| 13 | 22,824 | 1,921 | 203 |
| 14 | 24,738 | 1,911 | 197 |
| 15 | 26,720 | 1,980 | 199 |
| 16 | 28,691 | 1,968 | 199 |
| 17 | 30,527 | 1,835 | 189 |
| 18 | 32,514 | 1,984 | 196 |
| 19 | 34,381 | 1,864 | 198 |
| 20 | 36,358 | 1,975 | 199 |
| 21 | 38,356 | 1,995 | 192 |
| 22 | 40,242 | 1,884 | 191 |
| 23 | 42,065 | 1,820 | 196 |
| 24 | 44,047 | 1,980 | 201 |

## Node state at exit (RPC)

| node | count | cemented | unchecked | data dir |
|---|---|---|---|---|
| PR0 | 45053 | 45053 | 0 | 52M |
| PR1 | 45053 | 45053 | 0 | 38M |
| PR2 | 45053 | 45053 | 0 | 112M |
| PR3 | 45053 | 45053 | 0 | 52M |
| PR4 | 45053 | 45053 | 0 | 96M |
| PR5 | 45053 | 45053 | 0 | 112M |

`confirmation_active` reported 0 unconfirmed on every node. 45,053 = 45,000 spam blocks + 53 setup blocks.

## Node counters at exit (`stats` counters, dir=in)

| counter | PR0 | PR1 | PR2 | PR3 | PR4 | PR5 |
|---|---|---|---|---|---|---|
| `active_elections.started` | 45,229 | 45,280 | 45,372 | 45,346 | 45,296 | 45,322 |
| `active_elections.confirmed` | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 |
| `active_elections_dropped.priority` | 176 | 228 | 319 | 294 | 243 | 269 |
| `active_elections_started.hinted` | 32 | 46 | 5 | 3 | 7 | 43 |
| `active_elections_confirmed.hinted` | 18 | 1 | 2 | 2 | 2 | 3 |
| `active_elections_confirmed.optimistic` | 0 | 0 | 0 | 0 | 0 | 0 |
| `election.confirmation_request` | 26,340 | 25,448 | 25,342 | 25,849 | 26,372 | 25,457 |
| `election.vote` | 489,855 | 489,256 | 489,187 | 492,406 | 492,541 | 492,648 |
| `election_vote.replayed` | 4,330 | 5,971 | 5,278 | 4,231 | 3,684 | 2,793 |
| `vote_processor.process` | 10,410 | 12,655 | 11,066 | 8,800 | 8,055 | 7,159 |
| `vote_cache.inserted` | 491,912 | 492,053 | 491,871 | 492,694 | 492,603 | 492,696 |
| `block_processor_result.progress` | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 |
| `block_processor_result.old` | 167 | 65 | 131 | 161 | 75 | 100 |
| `block_processor_result.fork` | 0 | 0 | 0 | 0 | 0 | 0 |
| `block_processor_result.conflict` | 20 | 4 | 8 | 5 | 5 | 4 |
| `message.publish` | 45,167 | 45,080 | 45,133 | 45,176 | 45,077 | 45,124 |
| `message.confirm_ack` | 3,862 | 3,772 | 3,753 | 3,751 | 3,749 | 3,790 |
| `message.confirm_req` | 891 | 752 | 778 | 778 | 800 | 432 |
| `confirming_set.cemented` | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 | 45,052 |
| `hinting.insert_failed` | 6,812 | 7,233 | 6,784 | 6,992 | 7,466 | 7,459 |

## Known variance

The AEC limit (`[node.active_elections] size`, default 5000) is applied per balance bucket:
`5000 / 66 buckets ≈ 75` elections per bucket. During the 0→2000 bps ramp the in-flight election count
sits close to that effective cap; when it is crossed the lowest-priority election of the bucket is evicted
and later re-activated (hinted / successor activation), which lengthens confirmation for those blocks and
can cascade for a few seconds. Across three runs on this day:

| run | config | rate | avg conf | peak 1-s avg conf | evicted per node |
|---|---|---|---|---|---|
| 1 | default | 1,804 cps | 330 ms | 875 ms | 2,661–3,148 |
| 2 | AEC size 50000 | 1,844 cps | 198 ms | 257 ms | 0 |
| 3 (this baseline) | default | 1,835 cps | 205 ms | 288 ms | 176–319 |

So with the default config expect ~200 ms / ~1,830 cps most of the time, with an occasional run at
~330 ms when the ramp-up tips into the eviction cascade. Raising the AEC size removes the variance.

## Files

- `run.log` – full nanospam log (ANSI stripped) followed by per-node `block_count`, `confirmation_active`, `stats` counters and objects, and data-dir sizes.
- `run_one.sh` – the script used (quiet-machine gate, PATH override to `target/release`, fresh `~/NanoSpam`, RPC snapshots, kill + cleanup).
