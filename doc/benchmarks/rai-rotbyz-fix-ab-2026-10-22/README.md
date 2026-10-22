# Retained-branch bodies: A/B on the fork10 Byzantine-rotation cell (Oct 22 2026)

Cell: `fork10-grace-rot-byznode` of `run_sweep --set rotation-byz` (six PRs, two
standby, four seats f=1 p=0, fast path off, rotation `0:1>4,2>5`, Byzantine PR3
`equivocate,conflicting,equivocate` leading the first round of every close),
2,000 blocks/s, 48k blocks, 8 s epochs. Same `run_gate_b.py` command for every
arm; seeds 2001476-2001478 are the sweep's reps 1-3.

Why: fork5 rep 2 of `rai-rotation-byz-2026-10-22` failed on the build before the
fix. From epoch 3 on, 27 retained blocks were held by no node: blocks rolled back
by a late-instance discard whose elections were gone, so no election supplied
the body and no ledger served it. The position never resolved.

Arms (binaries in the job's `sweep/final2`):
- prefix: `bin-rotbyz/rai` -> `bin-fix8/p1` (before the fix, node sha 707ddac8)
- v1: `bin-fix11/p1` (bodies of late-discarded blocks kept by the checkpoint
  follower, used for retained branches and BlocksReq answers; source = 795eb9199)
- v2: `bin-fix12/p1` (bodies of every rolled-back block, fe0430ae3; v2 runs are
  the sweep's `rai*.v2-binary` and `v2-s*` here)

| seed | prefix p50/p95, goodput | v1 | v2 |
|---|---|---|---|
| 2001476 | 1013/6093 ms, 1338/s | 1194/6948 ms, 1029/s | 1513/15681 ms, 906/s |
| 2001477 | 1228/4107 ms, 1458/s | 1003/6715 ms, 1122/s | 856/16332 ms, 718/s |
| 2001478 | 1164/8173 ms, 1036/s | 752/5083 ms, 1208/s | **failed** 1809/11553 ms, 1017/s |
| 2001479 | 1221/6443 ms, 1327/s | 759/4691 ms, 1333/s | 1302/4901 ms, 1444/s |
| 2001480 | 1385/5708 ms, 1401/s | 937/4692 ms, 1430/s | 1322/7968 ms, 1039/s |
| median | 1221/6093 ms, 1338/s | 937/5083 ms, 1208/s | 1322/11553 ms, 1017/s |

Every prefix and v1 run settled with all forks resolved. v2 was worse on four of
five seeds and one run did not settle (epochs 7-8 decided on some nodes only,
one close 65 s): a fork loser forced back in from the cache fights the live
instance resolving its position. On pr0, v2 processed 3-4x the blocks (350-460k
against 110-210k), carried 10x the instances into post-load epochs (14,686
zero-vote instances on pr0 in the failed run) and overfilled the request
aggregator (1.46M hashes against 0). Kept: v1 (795eb9199).

The cell is at saturation, and runs of one arm vary by 1.4x in goodput.
