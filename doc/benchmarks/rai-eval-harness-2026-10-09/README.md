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
