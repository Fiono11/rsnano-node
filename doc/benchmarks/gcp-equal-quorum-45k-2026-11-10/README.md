# RAI at Nano's quorum across the six regions, Nov 10 2026

RAI with six seats, f = 1, p = 0 and no fast path (`run_gate_b.py --no-fast-path --committee-p 0 --committee-seats 6`): a block needs five of six first votes and five of six final votes, as Nano needs 67% of the weight twice. The cells and seeds of the paper's matrix without the committee change (`run_sweep.py --set final-wan-2000`, arm `raieq`), 45,000 blocks at 2,000/s, three runs per cell. Nodes 8a7c1763 (the paper matrix's RAI node), client b728d052, harness 293ce810a. The harness passes `RAI_FAST_PATH=0` to the remote nodes since cd0e6e764. Driver `eq.sh` (~/rai-gcp-tools-2026-11-04/remote/), run 11:22-12:57 UTC.

All 18 runs settled with correct ledgers.

Medians of three runs; Nano and RAI (f = p = 1) are the paper's cells (`RAI_LaTeX/sections/eval-data/wan/bars-*.dat`).

| cell | p50 ms: this / Nano / RAI | p95 ms: this / Nano / RAI | blocks/s: this / Nano / RAI | slowest close ms |
|---|---|---|---|---|
| nofork | 549 / 535 / 390 | 774 / 592 / 483 | 1888 / 1928 / 1954 | 4779 |
| fork5-grace | 538 / 529 / 385 | 938 / 612 / 527 | 1792 / 1754 / 1858 | 4416 |
| fork10-grace | 541 / 533 / 400 | 1126 / 828 / 571 | 1704 / 946 / 1750 | 4417 |
| nofork-byznode | 543 / 529 / 393 | 739 / 595 / 508 | 1880 / 1912 / 1945 | 8339 |
| fork5-grace-byznode | 540 / 535 / 391 | 1018 / 608 / 538 | 1830 / 1770 / 1846 | 15149 |
| fork10-grace-byznode | 540 / 533 / 400 | 1077 / 1044 / 721 | 1744 / 740 / 1747 | 15213 |

| cell | rep | status | goodput | p50 | p95 | p99 | slowest close ms |
|---|---|---|---|---|---|---|---|
| nofork | 1 | settled | 1888 | 536 | 728 | 1010 | 4994 |
| nofork | 2 | settled | 1888 | 549 | 774 | 1053 | 4779 |
| nofork | 3 | settled | 1896 | 561 | 813 | 1173 | 4665 |
| fork5-grace | 1 | settled | 1794 | 538 | 991 | 1266 | 4416 |
| fork5-grace | 2 | settled | 1792 | 543 | 938 | 1175 | 4425 |
| fork5-grace | 3 | settled | 1792 | 528 | 923 | 1263 | 4368 |
| fork10-grace | 1 | settled | 1704 | 534 | 1028 | 1198 | 4417 |
| fork10-grace | 2 | settled | 1733 | 541 | 1182 | 1374 | 4174 |
| fork10-grace | 3 | settled | 1696 | 544 | 1126 | 1282 | 4682 |
| nofork-byznode | 1 | settled | 1880 | 537 | 739 | 1089 | 8339 |
| nofork-byznode | 2 | settled | 1880 | 544 | 739 | 1105 | 8525 |
| nofork-byznode | 3 | settled | 1896 | 543 | 709 | 993 | 5532 |
| fork5-grace-byznode | 1 | settled | 1739 | 554 | 2403 | 3354 | 15149 |
| fork5-grace-byznode | 2 | settled | 1830 | 540 | 944 | 1123 | 16154 |
| fork5-grace-byznode | 3 | settled | 1846 | 532 | 1018 | 1210 | 11274 |
| fork10-grace-byznode | 1 | settled | 1741 | 538 | 898 | 1097 | 16526 |
| fork10-grace-byznode | 2 | settled | 1744 | 540 | 1459 | 2367 | 15213 |
| fork10-grace-byznode | 3 | settled | 1747 | 549 | 1077 | 1566 | 14304 |
