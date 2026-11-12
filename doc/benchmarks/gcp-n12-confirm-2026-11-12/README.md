# Twelve seats across six regions, confirmation runs (Nov 12 2026)

Build b49726dce (node code of 51e4683fc: the two elections-lock fixes, the reconciliation
stream change and the common epoch origin), RAI only, no lock profile. Fourteen VMs as in
`../gcp-n12-2026-11-11` (`hosts-n14.txt`), twelve seats at f = p = 2, 45,000 blocks at
2,000 blocks/s, the matrix's seeds (`run_sweep.py --set final-wan-2000-n12`). Every run settled
with every block final and identical on all twelve validators, and one decided state per epoch.

| Cell | Blocks/s | p50 | p95 | p99 | Close median / max |
|---|---|---|---|---|---|
| nofork rep 1 (`../gcp-n12-2026-11-11`, `origin1`, lock profile on) | 1,946 | 425 ms | 764 ms | 949 ms | 2.3 / 3.8 s |
| nofork rep 2 | 1,937 | 451 ms | 1,160 ms | 1,759 ms | 2.5 / 4.9 s |
| nofork rep 3 | 1,937 | 434 ms | 636 ms | 723 ms | 2.3 / 4.6 s |
| fork10-grace rep 1 | 1,650 | 480 ms | 2,168 ms | 2,565 ms | 3.7 / 10.7 s |
| fork10-grace-byznode rep 1 | **209** | 585 ms | **27.1 s** | 28.1 s | **19.3 / 154 s** |

Nano at twelve seats, no forks: 1,937 blocks/s, p50 523, p95 579, p99 879 ms. With 10 % forks
at most 1,800 non-fork blocks/s are offered.

**The Byzantine cell collapses at twelve seats.** Every honest validator dropped 0.5 to 2.0 M
votes (the Byzantine one none), voter scans reached 0.3 to 0.9 s, and solicitation reached
257,000 to 622,000 asked instances. The epoch-2 close took 28 to 34 s and three rounds, epoch 3's
149 to 154 s and eleven; the agreement phase took 26 s at the median. Confirmations were healthy
for the first 12 s, then latency rose to 2 s and 4.5 s, and after the load the last blocks came
over 30 s. At six seats the same cell ran at 1,747 to 1,945 blocks/s with p95 under 1 s. Cause
not yet established: a lock-profiled run of this cell is the next step.

Files: `*/r2000/*/result.json.gz`, `close.dat` (`RAI_LaTeX/tools/close_breakdown.py --runs`),
the sweep summaries, and `confirm.log` (build and sweep log). The node logs are on the
controller under `~/rsnano-node/doc/benchmarks/gcp-n12-confirm-2026-11-12`.
