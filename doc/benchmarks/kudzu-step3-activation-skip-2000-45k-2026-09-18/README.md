# Kudzu step 3 + scheduler skips running priority elections (2026-09-18)

Same build flags, command and machine as `../kudzu-step3-forks-2000-45k-2026-09-18`
(`6aac5ab74` + the changes below, `--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks,
5 % forks, `run_fork_settle.sh` until `SETTLED_CONSISTENT`).

Changes on top of step 3 (uncommitted at the time of the run):

1. `PriorityScheduler::activate_with_info` looks the next unconfirmed hash up in the AEC
   before reading the block: if a priority (or manual) election is already running, the
   activation is skipped (`election_scheduler.already_active`). Hinted and optimistic
   elections are still inserted so the AEC can upgrade them. Previously every such activation
   cost a block read, `dependencies_confirmed`, `block_priority`, a bucket insert and an AEC
   `refill` round only to be rejected as `election_bucket.activate_failed_duplicate`
   (94k–118k per node in `../kudzu-step3-norebroadcast-2000-45k-2026-09-18`, i.e. more than
   two per election). The source is the backlog scan, which re-activates every unconfirmed
   frontier on every pass (`backlog_scan.activated` ≈ the rejected count).
2. nanospam subscribes to `confirmation` with `include_block: false` (no block JSON on PR0).
3. Vote rebroadcast is back on (default); the previous run showed it is the only second
   carrier for a dropped vote.

## Results

| Metric (nanospam, publishing phase = status lines with ≥ 1000 cps) | step 3 | no rebroadcast | **this run** |
|---|---|---|---|
| Non-fork blocks confirmed | 42,819 | 42,854 | 43,034 |
| Confirmation rate | 1832 cps | 1814 cps | 1831 cps |
| Average confirmation time, cps-weighted | 213 ms | 198 ms | **179 ms** |
| Median of the per-second averages | 123 ms | 133 ms | **117 ms** |
| Worst second | 695 ms | 598 ms | 550 ms |
| Settle phase | 11 s | 38 s | **3 s** |

Node side (`kudzu-node-latency.txt`):

| PR | non-fork finalization p50 / p95 / p99 | fork termination p50 / p95 | evicted | duplicate activations |
|---|---|---|---|---|
| PR0 | 106 / 291 / 490 ms | 166 / 460 ms | 1,011 | 2 (53k skipped) |
| PR1 | 105 / 282 / 435 ms | 165 / 386 ms | 1,363 | 0 (53k skipped) |
| PR2 | 104 / 205 / 396 ms | 167 / 392 ms | 548 | 2 (52k skipped) |
| PR3 | 105 / 228 / 357 ms | 165 / 356 ms | 645 | 3 (52k skipped) |
| PR4 | **298 / 5422 / 6023 ms** | **385 / 5556 ms** | **14,793** (171 over-capacity) | 101 (69k skipped) |
| PR5 | 105 / 232 / 357 ms | 165 / 386 ms | 596 | 3 (52k skipped) |

Step 3 had p95 242–444 ms on the healthy PRs; the p95 is now 205–291 ms and fork termination
p50 dropped from ~150 ms to ~165 ms only because the count includes the starved PR's late
terminations (healthy PRs: 165 ms vs 148–153 ms, within run variance). The `backlog_scan`
counters show the skip: 52k–54k activations per node now cost one AEC read lock each.

## The starved PR is the eviction → backlog-scan cycle

Every run of this series has had one starved PR (PR1 last time, PR4 here); nanospam does not
see it because it measures on PR0. Its signature is the same each time: ~12k–15k evictions,
p95 ≈ 5.4 s. That p95 is the backlog scan period (45k accounts at 10k accounts/s), i.e. the
PR falls a few seconds behind, exceeds the per-bucket cap (5000 / 66 ≈ 75 elections), evicts
its vote-less elections — under Kudzu they are vote-less only because the cached votes have
not been applied yet — and gets them back ~4.5 s later from the backlog scan.

Options, in the user's terms (Kudzu elections should not leave the AEC unless finalized):

- raise `active_elections.size` in the nanospam node config (50000 gave zero evictions on
  develop), which removes the symptom for the benchmark only;
- under `rai_protocol`, never evict: let over-cap candidates wait in the scheduler buckets
  (they are applied with their cached votes as soon as a slot frees) instead of inserting
  beyond the cap or evicting. Needs the `next_candidate` priority-override clause disabled
  when nothing can be evicted, otherwise the scheduler spins (seen on `rai_close_epochs`,
  2026-09-14).
