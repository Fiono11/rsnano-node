# RAI latency optimization — 100 ms batching retained

Workload: six PRs, no priority workload, 15,000 accounts and blocks, 1,500 blocks/s, 7,500-block epochs, 5% random forks. Each run starts with fresh node/account data; all data directories and process groups are removed afterward.

The retained changes batch second-look notifications so representative keys are loaded once per tick, skip certificate/audit processing for rejected replay votes, and give newly eligible timeout votes a separate retry record. Repeated timeout votes remain rate limited. The node batching default remains **100 ms**, as requested. No quorum, signing-lock, finalization, or termination rules were relaxed.

Across the two unprofiled optimized 100 ms runs (sample-weighted PR0 means):

- Nonfork finalization: 1333.3 → 687.3 ms (48.5% lower).
- Fork termination: 1815.8 → 1028.9 ms (43.3% lower).

Both selected runs reached canonical agreement for all 15,000 roots within the performance window, with no conflicting finalizations. Nonfork roots lacking finalization at cutoff numbered 33 and 91, versus 124 in the baseline. These missing finalizations are not included in the latency means.


## Measurements

PR0 local earliest election insertion → first local finalization for nonforks; insertion → first verified block or timeout notarization for forks. Times span epochs and are cut off at the end of the 60-second performance window. Means exclude missing outcomes; their counts are shown explicitly. Fork finalization is not an optimization target. Per-PR distributions, p50/p95/p99/max, and per-epoch distributions are in each run’s `latency.json`.

| Run | Nonfork finalized / total | Nonfork mean / p95 / p99 ms | Fork terminated / total | Fork mean / p95 / p99 ms | All-PR agreement at cutoff | Extra recovery s |
|---|---:|---:|---:|---:|---|---:|
| baseline | 14141 / 14265 | 1333.3/3390.1/4474.7 | 735 / 735 | 1815.8/4411.8/4840.3 | yes | 0.000 |
| timeout-scheduler | 14142 / 14240 | 1517.6/3901.7/5874.2 | 760 / 760 | 2114.2/4487.0/5391.8 | yes | 0.000 |
| batched-notifications | 14237 / 14270 | 597.7/1432.0/1552.4 | 730 / 730 | 961.3/1973.6/2718.0 | yes | 0.000 |
| delay20 | 14189 / 14237 | 554.4/1430.5/1793.0 | 763 / 763 | 1015.8/2374.9/5604.5 | no | 16.124 |
| delay50 | 14151 / 14220 | 874.5/2198.2/2519.0 | 780 / 780 | 1207.7/2631.4/3294.0 | yes | 0.000 |
| repeat100 | 14180 / 14271 | 777.3/2075.5/3700.1 | 729 / 729 | 1096.5/2384.8/2726.5 | yes | 0.000 |

The timeout-scheduler run included a three-second CPU sample and is diagnostic, not a clean performance comparison. The 20 ms trial eventually reached agreement but needed additional recovery; it is not the retained configuration. Fork placement and scheduling vary between runs.

## Epoch results at 100 ms

Rows group completed outcomes by the epoch of their first local outcome. The clock starts at the root’s earliest insertion, including any earlier epoch. These are local timing cohorts; canonical certificate agreement is checked separately across every PR.

| Run | Epoch | Nonfork finalized samples | Nonfork mean / p95 ms | Fork terminated samples | Fork mean / p95 ms |
|---|---:|---:|---:|---:|---:|
| baseline | 0 | 8974 | 756.9 / 1704.7 | 491 | 1129.9 / 2993.4 |
| baseline | 1 | 5167 | 2334.4 / 3697.5 | 244 | 3196.1 / 4668.7 |
| batched-notifications | 0 | 7902 | 251.5 / 546.5 | 402 | 514.5 / 824.7 |
| batched-notifications | 1 | 6335 | 1029.5 / 1521.2 | 328 | 1509.0 / 2160.7 |
| repeat100 | 0 | 8555 | 365.7 / 847.6 | 426 | 610.5 / 1334.0 |
| repeat100 | 1 | 5625 | 1403.2 / 2426.7 | 303 | 1779.9 / 2544.7 |

## Bottleneck evidence

The sampled voter thread spent time fetching/decrypting wallet keys for individual second-look notifications, even when the root did not need a second-look vote. Notification draining is bounded at 64 roots per tick; previously that meant up to 64 key loads per tick. The batched implementation performs one load for the entire nonempty batch and releases the signing-state lock before enqueueing selected candidates.

The sample also shows substantial shared-lock waiting in vote-processing and vote-generation threads. RAI replay votes do not change tallies, but previously still ran certificate/audit and admission processing under the AEC write lock. They now return their per-block replay result and continue to the next hash without that work. Signature verification and new-vote processing remain intact.

On PR0, the instrumented baseline recorded 1,003 vote-processor overfills, 26,723 outgoing vote-message drops, and 41,984 outgoing block-message drops. The first optimized 100 ms run had zero vote-processor overfills, 13,141 outgoing vote-message drops, and 29,833 outgoing block-message drops. Queue pressure remains, particularly around epoch transition; this is not a claim that all bottlenecks have been eliminated.

## Reporting overhead and limits

The termination audit records timestamps and deduplicates events under the election lock, so instrumentation has a cost. The baseline and unprofiled optimized runs use the same event instrumentation, periodic RPC telemetry, and progress logging. Full audit download happens after the performance cutoff. The CPU sample was enabled only for the explicitly marked diagnostic run. Absolute no-audit latency has not been measured; the results are for this instrumented six-node local workload, not a production-network latency guarantee.

Every root must terminate with canonical all-PR agreement for the runner to pass. This does not mean every nonfork root finalized: nonfork roots with only notarization at cutoff remain visible in the finalized/total column. Timeout certificates are never counted as nonfork finalization.

## Validation and reproduction

632 RAI node unit tests, 597 legacy node unit tests, 3 RAI recovery integration tests, and 33 nanospam tests passed (one nanospam test ignored). Formatting and whitespace checks passed. Each run retains its executable hashes, command, outcome summary, compressed audit, telemetry, and cleanup record.

```sh
python3 workload-results/rai-latency/run.py 15000 1500 my-run 100
python3 workload-results/rai-latency/analyze.py workload-results/rai-latency/my-run
```

The runner requires permission to open local TCP/WebSocket channels. Its optional fourth argument sets `--vote-generator-delay-ms`; omitting it uses the unchanged 100 ms node default. The script exits nonzero if the underlying run fails or any workload root remains pending or inconsistent at the final check.
