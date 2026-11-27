# Solicitation backpressure A/B, twelve seats, committee change (Nov 27 2026)

Question: do the vote storms behind the slow closes of the twelve-seat committee-change cells
(matrix `gcp-matrix-n12-45k-2026-11-16`, cell `fork5-grace-reconf`: closes of 154-202 s in 3 of 3
runs) go away with solicitation backpressure and the inbound close lane?

Cell: `run_sweep.py --set final-wan-2000-n12 --variant fork5-grace-reconf` (twelve seats `f=p=2`,
two Toronto standbys taking both Montréal seats, 5% forks, 2000 blocks/s, 45,000 blocks), the
matrix's harness limits and seeds (reps 1 and 2), the matrix's nanospam. Same six-region fleet,
one evening (20:00-21:20 UTC), runs interleaved.

| Arm | Build | Node |
|---|---|---|
| base | `rai_dsn_anon` 5d5709827 (matrix node + catch-up fix + keep-pass fallback) | rsnano e4ce3eb0 |
| fix | `rai_backpressure` 15c7f4693 (base + `SolicitRate` + inbound close lane) | rsnano 042e7316 |
| matrix | the matrix's build (node e4601219e), same-time control | rsnano a0181c9b |

| Run | Goodput | p50 ms | p95 ms | Slowest close s | Votes sent M | Votes dropped at send M | Nodes dropping votes | Rate cuts |
|---|---|---|---|---|---|---|---|---|
| base rep 1 | 1912 | 416 | 877 | 4 | 0.24 | 0.0 | 0 | - |
| fix rep 1 | 1912 | 426 | 1256 | 6 | 0.33 | 0.2 | 1 | 5 |
| base rep 2 | 1811 | 434 | 2686 | 8 | 0.29 | 0.1 | 1 | - |
| fix rep 2 | 1841 | 429 | 2502 | 5 | 0.26 | 0.0 | 1 | 6 |
| matrix rep 2 | 1912 | 418 | 788 | 3 | 0.46 | 0.1 | 1 | - |
| matrix rep 1 | 587 | 594 | 34949 | 55 | 9.41 | 30.2 | 14 | - |

All six settled with identical ledgers. Only the matrix build stormed (rep 1). The same seeds
had stormed in the matrix (Nov 17): rep 2 closed after 202 s there and in 3 s here, so whether a
run storms depends on run conditions, not only on the seed.

Reading: neither base nor fix stormed (0 of 2 each), the matrix build stormed in 1 of 2. Too few
runs to credit the newer code or the fix; the fix shows no regression (closes 5-6 s against 4-8 s,
p95 within run-to-run spread). `bp_sum.py` recomputes the table; `bp_ab.sh`, `bp_m12.sh`,
`build_bp.sh` are the scripts. Node logs are not committed (kept outside the repo).
