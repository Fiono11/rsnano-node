# Full recorded performance comparison

Baseline: `5e037cfe0527d7b06b573456c7c876568ce179ea` (clean rai_kudzu HEAD).

Latest workload: 45,000 primary blocks, 45,000 accounts, 2,000 blocks/s, six representatives, no intentional forks, 8-second epochs. Five alternating AB/BA pairs per full batch. Both nodes use the same pinned recovery client. Earlier account-only smoke uses its own manifest workload.

Host: MacBookAir10,1, eight CPUs, 16 GiB RAM, macOS 14.6.1 arm64. Six processes on one host over loopback. LMDB uses inherited `nosync_unsafe`. Workload generation is unseeded. These are fixed offered-load measurements, not capacity, crash durability or multi-host results.

p50/p95/p99 are observed primary block confirmation latencies in milliseconds. Goodput includes primary recovery waiting; additional owner recovery children are reported separately. Wall time includes setup/drain and is used only for deadlines, not goodput.

Current gate: paired bootstrap 95% interval of mean candidate/baseline ratios, 10,000 resamples, seed 731. Goodput lower bound must be >= 0.95; both p50 and p95 upper bounds must be <= 1.10. p99 is diagnostic. Earlier p99 gate verdicts are preserved. A pass applies only to the measured workload.

Subsequent runs use ceil(1.5 × slowest matching completed baseline wall time), currently 156 seconds. Candidate durations never extend the deadline. Each attempt records its actual deadline; earlier attempts used a fixed 240-second ceiling. Settlement and cleanup have separate bounded waits.

All attempts below are retained, including failures. Missing metrics are shown as —. Settlement true means all six nodes drained with equal final-state hashes; it is not a proof of protocol safety. Full signed-evidence validation, durable signing and other protocol stages remain incomplete; see IMPLEMENTATION.md.

## Initial harness smoke (data-directory failure)

Evidence: `/private/tmp/rai-cross-epoch-artifacts/smoke-account`. Recorded verdict: **FAIL_COMPLETION**.

| Pair | Node | Goodput blocks/s | p50 ms | p95 ms | p99 ms | Recovery children | Complete | Settled | Wall s | Deadline s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 0 | baseline | — | — | — | — | — | false | — | 0.49 | — |
| 0 | candidate | — | — | — | — | — | false | — | 0.23 | — |

Recorded gate output:

```json
{
  "verdict": "FAIL_COMPLETION",
  "attempts": 2
}
```

## Account-only smoke

Evidence: `/private/tmp/rai-cross-epoch-artifacts/smoke-account-v2`. Recorded verdict: **SMOKE_ONLY**.

| Pair | Node | Goodput blocks/s | p50 ms | p95 ms | p99 ms | Recovery children | Complete | Settled | Wall s | Deadline s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 0 | baseline | 2131.83 | 143 | 444 | 482 | 0 | true | — | 82.46 | — |
| 0 | candidate | 2148.22 | 138 | 237 | 278 | 0 | true | — | 82.50 | — |

Recorded gate output:

```json
{
  "verdict": "SMOKE_ONLY",
  "attempts": 2
}
```

## Initial closure (genesis ancestry failure)

Evidence: `/private/tmp/rai-cross-epoch-artifacts/smoke-handoff`. Recorded verdict: **FAIL_COMPLETION**.

| Pair | Node | Goodput blocks/s | p50 ms | p95 ms | p99 ms | Recovery children | Complete | Settled | Wall s | Deadline s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 0 | baseline | 1947.78 | 121 | 3399 | 4322 | 0 | true | — | 103.75 | — |

Recorded gate output:

```json
{
  "verdict": "FAIL_COMPLETION",
  "baseline_complete": true,
  "candidate_complete": false,
  "candidate_confirmed_at_last_status": 15953,
  "candidate_timeout_seconds": 240,
  "note": "Initial step1 candidate stalled at first handoff. SIGTERM cleanup was sent; subsequent unconditional SIGKILL raised PermissionError before result.json was saved. live-stall.json and run.log retain diagnostic evidence. This run is not excluded or treated as passing."
}
```

## Genesis ancestry correction (retained tail)

Evidence: `/private/tmp/rai-cross-epoch-artifacts/step1-handoff-genesis`. Recorded verdict: **FAIL_COMPLETION**.

| Pair | Node | Goodput blocks/s | p50 ms | p95 ms | p99 ms | Recovery children | Complete | Settled | Wall s | Deadline s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 0 | baseline | 1950.04 | 113 | 3361 | 4280 | 0 | true | true | 103.24 | — |
| 0 | candidate | — | — | — | — | — | false | — | 240.28 | — |

Recorded gate output:

```json
{
  "verdict": "FAIL_COMPLETION",
  "attempts": 2,
  "stopped_early": true
}
```

## Owner recovery, 181211b6f

Evidence: `/private/tmp/rai-cross-epoch-artifacts/step1-handoff-recovery`. Recorded verdict: **INCONCLUSIVE**.

