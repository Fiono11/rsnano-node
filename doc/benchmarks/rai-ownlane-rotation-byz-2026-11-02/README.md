# Committee change with a Byzantine member, with the own-vote lane

The cells of `rai-rule2-rotation-byz-2026-10-29`, with one run replaced:
`fork10-grace-rot-byznode/r2000/rai-rep3` (seed 2001478) is rerun on the own-vote lane build.
Every other run, RAI, pause, and Nano (`develop*`), is copied unchanged from the 2026-10-29 record.

- Build: `base-commit.txt` (205eeb3d3 Rule 2 and later) plus `ownlane.patch`: a node's own signed
  votes enter its vote processor on a lane of their own (`VoteProcessorQueue::enqueue_own`), so they
  always reach its vote records and are served as evidence; the patch also carries diagnostic logging
  (`EPOCH_UNJUSTIFIED_WHY` fields, `EPOCH_RESIDUAL_WHY`). Node binary sha256 starts `fff93d5f4df9`.
- The run uses the sweep's flags (`run_sweep.py --set rotation-byz`, RAI arm
  `--epoch-ms=8000 --min-closed=3 --timeout=400`, `RAI_FAST_PATH=0`); the client command matches the
  2026-10-29 rep 3 exactly.
- Result: settled, every forked position resolved (177 open at the end of the load, 0 after 9.6 s).
  The 2026-10-29 rep 3 left 508 open: its validators' own votes were dropped in the loopback lane of the
  vote queue, which the vote cache's replays share, so reports listed votes no validator could serve,
  and the epoch-7 close never became ready.
- A second seed of the old 321-open run (2001476) also resolved every fork on this build
  (scratch replay, not part of this record).
