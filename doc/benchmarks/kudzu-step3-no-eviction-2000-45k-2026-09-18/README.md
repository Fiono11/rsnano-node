# Kudzu step 3 + no eviction: elections leave the AEC only when finalized (2026-09-18)

Same build flags, command and machine as `../kudzu-step3-forks-2000-45k-2026-09-18`
(`f58a829b5` + the changes below, `--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks,
5 % forks, `run_fork_settle.sh` until `SETTLED_CONSISTENT`).

Changes on top of `../kudzu-step3-activation-skip-2000-45k-2026-09-18`, all under `rai_protocol`:

1. `ActiveElectionsContainer::erase_lowest_prio_election` never evicts. Previously a vote-less
   election could be evicted when its bucket was over the per-bucket cap (5000 / 66 ≈ 75).
2. `Bucket::available` has no priority override: a block whose AEC bucket is full waits in the
   scheduler bucket (8192 per bucket) until an election of that bucket terminates or finalizes,
   instead of replacing a running election. Without an override the scheduler cannot spin on
   a candidate it cannot place.
3. New `AecFact::ElectionTerminated`: a terminated election leaves its bucket, so the
   schedulers are notified exactly as on `ElectionEnded`.

## Results

| Metric (nanospam, publishing phase = status lines with ≥ 1000 cps) | step 3 | activation skip | **this run** |
|---|---|---|---|
| Non-fork blocks confirmed | 42,819 | 43,034 | 42,818 |
| Confirmation rate | 1832 cps | 1831 cps | 1817 cps |
| Average confirmation time, cps-weighted | 213 ms | 179 ms | **159 ms** |
| Median of the per-second averages | 123 ms | 117 ms | 117 ms |
| Worst second | 695 ms | 550 ms | **473 ms** |
| Settle phase | 11 s | 3 s | 11 s |
| Starved PR | PR? (not measured) | PR4: p50 298 ms, 14.8k evictions | **none**, 0 evictions on every PR |

Node side (`kudzu-node-latency.txt`):

| PR | non-fork finalization p50 / p95 / p99 | fork termination p50 / p95 | evicted / over-cap |
|---|---|---|---|
| PR0 | 97 / 228 / 413 ms | 156 / 392 ms | 0 / 0 |
| PR1 | 98 / 203 / 325 ms | 151 / 376 ms | 0 / 0 |
| PR2 | 101 / 521 / 754 ms | 161 / 736 ms | 0 / 0 |
| PR3 | 95 / 209 / 319 ms | 150 / 372 ms | 0 / 0 |
| PR4 | 97 / 209 / 315 ms | 152 / 379 ms | 0 / 0 |
| PR5 | 96 / 238 / 401 ms | 152 / 381 ms | 0 / 0 |

PR2 lagged during the ramp (p95 521 ms) but shows none of the 5 s tail that evicted PRs had
in every previous run: without eviction a lagging PR works through its scheduler queue in
priority order and each election finalizes on its cached votes. `election_scheduler.activate_full`
is 0 on all PRs, i.e. the 8192-block scheduler buckets never overflowed.

Fork finalization (the ~100 forks per PR that do finalize) still shows p95 ≈ 5.3 s: those are
final votes recovered through the 5 s solicitation round. The 11 s settle phase is the same
mechanism (18 roots pending at t = 3 s, none at t = 11 s).