| Pair | Node | Goodput blocks/s | p50 ms | p95 ms | p99 ms | Recovery children | Complete | Settled | Wall s | Deadline s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 0 | baseline | 1933.16 | 105 | 3316 | 4226 | 0 | true | true | 88.31 | — |
| 0 | candidate | 1953.96 | 99 | 631 | 1159 | 48 | true | true | 100.30 | — |
| 1 | baseline | 1921.86 | 103 | 1736 | 2187 | 0 | true | true | 103.86 | — |
| 1 | candidate | 1914.55 | 116 | 1627 | 2166 | 21 | true | true | 103.65 | — |
| 2 | baseline | 1937.30 | 111 | 3389 | 4305 | 0 | true | true | 103.59 | — |
| 2 | candidate | 1941.80 | 104 | 770 | 1202 | 4 | true | true | 103.46 | — |
| 3 | baseline | 1941.42 | 114 | 3363 | 4275 | 0 | true | true | 103.40 | — |
| 3 | candidate | 1904.91 | 115 | 2510 | 3399 | 23 | true | true | 104.10 | — |
| 4 | baseline | 1948.40 | 101 | 990 | 1472 | 0 | true | true | 103.19 | — |
| 4 | candidate | 1958.12 | 108 | 3423 | 4343 | 34 | true | true | 103.36 | — |

Recorded gate output:

```json
{
  "verdict": "INCONCLUSIVE",
  "goodput_ratio_ci95": [
    0.989643273809445,
    1.0067613445597776
  ],
  "p99_ratio_ci95": [
    0.3804034778460933,
    2.0241661703946425
  ]
}
```

Arithmetic means of per-run metrics (not pooled percentiles):

| Metric | Baseline mean | Candidate mean | Mean paired change |
| --- | ---: | ---: | ---: |
| goodput | 1936.43 | 1934.67 | -0.09% |
| p50_ms | 106.80 | 108.40 | +1.68% |
| p95_ms | 2558.80 | 1792.20 | +11.17% |
| p99_ms | 3293.00 | 2453.80 | +5.79% |

## R/N/F smoke, dd1e743e4

Evidence: `/private/tmp/rai-cross-epoch-artifacts/step1-rnf-smoke`. Recorded verdict: **SMOKE_ONLY**.

| Pair | Node | Goodput blocks/s | p50 ms | p95 ms | p99 ms | Recovery children | Complete | Settled | Wall s | Deadline s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 0 | baseline | 1948.29 | 101 | 1005 | 1514 | 0 | true | true | 87.91 | 156 |
| 0 | candidate | 1947.94 | 114 | 3056 | 3972 | 25 | true | true | 103.70 | 156 |

Recorded gate output:

```json
{
  "verdict": "SMOKE_ONLY",
  "attempts": 2
}
```

Arithmetic means of per-run metrics (not pooled percentiles):

| Metric | Baseline mean | Candidate mean | Mean paired change |
| --- | ---: | ---: | ---: |
| goodput | 1948.29 | 1947.94 | -0.02% |
| p50_ms | 101.00 | 114.00 | +12.87% |
| p95_ms | 1005.00 | 3056.00 | +204.08% |
| p99_ms | 1514.00 | 3972.00 | +162.35% |

Observed checkpoint/report diagnostics (rounds are indexed from zero):

| Attempt | Close rounds by epoch | Maximum R in a frozen report |
| --- | --- | ---: |
| 0 baseline | {'0': 0, '1': 0} | — |
| 0 candidate | {'0': 1, '1': 0} | 153 |

A maximum R of zero means that attempt does not measure sustained carried-R inventories, even if it generated fresh recovery children.

## Selected-prefix R/N/F, c0f4c0057

Evidence: `/private/tmp/rai-cross-epoch-artifacts/step1-rnf-prefix-p50-p95`. Recorded verdict: **INCONCLUSIVE**.

| Pair | Node | Goodput blocks/s | p50 ms | p95 ms | p99 ms | Recovery children | Complete | Settled | Wall s | Deadline s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 0 | baseline | 1940.82 | 105 | 2664 | 3578 | 0 | true | true | 103.41 | 156 |
| 0 | candidate | 1929.74 | 146 | 3440 | 4109 | 10 | true | true | 104.00 | 156 |
| 1 | baseline | 1956.58 | 111 | 3283 | 4203 | 0 | true | true | 103.16 | 156 |
| 1 | candidate | 1931.92 | 115 | 3081 | 3991 | 13 | true | true | 103.48 | 156 |
| 2 | baseline | 1959.65 | 111 | 3311 | 4217 | 0 | true | true | 103.00 | 156 |
| 2 | candidate | 1954.92 | 102 | 1211 | 1900 | 31 | true | true | 103.26 | 156 |
| 3 | baseline | 1938.56 | 105 | 1223 | 1783 | 0 | true | true | 103.41 | 156 |
| 3 | candidate | 1956.14 | 103 | 1113 | 1807 | 24 | true | true | 98.93 | 156 |
| 4 | baseline | 1963.50 | 111 | 3359 | 4289 | 0 | true | true | 103.19 | 156 |
| 4 | candidate | 1944.79 | 102 | 1739 | 2641 | 15 | true | true | 87.99 | 156 |

