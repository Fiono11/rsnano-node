# DSN 2027 paper matrix — 8 October 2026

**The paper's Table III comes from `fixed/`** (below). This top-level directory is the earlier pass on `06e099d37`.

Nine-variant matrix on `06e099d37` (the `rai_kudzu_close` commit with the TLA+ repairs and the proposal-check fix), one run per variant: six equal-weight representatives (f = p = 1), 45,000 accounts, 45,000 blocks at 2,000/s, 8 s epochs, quiet host (`--wait-quiet 900`), `--timeout 150 --settle-timeout 90`. Settled means identical cemented state and identical decided checkpoints on every running node.

## fixed/ — the same matrix on `2b785288e` (close-round grace, see `../rai-close-grace-2026-10-08`)

One run per variant, same settings (settle timeout 240 s). The `nofork` record's `source_revision` reads `ce6328162` because the commit was made while that run was in progress; the binary was the fixed one (`node_sha256` matches the other eight). `RAI_LaTeX/tools/eval_table.py fixed RAI_LaTeX/sections/eval-table.tex` regenerates the table.

| Variant | Status | Open positions | Checkpoints | Close rounds | Longest close | Non-fork goodput | Client p50 / p95 / p99 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| nofork | settled | 0 | 3 | 0, 0, 0 | 4.5 s | 1,921 blocks/s | 118 / 394 / 803 ms |
| nofork-offline1 | settled | 0 | 3 | 0, 0, 1 | 4.0 s | 1,958 | 112 / 281 / 371 |
| nofork-byz1 | settled | 0 | 3 | 0, 0, 1 | 3.7 s | 1,957 | 112 / 265 / 513 |
| fork5 | settled | 0 | 5 | 0, 0, 0, 0, 0 | 8.2 s | 1,667 | 225 / 1000 / 2349 |
| fork5-offline1 | settled | 1,433 | 3 | 0, 0, 1 | 8.6 s | 1,592 | 186 / 1068 / 4014 |
| fork5-byz1 | settled | 0 | 5 | 0, 0, 1, 0, 0 | 9.0 s | 1,567 | 162 / 1246 / 1479 |
| fork10 | settled | 0 | 4 | 0, 0, 0, 0 | 10.1 s | 1,484 | 406 / 3141 / 3717 |
| fork10-offline1 | settled | 2,920 | 3 | 0, 0, 1 | 14.8 s | 1,292 | 523 / 2674 / 3293 |
| fork10-byz1 | settled | 0 | 6 | 0, 0, 1, 0, 0, 0 | 15.1 s | 1,367 | 618 / 3560 / 3957 |

Against the `06e099d37` pass: the Byzantine variants close faster (fork10-byz1 21.3 -> 15.1 s, p95 5.8 -> 3.6 s, goodput 1,080 -> 1,367) and every correct-led round decides in that round; the no-fork rows are within noise; the offline fork rows lost 10-20% goodput in these single runs, which is within the run-to-run spread seen for this variant and not attributed. Whether the 3-3 split positions end up resolved or retained (Open 0 versus 1,400-2,900) flipped between the two passes in both directions and depends on timing, not on the fix.

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

Three variants run again the same evening for run-to-run variance: `nofork` 1,939 blocks/s, 125 / 846 / 1295 ms; `fork10` 1,698 blocks/s, 497 / 1638 / 2062 ms, 0 open positions, 4 checkpoints; `fork10-byz1` has no summary: the client finished normally (45,000 blocks in 37.8 s) and the nodes had closed epochs 0-2 and proposed epoch 3's close when the harness's settle poll failed (a 5 s RPC timeout against a node busy validating) and its cleanup crashed before writing `result.json` (`os.killpg` EPERM; see the `.err`). That was a harness defect, not a protocol stall; `run_gate_b.py` now retries failed polls, tolerates the kill error and writes the record first. Four further `fork10-byz1` runs on the same binary with the repaired harness are in `stallhunt/`: all settled, longest closes 17.3-18.1 s, p95 3.8-4.6 s. See `../rai-close-grace-2026-10-08` for why those closes are slow and the fix.

Reproduce one variant:

```sh
python3 tools/rai/run_gate_b.py <dir> --wait-quiet 600 --fork-percentage 10 --byzantine 1 --timeout 150 --settle-timeout 90
```
