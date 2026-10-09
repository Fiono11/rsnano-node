# Restart and equivocation-audit harness: smoke runs (2026-10-09)

Build: `963cdeeac` (branch `rai_eval_harness`), release, `rai_protocol`.
Workload: `run_gate_b.py`, 6 equal-weight PRs (f = p = 1), 45,000 blocks at 2,000/s,
5 % forks, 8 s epochs. Both runs on a quiet host (`performance_eligible`).

| Run | Command extras | Settled | Audit conflicts (slots checked) | Restarts |
|---|---|---|---|---|
| `audit-only` | `--audit-votes` | yes | 0 (544,715) | - |
| `restart` | `--restart 2:close:1 --restart 3:at:12` | **no** | 0 (480,200) | both killed and back up |

`audit-only` shows the signed-vote log and the auditor raise no false alarm on an
honest run (non-fork goodput 1,662 cps, p50/p95 141/695 ms; single run, not a
performance record: the log adds one write per signed vote).

## What the restart run found

Both restarts fired: PR3 SIGKILLed 12 s into the spam and back up 2.7 s later;
PR2 SIGKILLed in round 0 of the epoch-1 close and back up 6.0 s later. Neither
restarted node signed anything its earlier lifetime contradicts (0 conflicts
across restarts).

The run did not settle because **a restarted validator never rejoins the epoch
protocol**. After the restart PR2 and PR3 emit only `EPOCH_RECONCILED` lines
(serving reconciliation of epoch 0 from the restored reports) and never
`EPOCH_START`: the epoch clock T0 is set by the `epoch_start` RPC, which
nanospam sends once at setup, and it is not persisted; decided epoch values
and the epoch ledger are not persisted either, and there is no checkpoint
transfer to catch up with. Both nodes stay in epoch 0 (`current_epoch=0`,
4,857 and 9,722 pending elections). The four other nodes decided epochs 0 and 1
but the epoch-2 close never finalized (two of six validators missing their
reports), so cementing stopped at 43,938 of 45,065 blocks.

This is the restart departure the paper lists ("the restart path is exercised
by unit tests only"), now reproduced end to end. Fixing it needs, on the node:
T0 and the decided epoch values persisted (signing store), the epoch ledger
rebuilt from them at start, and catch-up for epochs decided while down
(checkpoint transfer). Step 2 of the plan.

Diagnose with the per-node tag: every diagnostic line now ends with
`node=prN t=<ms>`; nanospam logs `RAI_RESTART_{KILLED,UP,RECONNECTED,SKIPPED,MISSED,FAILED}`.

## Step 2: restart rejoin (`step2/`, build `1e1dc5e58`)

Three commits make a SIGKILLed validator rejoin:

- `12d3bbf0e` durable epoch records: T0 (unix ms) with the genesis frontiers and
  history, per decided epoch the close outcome, `d_e` and the committee
  frontiers, and the latest two decided states; `restore_epochs` replays them
  (`EPOCH_RESTORED`).
- `969dbde2c` checkpoint catch-up: an epoch closed here for 1 s without its
  value derived fetches `S_e` by `d_e` (`CheckpointReq`/`CheckpointReply`,
  60 KB chunks) and adopts the certified value (`EPOCH_CATCH_UP`,
  `EPOCH_CAUGHT_UP`). Needed because the other replicas release an epoch's
  reports once N-f successors installed it, so a node one close behind can no
  longer derive.
- `1e1dc5e58` an unready close solicits rounds 0-3 after 2 s (the certificate
  of a close missed while down), and a replica that sees members holding more
  than f of the committee's weight voting in later epochs ends its epoch at
  once (`EPOCH_FOLLOW_AHEAD`); `final_state` lists restored decided closes.

| Run | Extras | Settled | Epochs agreed | Audit conflicts (slots) | Non-fork cps | p50/p95/p99 ms |
|---|---|---|---|---|---|---|
| `audit-only` | `--audit-votes` | yes | 0-3 | 0 (529,071) | 1,733 | 172 / 664 / 897 |
| `restart-at` | `--restart 3:at:12` | yes | 0-9 | 0 (645,621) | 1,526 | 210 / 1,017 / 1,337 |
| `restart-both` | `--restart 2:close:1 --restart 3:at:12` | yes | 0-7 | 0 (654,027) | 1,388 | 177 / 909 / 1,608 |

Single runs each, quiet host. In `restart-both` PR2 (killed in round 0 of the
epoch-1 close, down 6.2 s) restored epoch 2 with the epoch-1 close open,
followed the members ahead, learned the epoch-1 certificate by solicitation and
fetched `S_1` (3.3 MB in 14 ms). Catch-up also fired on nodes that were never
restarted, when they fell one close behind (PR0 in an earlier run, PR4 here):
the release rule strands any laggard, restarted or not. Follow-ahead never
fired in `audit-only`.

Not covered yet: power loss (the signing store still uses the ledger's
`nosync_unsafe` flags), close-election votes are still not persisted, and a
restart before the first decided record of the genesis (`Z`) is written loses
the epochs.
