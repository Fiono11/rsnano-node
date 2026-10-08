# DSN 2027 paper matrix — 8 October 2026

Nine-variant matrix on `06e099d37` (the final `rai_kudzu_close` commit, with the TLA+ repairs and the proposal-check fix), one run per variant: six equal-weight representatives (f = p = 1), 45,000 accounts, 45,000 blocks at 2,000/s, 8 s epochs, quiet host (`--wait-quiet 900`), `--timeout 150 --settle-timeout 90`. These are the RAI rows of Table III of the DSN 2027 submission (`RAI_LaTeX/tools/eval_table.py <this dir> RAI_LaTeX/sections/eval-table.tex`). Settled means identical cemented state and identical decided checkpoints on every running node.

| Variant | Status | Open positions at settlement | Checkpoints | Close rounds | Longest close | Non-fork goodput | Client p50 / p95 / p99 | Fork positions won by the sibling |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| nofork | settled | 0 | 3 | 0, 0, 0 | 5.3 s | 1,919 blocks/s | 117 / 595 / 741 ms | - |
| nofork-offline1 | settled | 0 | 3 | 0, 0, 1 | 3.8 s | 1,951 | 112 / 265 / 368 | - |
| nofork-byz1 | settled | 0 | 3 | 0, 0, 1 | 3.9 s | 1,958 | 112 / 292 / 483 | - |
| fork5 | settled | 0 | 4 | 0, 1, 0, 0 | 8.6 s | 1,759 | 223 / 601 / 724 | 866 / 2,279 |
| fork5-offline1 | settled | 0 | 5 | 0, 2, 1, 0, 0 | 11.6 s | 1,757 | 154 / 1039 / 1339 | 2 / 2,204 |
| fork5-byz1 | settled | 1,453 | 3 | 0, 2, 1 | 12.7 s | 1,382 | 163 / 1131 / 1323 | 790 / 2,243 |
| fork10 | settled | 2 | 4 | 0, 1, 0, 0 | 11.2 s | 1,634 | 638 / 2437 / 2747 | 1,617 / 4,433 |
| fork10-offline1 | settled | 0 | 6 | 0, 0, 1, 0, 0, 0 | 13.6 s | 1,657 | 482 / 3040 / 3579 | 0 / 4,449 |
| fork10-byz1 | settled | 2,920 | 3 | 0, 2, 3 | 21.3 s | 1,080 | 554 / 5756 / 13346 | 1,574 / 4,497 |

Compared with the 7 October matrix on the build before the repairs (`rai-paper-2026-10-07/run7`), forked positions now resolve (matching-origin discharge and the continuation of every retained block) instead of staying retained for the whole run, at the cost of longer closes and higher fork-variant tails; with a Byzantine member about two thirds of the forked positions stay retained and the closes need two to four rounds. Late-notarized blocks reached their closing-epoch exclusion witness in 95-96% of cases without forks, 86-88% at 5% forks and 79-80% at 10% (`EPOCH_LATE_NOTAR` lines in `run.log`). Reconstruction needed 1.5-2.1 coded symbols per differing item, no stream dropped.

## rerun/

Three variants run again the same evening for run-to-run variance: `nofork` 1,939 blocks/s, 125 / 846 / 1295 ms; `fork10` 1,698 blocks/s, 497 / 1638 / 2062 ms, 0 open positions, 4 checkpoints; `fork10-byz1` did not settle: epoch 2's close never became ready on the leaders (`EPOCH_CLOSE_UNREADY short_of=reports`, two reports never usable) and the harness timed out at 150 s (its `summary.json` is empty; `run.log` has the trace). The same stall was seen in 2 of 8 runs of that variant during the 8 October A/B work and is not diagnosed.

Reproduce one variant:

```sh
python3 tools/rai/run_gate_b.py <dir> --wait-quiet 600 --fork-percentage 10 --byzantine 1 --timeout 150 --settle-timeout 90
```
