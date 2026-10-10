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
- 16:49 the charger was unplugged again. The first pass (matrix + `delay40/`) ended at 18:39; in the gap
  before the queued `resume.sh` started, nothing held the Mac awake and it idle-slept at 18:39:32. The
  resume pass then started `fork10-byz1/rai-r1` during a dark wake at 18:55 and the run spanned sleeps
  again (`rai-r1.slept-again/`, excluded). Aborting it, I killed the process group of the run that had
  already replaced it, `fork10-byz1/pause-r1` (`pause-r1.killed-by-operator-2/`, excluded). No sleep
  occurred between 16:05 and 18:39, the window of every other run.
- The Nano arm of `delay40/` ran on the develop binary, which has no `simulated_delay_ms` and ignored it
  (its latencies equal the loopback ones): moved to `delay40-nano-unpatched/`, excluded. `finish.sh`
  (run under one `caffeinate -i`) re-runs the two excluded matrix runs, builds develop `ff2b32710` plus
  only the link-delay patch of `3ce49faff` (network crate and its TOML key), and re-runs the Nano arm of
  `delay40/` with it.
- `finish.sh` completed at 20:06 on an awake machine: `fork10-byz1/rai-r1` and `pause-r1` re-run, and the
  Nano arm of `delay40/` re-run with develop `ff2b32710` plus `develop-ff2b32710-link-delay.patch` (the
  network crate and TOML key of `3ce49faff`, nothing else), built without `rai_protocol`.

## Results (median of three runs; ranges are min-max)

Figures and numbers: `RAI_LaTeX/tools/eval_figures.py matrix RAI_LaTeX/sections/eval-data --delay delay40`.
Runs whose client never saw every non-fork block confirmed have no client numbers (stall in `result.json`):
RAI `nofork` r3 (one confirmation notification missing, all nodes cemented it), pause `fork10-offline1`
r1-r3 and `fork10-byz1` r1, r3. Runs not settled within the settle wait: pause `fork10` r3, Nano `fork10`
r1, r2 (validators differed by a few cemented blocks).

