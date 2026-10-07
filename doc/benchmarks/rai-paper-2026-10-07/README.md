# Paper matrix — 7 October 2026

**All nine variants settled on `a4872d09e`**, one run each: six equal-weight representatives (f = p = 1), 45,000 blocks at 2,000/s, 45,000 accounts, 8 s epochs, quiet host (`--wait-quiet 900`), `--timeout 150 --settle-timeout 90`. Settled means identical cemented state and identical decided checkpoints on every running node. These are the RAI rows of the paper's Table 3 (`RAI_LaTeX/tools/eval_table.py run2`).

| Variant | Status | Uncemented, same on all | Close rounds | Last close, median / max | Non-fork goodput | Client p50 / p95 / p99 | Symbols per item / dropped streams |
| --- | --- | --- | --- | --- | --- | --- | --- |
| nofork | settled | 0 | 0, 0, 0 | 1.25 / 2.23 s | 1,968 blocks/s | 98 / 280 / 880 ms | - / 0 |
| fork5 | settled | 2195 | 0, 0, 0 | 1.82 / 2.37 s | 1,824 blocks/s | 108 / 611 / 859 ms | 2.96 / 0 |
| fork10 | settled | 4212 | 0, 0, 0 | 2.09 / 3.88 s | 1,679 blocks/s | 128 / 660 / 960 ms | 1.9 / 0 |
| nofork-offline1 | settled | 0 | 0, 0, 1 | 1.45 / 3.61 s | 1,978 blocks/s | 110 / 205 / 441 ms | - / 0 |
| fork5-offline1 | settled | 2249 | 0, 0, 1 | 1.89 / 3.64 s | 1,836 blocks/s | 117 / 245 / 552 ms | 1.91 / 0 |
| fork10-offline1 | settled | 4513 | 0, 0, 1 | 2.94 / 4.31 s | 1,677 blocks/s | 131 / 411 / 492 ms | 1.98 / 0 |
| nofork-byz1 | settled | 0 | 0, 0, 1 | 1.44 / 3.89 s | 1,978 blocks/s | 112 / 189 / 433 ms | - / 0 |
| fork5-byz1 | settled | 2155 | 0, 0, 1 | 1.68 / 3.82 s | 1,841 blocks/s | 118 / 298 / 513 ms | 1.54 / 0 |
| fork10-byz1 | settled | 4484 | 0, 0, 1 | 2.92 / 4.29 s | 1,681 blocks/s | 131 / 448 / 717 ms | 3.21 / 0 |

The build before this one (`25646c27e`, [paper-run1](../rai-gate-d-2026-10-07/paper-run1/)) never closed an epoch in fork10 (epoch 1) and fork5-offline1 (epoch 2): every leader's `EPOCH_PROPOSAL_REFUSED ... MissingAncestry`. A block voted in two consecutive epochs lost its recorded parent when the older epoch's slot state was dropped, so the next report carried a residual record with a zero parent. Fixed in `a4872d09e`, which also refuses malformed reports (zero parent above height 1 unless the predecessor finalized the block there) so one such report cannot stall the close.

Uncemented positions are forks whose votes split, retained identically everywhere; nanospam never finishes on forks, so settlement is judged on the nodes. Reproduce one variant:

```sh
python3 tools/rai/run_gate_b.py <dir> --wait-quiet 600 --fork-percentage 10 --offline 1 --timeout 150 --settle-timeout 90
```

