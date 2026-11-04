# Durable notarization: paired comparison on one host (Nov 4 2026)

`base`: rai_multihost 200520962 node code. `durable`: the same plus the first votes behind a final vote in the signing transaction, restored on a restart. Six processes on one MacBook Air, f = p = 1, 8 s epochs, 45,000 blocks, the seeds of `../gcp-matrix-45k-2026-11-03`, the arms alternating per repetition. The driver is `ab_run.py`.

| Cell | Blocks/s, base -> durable | p50 ms | p95 ms | Signing write, avg ms |
|---|---|---|---|---|
| no forks, 2000/s | 1918 -> 1915 | 114 -> 117 | 326 -> 340 | 0.9 -> 1.1 |
| 10% forks, 2000/s | 1605 -> 1596 | 280 -> 318 | 1889 -> 1848 | 1.2 -> 1.5 |
| 10% forks + Byzantine, 1000/s | 882 -> 880 | 118 -> 117 | 330 -> 290 | 0.9 -> 1.1 |

Medians over five runs; `fork10-grace-byznode/r1000/base-rep4` failed in setup (one node a block short while funding) and counts for nothing. Notarization votes written: 43-57 MB per run over six validators, about 200 bytes per block per validator. All 15 durable runs settled with every fork resolved. The differences are within the spread between runs. This is one host: it does not give the cost on the cloud disks of the six-region matrix.

The `durable` binary predates the guard in `mark_kudzu_voted` (a vote for an erased election is signed only if it was recorded), which only withholds votes.

`restart-runs.txt`: five audited restart runs on the final build (two validators killed, down 10 s, 1000/s, 30,000 blocks, 5% forks).
