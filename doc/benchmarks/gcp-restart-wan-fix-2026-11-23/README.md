# Restarts and a crashing departing member across six regions, Nov 23 2026

`run_sweep.py --set restart-wan`, RAI only, 45,000 blocks at 2,000 blocks/s, 8 s epochs, f = p = 1 on six seats (one per region: Warsaw, Iowa, Montreal, Las Vegas, Taiwan, Melbourne) plus the Toronto standby for the committee change (`hosts-r7.txt`). Every signature is logged and audited (`--restart` implies `--audit-votes`, `--signing-sync fsync`).

Build: the twelve-seat matrix node (ce61702ec, node code e4601219e) with `catchup.patch`, rustc 1.100.0; rsnano `885cbf53`, rsnano-byzantine `df0e5a59`, nanospam `8e63e375` (the matrix client). Driver flags: `--timeout=480:--stall-abort=90:--drain-limit=15:--settle-timeout=600:--fork-grace-stall=180`.

| cell | what happens | runs | goodput (blocks/s) | p50 / p95 (ms) | others' slowest close (s) | restarted node back (s) | audit (correct validators) |
|---|---|---|---|---|---|---|---|
| fork10-restart | PR2 killed 12 s into the load, down 5 s | 3/3 settled | 1,920 / 1,920 / 1,920 | 403-405 / 567-597 | 7.3-11.0 | 16.1-17.5 | 0 conflicts in 0.74-0.95 M slots |
| fork5-restart-two | PR1 killed as epoch 2's close starts, PR3 16 s into the load, both down 10 s | 3/3 settled | 1,227 / 1,240 / 1,248 | 384-389 / 597-639 | 30.0-34.9 | (joined the closes) | 0 conflicts in 0.63-0.64 M slots |
| fork5-handoff-crash | PR2 hands its seat to PR6 in epoch 0, killed as its last close (epoch 1) starts, down 30 s | 3/3 settled | 1,076 / 1,067 / 1,057 | 393-401 / 530-549 | 2.9-3.1 | 45-56 (incl. 30 s down) | 0 conflicts in 0.67-0.80 M slots |
| fork5-handoff-crash-byznode | the same with PR5 Byzantine (equivocates, leads every close) | 3/3 settled | 1,052 / 1,066 / 1,051 | 407-514 / 553-6,703 | 43.2-44.0 | 49-56 | 0 conflicts in 0.69-0.76 M slots; PR5 itself 2,427-3,227 |

All runs: forked positions resolved (`fork_grace.outcome = resolved`); the client counts the outage of a killed node in goodput. "Others' slowest close" is the largest `first_close_ms`; "back" the largest `last_close_ms` (the restarted node's).

## The bug these runs fix

`gcp-restart-wan-2026-11-23` is the same set on the unpatched matrix build. Rep 3 of fork10-restart never settled: the restarted PR2 never obtained epoch 2's checkpoint (`EPOCH_CLOSE_UNREADY epoch=2 usable=1 required=5`, no `EPOCH_CATCH_UP`), its epoch-3 instances waited for that checkpoint (`withheld_early`), and its drain never ended. Safety held (0 conflicts in 552,487 slots). Reps 1-2 recovered only after 28-73 s.

Cause: a close round's catch-up requests were sent only to peers known as representatives, and a restarted node learns which peer is which representative from the rep crawler's answers, which members no longer give once the epochs run (they re-send statements only for instances they voted in). `catchup.patch` asks every connected peer for close rounds and logs `EPOCH_CLOSE_SOLICIT` (peers asked, known representatives) and `EPOCH_CLOSE_SERVE` (requests answered). In every run here the restarted node knew no representative while catching up (`known_reps=0`, 9-10 peers asked), and caught up 4-6 epochs.

## Harness change

The vote audit read the Byzantine node's own signature log, so a restart run with `--byzantine-node` always failed on its deliberate equivocations. `audit_votes.audit_correct` leaves the Byzantine node (the last member before the standbys) out and reports its conflicts as `byzantine_conflicts`. The first byznode run, which settled but was judged with the old audit (2,220 conflicts, all by PR5), is in `set-aside/` with a note; the cell was run again with the fixed harness.

Records keep each run's `result.json`, summary, hosts, node diagnostics and signed-vote logs; `driver.log` is the controller's log.
