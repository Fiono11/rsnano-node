# RAI leaderless close with sketch reconciliation, 2026-09-15

Two runs of the same workload on the 8-core M1 with the leaderless close and
IBLT sketch reconciliation (working tree on top of 54d4ba862):

```
nanospam --prs 6 --no-prio --fork-percentage 5 --blocks 50000 --accounts 50000 --rate 2000 \
  --epoch-terminated-elections 25000 --closed-epochs 2 --close-timeout 240 --data-dir <fresh dir>
```

Release `rsnano` and `nanospam` built with `--features rai_protocol`; no close
tracing (`RAI_CLOSE_TRACE_DIR` unset). Node data was deleted after each run.

## What changed

- Announcements carry a 192-cell invertible Bloom lookup table of the membership
  instead of the 256 level-1 digests. A peer subtracts its own sketch and decodes
  the members on either side in one message; the level-1 digests and pages are
  fetched only when the sketch does not decode (never happened in these runs).
- No leader: every drained replica FIRST-votes for the root it announced once
  every representative announced the same root (or a certificate-weight majority
  after 6 s). Replicas with the same membership and parent create the same
  candidate, so the votes tally without a proposer. A candidate no longer needs a
  leader's vote to be well formed.
- The per-certificate `block_certificate` trace is verbose-only; with the trace
  directory set it wrote 641k events under the container lock and inflated
  confirmation times to ~10 s (run 1, discarded).

## Results

| Run | Epoch | Close round (all 6 PRs) | Close after last drain start, first / last | Blocks / discarded | Non-fork finalization mean / p95, ms (PR0 receipt) |
|---|---|---|---:|---:|---:|
| run2 | 0 | 0 | 4.93 s / 5.21 s | 25,849 / 83 | 140 / 327 |
| run2 | 1 | 0 | 2.53 s / 2.64 s | 25,189 / 12–32 | 280 / 498 |
| run3 | 0 | 0 | 4.46 s / 4.86 s | 26,024 / 173 | 124 / 244 |
| run3 | 1 | 0 | 2.74 s / 2.89 s | 25,277 / 2–9 | 374 / 716 |

Workload: 47,414 and 47,512 of 50,000 blocks confirmed (the rest are the losing
sides of forks and the discards), mean confirmation 250 ms and 307 ms, 790 cps
over the 60 s window.

Every close had one hash across the six PRs. Reconciliation views in the
progress lines all show `decoded=true`, `sketch_failed=false`, and 0 level-2
pages or leaves fetched; the 2–5 members that differed per peer were solicited
by hash directly from the decoded difference.

## Subset signability (2026-09-16, run4 and run5)

Root equality made every late certificate change the root a replica could sign,
so a certificate landing mid-round cost the round (timeouts double up to 192 s).
Now a replica also signs a candidate whose root is its live membership minus
**droppable** members: members whose root carries a timeout certificate or
another notarized block, which Kudzu Lemma 5.7 says can never be finalized.
Droppability is read from the container before `drive` runs (`State::droppable`,
monotone), and the omitted members come from the decoded sketch of the
candidate's announced root. Same workload, two runs, same binaries otherwise:

| Run | Epoch | Close round (all 6 PRs) | Close after last drain start, first / last | Blocks / discarded | Non-fork finalization mean / p95, ms |
|---|---|---|---:|---:|---:|
| run4 | 0 | 0 | 3.31 s / 3.60 s | 25,922 / 201 | 161 / 330 |
| run4 | 1 | 0 | 6.67 s / 50.32 s | 25,260 / 39–100 | 524 / 1,112 |
| run5 | 0 | 0 | 5.15 s / 5.61 s | 25,983 / 131 | 132 / 245 |
| run5 | 1 | 0 | 2.68 s / 2.95 s | 25,104 / 9–37 | 313 / 759 |

run4 epoch 1: five PRs closed at 6.7 s with certificate weight after the
agreement timeout; the sixth held nothing extra but lacked one member
(`5CA40499…`) and solicited it every 2 s for 44 s before the peers' archived
votes arrived. That is the recovery-reply path (archived NOTAR votes on the
`VoteReply` class, subject to `should_drop` and a 5 s per-signature repeat
cache) under a saturated straggler, not the close logic; the close itself was
round 0 on all six. Mean confirmation 444 ms (run4, the more loaded run) and
268 ms (run5).

Files: `runN-close-events.log` (drain, close, reconciliation and progress lines
of the six nodes), `runN-results.log` (`BENCHMARK_RESULT` and
`EPOCH_PERFORMANCE_RESULT` without the timeline), `summarize.py` and `rows.py`
to print the tables above from a nanospam log.
