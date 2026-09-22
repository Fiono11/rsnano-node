# Checkpoint recovery and the residual split (2026-09-22)

`5fa6e0c1f` (Protect_Q, residual First/Notar split) and `2ec92fa76` (the
placement hash binds its election slot), against `tex-weights`. 6 PRs,
2000 blocks/s, 45k blocks, 8 s epochs.

| variant | tex-weights | tex-protect | closes here |
|---------|-------------|-------------|-------------|
| fork0    | 2046 cps / 94 ms  | 2240 / 101 ms | all round 0 |
| fork5    | 2000 / 124 ms     | 1912 / 112 ms | all round 0, 4 epochs |
| offline1 | 2002 / 110 ms     | 1910 / 111 ms | all round 0 |
| byz1     | 2199 / 108 ms     | 2315 / 114 ms | 20 r0, 5 r1, 5 r2 |
| byz1 rep2 | -                | 2969 / 112 ms | 5 r0, 10 r1, 5 r2 |

All runs SETTLED_CONSISTENT, CLOSED_CONSISTENT, COMMITTEES_CONSISTENT, SAFE.

Only the residual split is live here: `Protect_Q` and the slot binding are
reached through `build_state`/`EpochValue`, which the running close does not
call yet. It still proposes the leader's own epoch state hash.

**Read the median, not the weighted tail, on byz1.** Its weighted figure was
279, 618 and 1380 ms on three runs of comparable code, a 5x spread, while the
median held at 108, 114 and 112 ms. Close rounds are noisy there too.
