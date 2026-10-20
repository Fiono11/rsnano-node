# RAI at Nano's quorum: n = 6, f = 1, p = 0, fast path off

Built from dcd84c00b. 2000 blocks/s, 48k blocks, 8 s epochs, 3 reps per cell.

- `rai`, `pause`: `--committee-p 0 --committee-seats 6` with `RAI_FAST_PATH=0` set for every node.
  A certificate needs n − f = 5 of 6, the same as Nano's 67% quorum on six equal representatives.
- `develop` (Nano): copied from `rai-paper-matrix-final-v2-2026-10-19`, which used the same Nano binary.
  The exception is `fork5-grace-byznode/r2000/develop` (rep 1), which was run here: the final-v2 run
  spanned a 10 h machine sleep (copied back there too).
- `*-byznode` cases: one equivocating Byzantine representative (votes, reports, first close leader).
  The RAI and pause cells of these cases were re-run on 3a8bad905, where a proposal carries the
  selected reports' signed commitments (`sweep-byzantine-rerun.log`); the honest cells are from
  dcd84c00b. Neither has b1ba244bb, 8958e13d3 or 325f6361b.
- No memo-off arm.
