# Paper matrix: n = 6, f = p = 1

2000 blocks/s, 48k blocks, 8 s epochs, 3 reps per cell.

- Honest cases (`nofork`, `fork5-grace`, `fork10-grace`): RAI and pause on 4bfec9897.
- `*-byznode` cases: one equivocating Byzantine representative (votes, reports, first close leader).
  RAI and pause re-run on 3a8bad905, where a proposal carries the selected reports' signed
  commitments (`sweep-byzantine-rerun.log`). `fork10-grace-byznode/r2000/rai-rep3` failed setup twice
  ("the PRs never held the same ledger") and was run with `--keep`, otherwise the same command.
- `develop` (Nano): the develop-fork-memo build (afe63ab1c) with the fork-signature memo; in the
  Byzantine cases the Byzantine node votes for every block of a fork. `fork5-grace-byznode/r2000/develop`
  (rep 1) is the run made in `rai-paper-matrix-p0-2026-10-20`: the one here spanned a 10 h machine sleep.
- The memo-off arm was dropped.
