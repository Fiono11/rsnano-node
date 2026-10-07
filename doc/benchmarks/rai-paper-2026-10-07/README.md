# Paper matrix — 7 October 2026 (run5)

**All nine variants settled on `172fb25b6`**, one run each: six equal-weight representatives (f = p = 1), 45,000 blocks at 2,000/s, 45,000 accounts, 8 s epochs, quiet host (`--wait-quiet 900`), `--timeout 150 --settle-timeout 90`. Settled means identical cemented state and identical decided checkpoints on every running node. These are the RAI rows of the paper's Table 3 (`RAI_LaTeX/tools/eval_table.py run5`).

| Variant | Status | Uncemented, same on all | Close rounds | Last close, median / max | Non-fork goodput | Client p50 / p95 / p99 | Symbols per item / dropped streams |
| --- | --- | --- | --- | --- | --- | --- | --- |
| nofork | settled | 0 | 0, 0, 0 | 1.89 / 3.08 s | 1,942 blocks/s | 108 / 2403 / 3399 ms | 1.38 / 0 |
| fork5 | settled | 2096 | 0, 0, 0 | 2.07 / 3.94 s | 1,807 blocks/s | 133 / 4169 / 4940 ms | 2.02 / 0 |
| fork10 | settled | 4293 | 0, 0, 0 | 4.13 / 4.35 s | 1,440 blocks/s | 401 / 3915 / 4797 ms | 1.56 / 0 |
| nofork-offline1 | settled | 0 | 0, 0, 1 | 1.79 / 3.90 s | 1,978 blocks/s | 117 / 1432 / 1962 ms | - / 0 |
| fork5-offline1 | settled | 2307 | 0, 0, 1 | 3.15 / 4.08 s | 1,815 blocks/s | 126 / 2070 / 3013 ms | 3.56 / 0 |
| fork10-offline1 | settled | 4381 | 0, 0, 1 | 5.38 / 7.81 s | 1,267 blocks/s | 203 / 8188 / 8820 ms | 1.53 / 0 |
| nofork-byz1 | settled | 0 | 0, 0, 1 | 1.57 / 3.75 s | 1,969 blocks/s | 117 / 1280 / 1897 ms | - / 0 |
| fork5-byz1 | settled | 2241 | 0, 0, 1 | 2.70 / 4.00 s | 1,750 blocks/s | 125 / 2191 / 2802 ms | 1.68 / 0 |
| fork10-byz1 | settled | 4481 | 0, 0, 1 | 4.57 / 7.59 s | 1,253 blocks/s | 281 / 5287 / 8469 ms | 1.66 / 0 |

## The day's passes

| Pass | Build | Result |
| --- | --- | --- |
| [run2](run2/) | `a4872d09e`: re-voted-parent fix, predecessor-backed overlap route, no manifest, in-memory records | 9/9 settled; the paper's comparison table (Table 4) |
| [run3](run3/) | `617eeffde`: prefix-wide overlap rule, manifest, install acknowledgements, durable records | 8/9; fork5-byz1 left 337 non-fork blocks unfinalized (first votes split across a boundary leave a recovery lock nobody re-votes) |
| [run4](run4/) | `73ac5d978`: every unfinalized instance re-voted in the new epoch | 9/9 settled, but fork10-offline1 and fork10-byz1 never finished the client measurement: carried fork duplicates filled the election container (cap 5,000) and nothing new was activated |
| run5 (this) | `172fb25b6`: manifest fetched from the proposer, owner continuation in the generator, re-voted recovery lock continues on an open-epoch NC, idle duplicates discarded at install, cap 20,000 | 9/9 settled and measured; the paper's Table 3 |

The build before all of these (`25646c27e`, [paper-run1](../rai-gate-d-2026-10-07/paper-run1/)) never closed an epoch in fork10 and fork5-offline1: a block voted in two consecutive epochs lost its recorded parent when the older epoch's slot state was dropped, so the next report carried a residual record with a zero parent and `BuildState` failed at every leader; fixed in `a4872d09e`, which also refuses malformed reports.

Against run2 the medians are within 10 to 30 ms without forks and at 5% forks, while p95 and p99 are an order of magnitude longer and 10%-fork goodput 15 to 25% lower: the prefix-wide overlap rule holds every block published during a close until the checkpoint installs, closes grew (report freeze 0.4 to 0.9 s with the durable write of the frozen sets, reconciliation 1.1 to 2.4 s with two to four times as many retained positions), and an epoch cannot end before the previous close decides. Per-epoch phase timings come from `phases.py` over `run.log` (ENDED, REPORT, RECONCILED, CLOSE_READY, PROPOSED, MANIFEST_FETCHED, CLOSED).

Uncemented positions are forks whose votes split, retained identically everywhere; nanospam never finishes on forks, so settlement is judged on the nodes. Reproduce one variant:

```sh
python3 tools/rai/run_gate_b.py <dir> --wait-quiet 600 --fork-percentage 10 --offline 1 --timeout 150 --settle-timeout 90
```

