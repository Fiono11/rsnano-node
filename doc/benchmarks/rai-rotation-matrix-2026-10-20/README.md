# Committee rotation at the p = 0 quorum

`run_sweep.py --set rotation`, 2000 blocks/s, 48k blocks, 8 s epochs, 3 reps per cell, honest.

- Six representatives, PR4 and PR5 standby with an eighth of a share; four seats (f = 1, p = 0),
  fast path off (`RAI_FAST_PATH=0`). In epoch 0 PR2 and PR3 hand their weight to PR4 and PR5, so
  epoch 2 runs with {PR0, PR1, PR4, PR5}. Nano moves the same balances at the same times
  (`--rotation-epoch-ms`).
- Client 68aee274a (weight-move blocks go to every PR). RAI and pause on c722b5b02, which reverts
  325f6361b (an epoch held open for the positions its predecessor carried in): the same node code as
  8958e13d3. An earlier sweep on 8958e13d3 had one fork10 RAI rep leave 704 forks open (closes fell
  behind the epochs and carried forks only got the 2 s vote window); counting it, 1 of 6 fork10 RAI
  runs without the hold stalled, and its seed resolved on a re-run.
- `develop` (Nano): the develop-fork-memo build (afe63ab1c).
