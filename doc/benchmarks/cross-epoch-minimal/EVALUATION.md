# Recorded evaluation — intermediate closure revision

Baseline: `5e037cfe0527d7b06b573456c7c876568ce179ea`.
Candidate: `181211b6f` (certificate-only checkpoint finality, correct genesis
ancestry, owner fresh-child recovery). This revision predates R/N/F report
membership from the updated PDF. Both versions used the same instrumented
recovery client, SHA256 `9b3e379da9bb4be58f8c85c319c2de0c34c305043469f1aaa8bcd3070ad50248`.

Five alternating AB/BA pairs, 45,000 primary blocks, 45,000 accounts,
2,000 blocks/s, six representatives, no intentional forks, 8-second epochs.
All ten attempts completed and settled consistently across all six nodes.
Recovery children are extra work, not part of primary goodput.

| Pair | Baseline blocks/s | Candidate blocks/s | Baseline p99 ms | Candidate p99 ms | Recovery children |
| --- | ---: | ---: | ---: | ---: | ---: |
| 0 | 1933.16 | 1953.96 | 4226 | 1159 | 48 |
| 1 | 1921.86 | 1914.55 | 2187 | 2166 | 21 |
| 2 | 1937.30 | 1941.80 | 4305 | 1202 | 4 |
| 3 | 1941.42 | 1904.91 | 4275 | 3399 | 23 |
| 4 | 1948.40 | 1958.12 | 1472 | 4343 | 34 |

The predeclared gate is **INCONCLUSIVE**, not PASS. Paired bootstrap 95%
intervals: goodput ratio [0.98964, 1.00676], within the .95 minimum;
p99 ratio [0.38040, 2.02417], unable to establish the 1.10 maximum.

Timing diagnostics associate several high-tail runs with an extra first
checkpoint round: baseline pairs 0/2/3 closed epoch 0 in round 1 near 11.6–11.8 s
from load start; candidate pair 4 did so near 11.6 s. Other epoch-0 closes
were near 8.8–9.2 s. Candidate pair 3 also closed epoch 1 later (18.7 s).
These observations explain an important source of variance; they do not
justify removing runs, changing the margin, or asserting non-inferiority.
No later stage has an accepted performance gate yet.

Earlier failed attempts remain recorded: missing harness data directory;
strict genesis reconstruction with synthetic frontier parents; and a completed
handoff leaving three retained blocks without an owner recovery child. The
failures motivated fixes; they were not silently retried or discarded.

Raw evidence and binaries: `/tmp/rai-cross-epoch-artifacts/`. The evaluated
batch is `step1-handoff-recovery/`; its `comparison.json`, per-attempt results,
logs, RPC snapshots, databases and `handoff-timing-analysis.json` are preserved.
See IMPLEMENTATION.md for evidence-validation and durability limitations.

## Baseline-derived deadlines

As requested after this batch, subsequent runs use
`ceil(1.5 * max(successful matching baseline wall_secs))`. The baseline
reference must match node/client binaries and workload arguments and have
completed with consistent settlement. Candidate times never increase the
allowance. These five baseline runs give a 156-second process deadline.
The original batch used its pre-existing 240-second ceiling, transparently
recorded in its manifest. Settlement observation and cleanup have their own
bounded deadlines and are outside the measured publishing interval.

## Approved disk cleanup

User approved reclaiming verified duplicate task Git objects. Verified all
197,160 referenced objects in the original repository, then replaced 4,802
byte-identical copied object files with hard links, reclaiming 11,260,580,387
logical duplicate bytes (~10.5 GiB). Git connectivity verification passed.
No source contents, dirty backup or benchmark evidence were deleted.
The audit is `/tmp/rai-cross-epoch-artifacts/git-deduplication.jsonl`.

## Initial R/N/F smoke comparison

Revision `dd1e743e4`, one pair, same workload and shared client, both using
156-second deadlines derived from the preceding five baselines. Both completed
and settled. Baseline: 1948.29 blocks/s, p99 1514 ms. Candidate: 1947.94 blocks/s,
p99 3972 ms, 25 extra recovery children. Verdict: SMOKE_ONLY. The candidate's
first close required an extra round. Its second report included 153 R entries,
confirming inherited recovery actually entered T rather than only the checkpoint.

