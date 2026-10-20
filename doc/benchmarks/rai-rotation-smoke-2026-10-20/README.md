# Committee rotation smoke run

Built from dcd84c00b. 2000 blocks/s, 48k blocks, 8 s epochs, honest, no forks, fast path on.

- 6 PRs, 2 of them standby (`--standby 2`), with `--committee-p 0`: 4 seats (n = 4, f = 1, p = 0).
- `--rotation "0:2>4,3>5"`: PR2 and PR3 give their weight to PR4 and PR5 at 0.5 s into epoch 0.
  Epoch 2 runs with the committee {PR0, PR1, PR4, PR5}.
