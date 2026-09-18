# Kudzu step 3 + vote rebroadcast off + no block JSON on the websocket (2026-09-18)

Same build, command and machine as `../kudzu-step3-forks-2000-45k-2026-09-18` (commit
`6aac5ab74`, `--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks,
`run_fork_settle.sh` until `SETTLED_CONSISTENT`), with two benchmark-harness changes
(`nanospam-changes.diff`, node code untouched):

1. `[node.vote_rebroadcaster] enable = false` in every node config. All six PRs are directly
   connected and there are no non-PR nodes, so every rebroadcast vote is a duplicate at the
   receiver.
2. nanospam subscribes to `confirmation` with `include_block: false`; only the hash and the
   receive time are used. Only PR0 has a subscriber.

## Result: no measurable gain, one starved PR, longer settle phase

| Metric (nanospam, publishing phase = status lines with ≥ 1000 cps) | step 3 | this run |
|---|---|---|
| Non-fork blocks confirmed | 42,819 | 42,854 |
| Confirmation rate | 1832 cps | 1814 cps |
| Average confirmation time, cps-weighted | 213 ms | 198 ms |
| Median of the per-second averages | 123 ms | 133 ms |
| Worst second | 695 ms | 598 ms |
| Settle phase (all 2,0xx forked roots identical on all PRs) | 11 s | **38 s** |

Node side (`kudzu-node-latency.txt`, measured from election start on each PR):

| PR | non-fork finalization avg / p50 / p95 | fork termination p50 / p95 | evicted |
|---|---|---|---|
| PR0 | 144 / 109 / 409 ms | 200 / 499 ms | 624 |
| PR1 | **885 / 411 / 5360 ms** | **497 / 5384 ms** | **12,404** (241 over-capacity) |
| PR2 | 146 / 110 / 389 ms | 202 / 490 ms | 823 |
| PR3 | 166 / 112 / 518 ms | 219 / 713 ms | 1,795 |
| PR4 | 171 / 112 / 543 ms | 210 / 669 ms | 1,464 |
| PR5 | 139 / 108 / 364 ms | 206 / 513 ms | 696 |

Step 3 had 97–101 ms p50 / 242–444 ms p95 non-fork finalization and 148–153 ms p50 fork
termination on every PR. Here the five healthy PRs are ~10 ms slower at the median and
~50 ms slower on fork termination, and PR1 was starved for the whole run (12k evictions,
p50 411 ms). nanospam measures on PR0, which is why its headline barely moved.

The starved-PR pattern is the known run-to-run variance (two of four identical step-3 runs
had it), so a single run cannot attribute the difference to the rebroadcast change. What the
run does show:

- Vote rebroadcast is not a cost worth removing in this topology: `confirm_ack in` dropped
  from ~35k to ~16–21k messages per healthy PR, with no latency gain.
- Without rebroadcast a vote dropped on the direct path has no second carrier. The settle
  phase went from 11 s to 38 s: 8 roots stayed "settled, finalizable" until solicitation rounds
  (5 s apart) fetched the missing final votes. Fork *finalization* (the ~150 forks that did
  finalize) has a p95 of 10–13 s and p99 of 30 s here, vs a max of ~10 s in step 3.
- `election_bucket.activate_failed_duplicate` is 94k–118k per node, i.e. more than two
  rejected activations per election. That is the activation churn identified on
  `rai_close_epochs` on 2026-09-14 and is the largest pure-waste item visible in these
  counters.
- The `vote_rebroadcaster.overfill` counter (19k–80k) only says the unstarted rebroadcaster's
  queue is full; `try_enqueue` still runs per processed vote.

Recommendation: keep vote rebroadcast on for the benchmark (or make it a nanospam flag),
keep `include_block: false` (harmless, saves JSON on PR0), and repeat before drawing any
conclusion from a single run.

Note: the busy-machine gate in `run_fork_settle.sh` matched a `top` header line
(`2026/09/18 ...`) as a busy process; it waited 30 s and then started. The machine was idle.
