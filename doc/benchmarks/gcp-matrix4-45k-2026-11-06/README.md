# Eleven reruns of the six-region matrix, Nov 7 2026

The runs of `gcp-matrix3-45k-2026-11-06` whose first publish gap began with at most 1,000 blocks in flight: the client had stopped publishing for 4-5 s waiting on a write to one node. Rerun on their matrix seeds with the client of 61880beaf (one writer per node, logs `RAI_PUBLISH_BLOCKED`), sha 8777c487. Nodes unchanged: 8a7c1763 for RAI and pause, 478d1750 for Nano. 45,000 blocks at 2,000/s, six regions plus the Toronto standby. Driver `matrix4.sh` (~/rai-gcp-tools-2026-11-04/remote/), run 10:33-12:06 UTC. `socket-samples/` holds the per-node and client samples of sockets with a full send queue, and the sweep log.

All 11 passed: RAI and pause settled with correct ledgers, Nano safe (same blocks held, values agree; its fork runs end as `failed` because Nano does not terminate forks). Ten of 11 published in every second of the load; pause fork5-grace-byznode rep 1 has one empty second (an account waits for its last block's confirmation). `RAI_PUBLISH_BLOCKED` appears once in fork10-grace rai and five times in nofork-reconf-byznode rai-rep2; the load continued.

| arm | cell | rep | status | goodput | p50 | p95 | p99 | slowest close ms | replaced run: p50 / p95 / goodput / close |
|---|---|---|---|---|---|---|---|---|---|
| rai | fork10-grace | 1 | settled | 1750 | 400 | 668 | 946 | 3922 | 414 / 653 / 1467 / 3798 |
| rai | nofork-byznode | 1 | settled | 1945 | 390 | 508 | 628 | 4420 | 401 / 756 / 1849 / 6144 |
| rai | nofork-byznode | 3 | settled | 1945 | 393 | 537 | 618 | 4609 | 402 / 536 / 1596 / 7987 |
| rai | nofork-reconf-byznode | 2 | settled | 1946 | 426 | 1190 | 1403 | 6611 | 417 / 601 / 1668 / 6185 |
| rai | fork5-grace-reconf-byznode | 3 | settled | 1726 | 443 | 1596 | 2349 | 14830 | 440 / 794 / 1734 / 6992 |
| pause | fork10-grace-reconf | 2 | settled | 1468 | 911 | 4693 | 5504 | 5735 | 1359 / 7334 / 1330 / 10582 |
| pause | fork5-grace-byznode | 1 | settled | 1073 | 912 | 6050 | 16685 | 18619 | 3043 / 15980 / 1097 / 17927 |
| develop | nofork-reconf | 1 | settled | 1929 | 530 | 687 | 1726 | - | 529 / 864 / 1904 / - |
| develop | nofork-reconf-byznode | 3 | settled | 1920 | 523 | 669 | 1607 | - | 538 / 1246 / 1020 / - |
| develop | fork5-grace-reconf-byznode | 3 | failed (safe) | 1521 | 536 | 1080 | 3011 | - | 537 / 1098 / 1165 / - |
| develop | fork10-grace-reconf-byznode | 2 | failed (safe) | 410 | 537 | 2431 | 56548 | - | 534 / 2117 / 554 / - |

The paper's figures take each run from this record first, then matrix3, then `gcp-matrix2-45k-2026-11-05` for Nano's cells without the committee change: `RAI_LaTeX/tools/wan_figures.py sections/eval-data/wan <matrix4> <matrix3> <matrix2>`.
