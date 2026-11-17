# Twelve-seat six-region matrix, 2000 blocks/s × 45,000 (Nov 16-17 2026, partial)

Twelve seats (f = p = 2) across six GCP regions, n2-standard-4, set `final-wan-2000-n12`
(`tools/rai/run_sweep.py`), arms RAI, pause and Nano (develop), three repetitions. Halted on
user request after RAI rep 3 of `fork10-grace`: 25 of 108 runs.

## Builds

| Runs | Node build |
|---|---|
| `nofork`, `fork5-grace`, every pause and Nano run, the smoke runs | `baba6248c` (frozen build) |
| `fork10-grace` RAI reps 1-3 | `e4601219e` (read-lock checks for no-op fork, activation and instance calls) |

`fork10-grace` RAI reps 1 and 2 are the fix's test runs from `gcp-n12-locks-2026-11-16`
(same seeds, settings and binary, sha256 `3acfd65f…`; see `ADOPTED.txt`). The frozen-build
rep 1 (settled) and rep 2 (failed: a laggard still catching up when the 30 s fork-grace
stall rule ended the run) are not included.

## Results (non-fork goodput blocks/s, latency ms, longest close s)

| Cell | RAI | pause | Nano |
|---|---|---|---|
| nofork | 1758-1937, p50 417-433, p95 669-787, close ≤ 4.3 | 1632-1811, p95 3394-5089 | 1929-1937, p50 513-518, p95 581-584 |
| fork5-grace | 1779-1832, p50 432-457, p95 861-1324, close ≤ 5.6 | 1100-1239, p95 14-19 s | 1756-1773, p50 514-519, p95 574-580 |
| fork10-grace | 946 / 871 / 264, p50 595-662, p95 6.9-51 s, close 19-83 | rep 1: 1151, p95 13.9 s | rep 1: 467, p99 61 s |

Smoke: RAI `fork10-grace-reconf-byznode` rep 1 settled (905/s, p95 14 s, handover close 68 s);
Nano `fork10-grace-byznode` rep 1 safe.

Every RAI and pause run here settled; every Nano run held the same blocks with agreeing values
(its forks do not terminate). At 10% forks four seats (pr2, pr3, pr4, pr8) overload on the
elections lock and fall behind; closes wait for their reports. Goodput at 10% forks is far below
the offered rate on both builds.
