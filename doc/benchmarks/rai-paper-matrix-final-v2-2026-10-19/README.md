# Paper matrix: n = 6, f = p = 1

2000 blocks/s, 48k blocks, 8 s epochs, 3 reps per cell.

- RAI and pause, all cases: c534df908 (report endorsements, retained-block bodies, close-request
  lanes, a close round waiting for its queued proposal), `sweep-c534df908.log`.
- `*-byznode` cases: one equivocating Byzantine representative (votes, reports, first close leader).
- `fork10-grace-byznode/r2000/rai`: 8 reps, pooled because the cell runs at saturation and 3 reps
  were too noisy. Reps 1-3 are the sweep's (seeds 2001090-2001092); reps 4-8 are the B arm of
  `rai-queued-check-ab-2026-10-21` (seeds 2001090-2001094), the same node binary (sha256 52503cfc...).
- `develop` (Nano): the develop-fork-memo build (afe63ab1c) with the fork-signature memo; in the
  Byzantine cases the Byzantine node votes for every block of a fork. `fork5-grace-byznode/r2000/develop`
  (rep 1) is the run made in `rai-paper-matrix-p0-2026-10-20`: the one here spanned a 10 h machine sleep.
- The memo-off arm was dropped.
