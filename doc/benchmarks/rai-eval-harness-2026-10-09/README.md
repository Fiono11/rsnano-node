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
restart before the epochs-started record (`Z`) is written loses
the epochs.

## Step 3: durable signing and its cost (`step3/`)

Commits: `fdd36eb8b` (`signing_sync = none | fsync | full`, default fsync;
close votes persisted per round as `C`; no record-less final replies once the
epochs run), `15675ecd3` (`SIGNING_WRITES` timings), `3594d6753` (decided
states and evidence moved to `epoch_records.ldb` with the ledger's setting;
old-epoch deletions in 2,000-key transactions), `9f892a841` (catch-up by the
certified value's hash; a close open for 2 s solicits rounds 0-3).

On this Mac LMDB's sync is `fsync` (`mdb.c:128`), measured at 0.05 ms per call:
APFS hands the data to the drive without waiting for its media. `F_FULLFSYNC`,
which does wait, measured 4.0 ms; mode `full` issues it after every signing write.

fork5, 6 equal-weight PRs, 45,000 blocks at 2,000/s, two runs per arm in
alternating order, quiet host. Non-fork goodput (blocks/s) and latency:

| Mode | `sync-ab-before-split` (`fdd36eb8b`) cps, p50 / p95 ms | `sync-ab` (`3594d6753`) cps, p50 / p95 ms |
|---|---|---|
| none | 1,737, 176 / 1,031 · 1,717, 164 / 568 | 1,765, 173 / 668 · 1,820, 139 / 677 |
| fsync | 1,740, 334 / 1,606 · 1,776, 346 / 1,244 | 1,802, 282 / 866 · 1,688, 310 / 1,400 |
| full | 1,740, 348 / 1,908 · 1,748, 331 / 2,226 | 1,763, 409 / 1,993 · 1,777, 304 / 1,161 |

Goodput is unchanged; synced signing costs about +130 ms at the median and up
to 2x at p95. The `SIGNING_WRITES` timings explain the first table: the vote
batches themselves took 1.4 ms on average under fsync, but the voter waited
for LMDB's one writer behind synced evidence batches (max 443 ms) and
boundary deletions (max 426 ms). With those moved out the longest signing
write fell from 464 to 172 ms.

`restarts/` (`9f892a841`, fsync): `--restart 2:close:1 --restart 3:at:12`
twice, both settled with every epoch's value equal on all six nodes and no
equivocation (0 conflicts); catch-up fired 1 and 4 times, follow-ahead 2 and
7 times. `fsync-perf` there: 1,786 cps, p50 / p95 273 / 935 ms. Two earlier
restart runs under fsync, before `9f892a841`, did not settle: a node left far
behind could not fetch states whose proposals were no longer repeated, and a
ready close never solicited the round the others had certified.

Still open: a restarted node takes 9-15 s to answer RPC after a respawn (2-3 s
in most earlier runs, 11 s once without sync; not attributed); the parent
records `P` are never forgotten.
