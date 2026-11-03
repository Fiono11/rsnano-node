# Paper matrix across six regions, 45,000 blocks (Nov 3 2026)

`run_sweep.py --set final-wan --blocks 45000 --reps 3 --reps-from 1000`, f = p = 1,
8 s epochs, on the six-region n2-standard-4 fleet (Warsaw, Iowa, Montréal, Las Vegas,
Taiwan, Melbourne; round trips 32-313 ms), controller in Warsaw. Honest cases at 2000/s;
the cases with a Byzantine member at 1000/s, the highest rate at which RAI settles with an
equivocating member across these links (`../gcp-byzantine-wan-2026-11-03`). Run from
15:14 to 21:58 UTC, 54 runs, the arms interleaved per repetition with the same seeds.

- `rai`: rai_multihost node code 078063f88 (epoch boundary fix, request floor = longest
  round trip, evidence and solicitation backoffs), frozen in `~/bin-matrix/rai`.
- `pause`: the same binary with `--epoch-transition=pause`.
- `develop` (Nano): develop-fork-memo afe63ab1c, `--no-epochs`; its Byzantine node votes
  for every block of a fork.
- Byzantine member (PR5): votes for both blocks of every fork, conflicting reports, leads
  every close's first round with two proposals.

All 36 RAI and pause runs settled with every node's ledger identical and every fork
resolved. Nano does not terminate forks: its fork runs end with positions uncemented
(not settled) but are safe in all 18 runs (every node holds the same blocks, decided
values agree).

Median (min-max) of three runs; latency of the non-fork blocks, from the start of the
publish to the confirmation seen at PR0 (Warsaw):

| variant | rate | arm | settled | goodput cps | p50 ms | p95 ms | p99 ms | close max ms |
|---|---|---|---|---|---|---|---|---|
| nofork | 2000 | rai | 3/3 | 1767.9 (1760.2-1781.8) | 381 (380-381) | 461 (455-468) | 547 (542-569) | 4086 (4029-4445) |
| nofork | 2000 | pause | 3/3 | 1488.8 (1243.3-1572.8) | 1857 (919-2943) | 6420 (5038-13294) | 6830 (5798-14304) | 6915 (6525-16179) |
| nofork | 2000 | develop | 3/3 | 1608.2 (1608.1-1625.8) | 538 (532-538) | 594 (593-598) | 613 (612-627) | - |
| fork5-grace | 2000 | rai | 3/3 | 1653.8 (1639.2-1664.8) | 399 (377-420) | 511 (472-940) | 654 (587-1328) | 4352 (4212-5728) |
| fork5-grace | 2000 | pause | 3/3 | 1329.3 (1316.4-1503) | 863 (624-1209) | 8007 (5018-8202) | 8353 (5549-8583) | 11722 (7154-11922) |
| fork5-grace | 2000 | develop | 3/3 | 1513.5 (1484-1524.9) | 541 (526-551) | 602 (593-624) | 634 (630-712) | - |
| fork10-grace | 2000 | rai | 3/3 | 1517.9 (1496.5-1549) | 400 (388-411) | 566 (508-664) | 781 (740-883) | 3964 (3877-4568) |
| fork10-grace | 2000 | pause | 3/3 | 1318.4 (889.5-1326.2) | 942 (887-1706) | 5608 (5478-15501) | 6222 (6052-16080) | 6604 (5179-16535) |
| fork10-grace | 2000 | develop | 2/3 | 816 (490.7-897.4) | 534 (532-537) | 3320 (1703-5950) | 21816 (16563-44688) | - |
| nofork-byznode | 1000 | rai | 3/3 | 943.7 (941.7-943.8) | 386 (380-389) | 463 (458-476) | 523 (515-944) | 5604 (4523-7219) |
| nofork-byznode | 1000 | pause | 3/3 | 843.6 (834-846.9) | 513 (504-647) | 4422 (4284-4652) | 5011 (4642-5109) | 5066 (4677-5195) |
| nofork-byznode | 1000 | develop | 3/3 | 905.5 (870-909.1) | 535 (526-536) | 593 (592-760) | 620 (615-1059) | - |
| fork5-grace-byznode | 1000 | rai | 3/3 | 885.8 (884.8-887.1) | 387 (383-391) | 472 (466-537) | 612 (573-1535) | 7925 (7760-8066) |
| fork5-grace-byznode | 1000 | pause | 3/3 | 704 (674.8-752.1) | 1976 (1497-2206) | 6332 (5662-7195) | 7440 (6356-8187) | 9090 (6999-9910) |
| fork5-grace-byznode | 1000 | develop | 1/3 | 857.6 (840.5-864.2) | 534 (532-535) | 647 (588-763) | 954 (614-1055) | - |
| fork10-grace-byznode | 1000 | rai | 3/3 | 826 (808-836.2) | 393 (393-393) | 935 (505-1392) | 1823 (793-2345) | 7769 (7626-8045) |
| fork10-grace-byznode | 1000 | pause | 3/3 | 577 (544.2-607.9) | 2926 (855-3484) | 13597 (7856-14395) | 15216 (10962-16062) | 15324 (14537-18914) |
| fork10-grace-byznode | 1000 | develop | 0/3 | 715.8 (625.5-785.6) | 537 (531-550) | 620 (600-5789) | 2781 (1007-11125) | - |

- RAI vs Nano: RAI's p50 is ~140 ms lower in every case (380-400 vs 526-551 ms). Without
  forks RAI's tail is also lower (p95 461 vs 594 ms); with forks RAI keeps its tail
  (p95 511-935 ms) while Nano's grows at 10% forks (p95 3.3 s, p99 22 s honest) and Nano
  leaves forks unresolved (settled 0/3 in fork10-grace-byznode).
- Pause stops new instances at every epoch transition: p50 0.5-2.9 s, p95 4.4-13.6 s.
- RAI's closes take 4-8 s (longest per run), within the 8 s epochs; with the Byzantine
  member every close needs all five honest reports, hence the longer closes.

Files: `index.json` (sweep plan and run list), `runs.csv` (one row per run), per run
`<variant>/r<rate>/<arm>[-rep<k>]/` with `result.json.gz`, `run.log.gz`, `host.txt.gz`
and per node `pr*/diagnostics.log.gz` + configs; `*.summary.json.gz` are the harness's
printed summaries. The table: `python3 tools/rai/matrix_table.py <this dir>`.
