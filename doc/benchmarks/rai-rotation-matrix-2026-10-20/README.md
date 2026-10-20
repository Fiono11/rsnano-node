# Committee rotation at the p = 0 quorum

`run_sweep.py --set rotation`, 2000 blocks/s, 48k blocks, 8 s epochs, 3 reps per cell, honest.

- Six representatives, PR4 and PR5 standby with an eighth of a share; four seats (f = 1, p = 0),
  fast path off (`RAI_FAST_PATH=0`). In epoch 0 PR2 and PR3 hand their weight to PR4 and PR5, so
  epoch 2 runs with {PR0, PR1, PR4, PR5}. Nano moves the same balances at the same times
  (`--rotation-epoch-ms`).
- Client 68aee274a (weight-move blocks go to every PR). RAI and pause on 8958e13d3 (close-round
  requests bypass full request queues); `fork10-grace-rot/r2000/rai*` re-run on 325f6361b (an epoch
  stays open for the positions its predecessor carried in), after one rep left 704 forks open.
- `develop` (Nano): the develop-fork-memo build (afe63ab1c).