Recorded gate output:

```json
{
  "verdict": "INCONCLUSIVE",
  "gate_version": "p50-p95-v1",
  "goodput_ratio_ci95": [
    0.9900078462957298,
    1.0030538244217222
  ],
  "p50_ratio_ci95": [
    0.9313256113256113,
    1.2252767052767053
  ],
  "p95_ratio_ci95": [
    0.5106872203231387,
    1.0739163280782948
  ],
  "diagnostic_only": {
    "p99_ratio_ci95": [
      0.596178702841606,
      1.0546589241003816
    ]
  }
}
```

Arithmetic means of per-run metrics (not pooled percentiles):

| Metric | Baseline mean | Candidate mean | Mean paired change |
| --- | ---: | ---: | ---: |
| goodput | 1951.82 | 1943.50 | -0.42% |
| p50_ms | 108.60 | 113.60 | +4.91% |
| p95_ms | 2768.00 | 2116.80 | -19.53% |
| p99_ms | 3614.00 | 2889.60 | -16.45% |

Observed checkpoint/report diagnostics (rounds are indexed from zero):

| Attempt | Close rounds by epoch | Maximum R in a frozen report |
| --- | --- | ---: |
| 0 baseline | {'0': 1, '1': 0} | — |
| 0 candidate | {'0': 1, '1': 0} | 0 |
| 1 baseline | {'0': 1, '1': 0} | — |
| 1 candidate | {'0': 1, '1': 0} | 0 |
| 2 baseline | {'0': 1, '1': 0} | — |
| 2 candidate | {'0': 0, '1': 0} | 0 |
| 3 baseline | {'0': 0, '1': 0} | — |
| 3 candidate | {'0': 0, '1': 0} | 0 |
| 4 baseline | {'0': 1, '1': 0} | — |
| 4 candidate | {'0': 0, '1': 0} | 0 |

A maximum R of zero means that attempt does not measure sustained carried-R inventories, even if it generated fresh recovery children.

## Optimized report projection, 0b71ca353

Evidence: `/private/tmp/rai-cross-epoch-artifacts/step1-rnf-projection-p50-p95`. Recorded verdict: **FAIL_SETTLEMENT**.

| Pair | Node | Goodput blocks/s | p50 ms | p95 ms | p99 ms | Recovery children | Complete | Settled | Wall s | Deadline s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 0 | baseline | 1927.37 | 103 | 1139 | 1786 | 0 | true | true | 103.61 | 156 |
| 0 | candidate | 1950.48 | 101 | 990 | 1590 | 16 | true | true | 103.41 | 156 |
| 1 | baseline | 1945.29 | 117 | 2967 | 3885 | 0 | true | false | 130.68 | 156 |
| 1 | candidate | 1937.34 | 100 | 710 | 1390 | 1 | true | true | 103.45 | 156 |

Recorded gate output:

```json
{
  "verdict": "FAIL_SETTLEMENT",
  "attempts": 4,
  "stopped_early": true
}
```

Observed checkpoint/report diagnostics (rounds are indexed from zero):

| Attempt | Close rounds by epoch | Maximum R in a frozen report |
| --- | --- | ---: |
| 0 baseline | {'0': 0, '1': 0} | — |
| 0 candidate | {'0': 0, '1': 0} | 0 |
| 1 baseline | {'0': 1, '1': 0, '2': 0} | — |
| 1 candidate | {'0': 0, '1': 0} | 0 |

A maximum R of zero means that attempt does not measure sustained carried-R inventories, even if it generated fresh recovery children.

## Latest settlement failure

The optimized batch stopped after four attempts (two pairs), as prescribed by --stop-on-failure. All four clients confirmed 45,000/45,000 primary blocks without a process timeout. Both candidate attempts settled. In baseline pair 1, PR4 held 45,065 blocks but had cemented only 16,243 and reported 13,528 pending entries with a different final-state hash; the other five nodes had cemented all 45,065 with zero pending and equal hashes. Logs repeatedly report unusable epoch-0 reports and missing predecessor state for epoch 1. These are observations, not a proven root cause. The 30-second settlement window expired, producing FAIL_SETTLEMENT. No confidence interval or non-inferiority verdict is issued for this incomplete batch; the failed baseline was not replaced.

The latest candidate passed 795 RAI node unit tests and 733 default node unit tests. Both completed candidate runs had zero R entries in their observed frozen reports; repeated carried-R performance remains unmeasured.

## Interpretation

Compare each revision with its own interleaved baseline. The two revision batches are sequential, not a randomized direct head-to-head comparison, so their difference cannot isolate the optimization effect. No failed attempt was retried to manufacture a passing result. Early batches have different measurement/settlement capabilities and should not be pooled with later batches. Missing candidate metrics after an early cleanup failure remain missing.

The frozen baseline can finalize weaker checkpoint evidence than the revised candidate. Completion is therefore not automatically correctness-equivalent. This limits paper claims, especially for forked workloads.

Raw manifests retain binary/client/harness hashes, commands, order, deadline derivation and free-space observations. Per-attempt logs, node data and RPC snapshots are preserved under the evidence paths above.
