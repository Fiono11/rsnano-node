# DSN 2027 evaluation matrix (2026-10-10)

Build `3ce49faff` and later (`rai_eval_harness`), release, `rai_protocol`; Nano arm: unmodified `develop` `ff2b32710`
(`doc/benchmarks/paper-eval-2026-09-24/bin/develop` in the main checkout) with the new nanospam.
Three arms (`rai`, `pause` = `--epoch-transition pause`, `nano` = `--no-epochs`), nine variants, three
repetitions, interleaved (`run_all.sh`, `tools/rai/run_eval_matrix.py`), then `delay40/` (40 ms simulated
one-way link delay, no forks and 5 % forks). Six equal-weight PRs, 45,000 blocks at 2,000/s, 8 s epochs.

## Host incidents

- The charger was unplugged at 12:48 local; every matrix run is on battery (low power mode off).
- The Mac idle-slept at 13:48:59 (battery idle sleep is one minute and nothing held it awake), one second
  after `fork10-byz1/rai-r1` started, and slept and dark-woke until 16:05. That run's setup and load spanned
  the sleeps (a 634 s close): kept as `fork10-byz1/rai-r1.stalled/`, excluded. All runs before it finished
  before the first sleep.
- Cleaning up the stalled run's nodes also killed `fork10-byz1/pause-r1` 51 s in
  (`pause-r1.killed-by-operator/`, excluded).
- From 16:08 the runner is kept awake with `caffeinate -i -w <runner pid>`; `resume.sh` (queued after the
  first pass) re-runs the two excluded runs under `caffeinate -i`.
- The harness's settle poll could wait forever on nodes that stop answering; fixed in `96771e673` (gives up
  past a hard deadline).