Inspection then identified a strongest-status projection gap: known old finality
and finality of a fresh descendant must upgrade inherited R to F in the live
report. That correction and frozen-snapshot/successor-exclusion tests are a
separate revision; this smoke result does not evaluate it. Evidence remains in
`/tmp/rai-cross-epoch-artifacts/step1-rnf-smoke/`.

## Measurement scope and primary metrics

The measured host is a MacBookAir10,1 with eight physical/logical CPUs and
16 GiB RAM, running macOS 14.6.1 on arm64. All six nodes run on this host
using loopback networking. The inherited LMDB configuration is `nosync_unsafe`;
these measurements do not establish crash durability or distributed deployment
performance. The fixed offered load does not measure saturation capacity.

At the user's request, subsequent batches use p50 and p95 as the primary
latency measures (both paired bootstrap upper bounds must be <= 1.10), with
the existing goodput lower bound >= 0.95. p99 remains diagnostic. The harness
labels this policy `p50-p95-v1`; preceding results retain their original gates.

## R/N/F selected-prefix comparison

Revision `c0f4c0057`, five alternating pairs, same workload/client and
156-second baseline-derived deadlines. All ten attempts completed and settled.

| Pair | Baseline p50 ms | Candidate p50 ms | Baseline p95 ms | Candidate p95 ms |
| --- | ---: | ---: | ---: | ---: |
| 0 | 105 | 146 | 2664 | 3440 |
| 1 | 111 | 115 | 3283 | 3081 |
| 2 | 111 | 102 | 3311 | 1211 |
| 3 | 105 | 103 | 1223 | 1113 |
| 4 | 111 | 102 | 3359 | 1739 |

Verdict: **INCONCLUSIVE** under `p50-p95-v1`. Paired bootstrap 95% ratio
intervals: goodput [0.99001, 1.00305], p50 [0.93133, 1.22528],
p95 [0.51069, 1.07392]. Goodput and p95 meet their respective bounds,
but p50 does not establish non-inferiority. Diagnostic p99 interval:
[0.59618, 1.05466]. No runs were excluded. Evidence:
`/tmp/rai-cross-epoch-artifacts/step1-rnf-prefix-p50-p95/`.

A subsequent optimization moves live report projection outside the AEC read
lock using owned observations and an immutable predecessor snapshot, avoids
building a finalized-position index when a projection is already parent-closed
with unique positions, and removes an unnecessary full scan for unique hash
deletions. Its correctness tests cover frozen inputs and fork/ancestry cases.
It is a separate candidate and is not evaluated by the results above.

## Optimized projection comparison — stopped on baseline settlement failure

Candidate `0b71ca353`; same workload/client, 156-second process deadline,
30-second settlement window, five pairs planned. The runner stopped after
four attempts (two pairs), retaining the failed attempt.

| Pair | Node | Goodput blocks/s | p50 ms | p95 ms | p99 ms | Recovery children | Settled |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| 0 | baseline | 1927.37 | 103 | 1139 | 1786 | 0 | yes |
| 0 | candidate | 1950.48 | 101 | 990 | 1590 | 16 | yes |
| 1 | baseline | 1945.29 | 117 | 2967 | 3885 | 0 | **no** |
| 1 | candidate | 1937.34 | 100 | 710 | 1390 | 1 | yes |

All clients confirmed 45,000/45,000 primary blocks without a process timeout.
Baseline pair 1 PR4 had 45,065 blocks but only 16,243 cemented, 13,528 pending,
and a different final-state hash. The other five nodes had all blocks cemented,
zero pending and equal hashes. Logs report unusable epoch-0 reports and a
missing predecessor state for epoch 1; the exact root cause remains unresolved.
Verdict: **FAIL_SETTLEMENT**. No performance pass or confidence interval is
claimed for this incomplete batch. No failed run was replaced.

Tests: 795 RAI node tests and 733 default node tests passed. Both candidate
attempts observed zero R entries in their frozen reports: fresh-child recovery
occurred, but sustained carried-R inventory cost is not measured here.

See [PERFORMANCE.md](PERFORMANCE.md) for every recorded comparison, earlier
failures, all per-run percentiles, confidence intervals and evidence paths.
The raw optimized batch is `step1-rnf-projection-p50-p95/` in the artifact root.
