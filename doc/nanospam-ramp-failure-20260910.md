# 51,260-block load-ramp checkpoint

**Artifact retention update:** the user deleted the historical `/private/tmp/nanospam-*` directories to recover disk space. Paths below describe historical evidence and are no longer available. New runs and regression logs are under `target/nanospam-debug-20260910/`. See [delta recovery and admission correction](epoch-close-delta-recovery-20260910.md) for subsequent changes and validation.

The resumed run at commit `267f1a623` failed to converge through epoch 1 within the existing close deadline. No higher load was started. The release build passed. The original run used six fresh PRs, 40-second epoch duration, 51,260 blocks/accounts, 1,026 blocks/s, 5% forks, and `--no-prio`. Full tracing and audit were disabled; compact PR0 outcomes and 500 ms PR0 block-count polling were enabled.

## Original run

`b51260-r1026` exited 1 after 459.01 seconds. All 51,260 blocks were published. The initial 60.01-second observation reported 36,551 account-ledger confirmations (609.09/s). These are not election termination counts. The close verifier failed before emitting `EPOCH_PERFORMANCE_RESULT`, so this run has no complete per-epoch latency table.

All six PRs eventually closed epoch 0 with digest `1B59C0124FB798C17039A74669D6B6308FAC2881A23F0D7A316D6218CBBF3F19` and 39,821 members. PR0–PR4 also closed epoch 1 with digest `1A874BC2C19B6AB31B55B5CB0C595C459CD601E7A36FE3B9A999C62CA77EB7DC` and 51,314 members. PR5 remained in voting/draining epoch 1 at the deadline. No panic was recorded.

The final epoch-1 drain diagnostic had seven active election roots. Each had only two or three representatives participating and no timeout eligibility. Pending counts were falling, so the evidence establishes deadline failure and very slow recovery, not a proven permanent deadlock. Some peers had already started voting epoch 2 while waiting for PR5; this is a failed two-epoch run, not a successful additional-epoch benchmark.

Read-only LMDB inspection found the same cemented portion of the epoch-0 snapshot on all six PRs; 3,217 canonical members were not cemented. Such members are permitted for notarized blocks and forks. The seven final pending epoch-1 winners were cemented in epoch 2 on PR0–PR4, absent from their canonical closed membership, and uncemented on PR5. This does not reproduce the earlier finalized-membership omission panic.

The publication interval ended before epoch 0 closed. There are no fresh post-close publication windows with which to assess epoch-1 latency normalization in this run.

Diagnostic compilation overlapped the later part of the already-observed slow recovery, beginning around 12:33 UTC. Treat the final deadline timing as potentially affected by that additional CPU load. A same-load diagnostic reproduction is required before assigning the slowdown to a specific implementation path.

## Preserved evidence

All paths are under `/private/tmp/nanospam-fix-20260910/`:

- `b51260-r1026.log`, `b51260-r1026.json`, and `b51260-r1026-pr0-counts.json`.
- `b51260-r1026-data/`: failed node data, preserved.
- `b51260-r1026-diagnostics/final-epoch1-drain.json`: all seven final pending roots, votes, and per-PR cemented/canonical membership.
- `b51260-r1026-diagnostics/final-pr*-epochs.txt`: read-only consensus metadata dumps after shutdown.
- `b51260-r1026-diagnostics/drain-examples.json`: parseable pending-root examples. Two interleaved diagnostic records could not be parsed.
- `dump-epochs.c` and `dump-epochs`: read-only LMDB metadata inspection utility and executable.

## Targeted diagnostic changes

`RAI_CLOSE_VALIDATION_DIAGNOSTICS=1` enables the first local membership-validation failure for each complete invalid snapshot and close-message queue/processing counters. Drain diagnostics now include PID and epoch. Assertions, certified membership, vote rules, thresholds, and deadlines are unchanged. The purpose is to distinguish missing locally verified block evidence from close-message queue pressure and slow processing. These diagnostics are not a consensus correction.

## Same-load diagnostic reproduction

`b51260-r1026-validation` used the same workload and enabled only the targeted diagnostics described above. No compilation ran concurrently. It was deliberately stopped with SIGTERM after 279.96 seconds, once the bottleneck was captured; exit -15 is a diagnostic interruption, not an expiration of the unchanged deadline. No epoch had closed. Its node data remains in `b51260-r1026-validation-data/`.

Every node reached the 4,096-packet close-input queue limit. The final parseable per-node counters were:

| PR / PID | Maximum close tick, ms | Maximum send phase, ms | Dropped close packets |
|---|---:|---:|---:|
| PR0 / 23252 | 82,860 | 76,768 | 4,251 |
| PR1 / 23254 | 94,203 | 86,798 | 3,768 |
| PR2 / 23255 | 77,724 | 69,989 | 4,511 |
| PR3 / 23256 | 98,022 | 92,286 | 4,600 |
| PR4 / 23257 | 85,109 | 81,831 | 3,075 |
| PR5 / 23258 | 104,261 | 78,838 | 4,479 |

These are wall durations under load, not isolated CPU benchmarks. The send phase includes acquiring the flooder and flooding all outgoing packets. The first large observed tick already spent 8,271 ms in that phase (9,505 ms total). The configured retransmission interval is 2 seconds, but ticks execute synchronously: long sends delay handling incoming votes and snapshot pages. The pending queue then overflows.

A one-second macOS stack sample of PID 23254 (PR1) placed 583 samples in the epoch-close flooding path. Of those, 365 were beneath `MessageSerializer::serialize`; 285 were beneath `Blake2Hash::encode_hex`. This identifies substantial repeated serialization work, not simply a socket wait.

The concrete code path is:

1. `State::packets` expands every complete retained candidate, plus the prior close archive, into full snapshot pages for retransmission.
2. `EpochCloser::tick` floods the entire outgoing vector synchronously before accepting the next batch of incoming close packets.
3. `MessageFlooder::flood_prs_and_some_non_prs` invokes `MessageSender::try_send` separately for each destination. Each invocation serializes the same message again.
4. `EpochClose` uses JSON serialization. Every hash is encoded by `encode_hex`, which formats each of its 32 bytes separately. The same snapshot page incurs this work repeatedly across peers and retransmissions.
5. Long send passes allow the bounded incoming queue to fill. Dropped pages/votes require further retransmission while normal workload recovery is also congested.

There is also concrete missing-block evidence. The 89 parseable validation diagnostics all reported `no_eligible_local_membership`: a complete snapshot named a hash absent from the node's eligible notarization index and cementation metadata. For example, PR2 lacked `400556A2F076EA839C3BDC4EC0ADB8A211C8AB58BFFE6ACFDF3911888CE5EC3B`, including its account-ledger payload. Receiving a hash manifest does not itself fetch or establish the missing block's notarization; validation correctly remains false until ordinary recovery supplies evidence.

At a single RPC checkpoint, PR0/PR2/PR3/PR5 respectively reported 6,120/6,342/6,275/6,265 request-aggregator overfilled batches and 677/353/273/2,420 incoming `confirm_ack` drops. The other two stats RPCs timed out. All six block-count RPCs responded. Thus congestion affected workload recovery as well as the close-input queue.

The 16 GiB host was under substantial memory pressure: a captured observation showed about 8.3 GiB occupied by compressed memory and about 584 MiB of swap in use. This is a contributor/confounder, not proof that serialization alone explains every delayed election. The targeted diagnostics and one-second profiler also add overhead; the reproduction is diagnostic evidence, not a latency comparison against successful ramp runs.

Evidence under `/private/tmp/nanospam-fix-20260910/`:

- `b51260-r1026-validation.log`, `.json`, and `-pr0-counts.json`.
- `b51260-r1026-validation-diagnostics/summary.json` and `events.json`: parsed timing, drop, and validation records; unparseable interleaved records are counted.
- `b51260-r1026-validation-diagnostics/pr*-rpc.json`: one read-only RPC checkpoint.
- `b51260-r1026-validation-diagnostics/host-state.json` and `stop-reason.json`.
- `validation-pr1-sample.txt`: stack sample of PR1/PID 23254.
- `validation-diagnostic-build-final.log`: successful RAI release build of all targeted diagnostics.

## Historical conclusion before delta recovery

The load ramp stops at 51,260/1,026. The original run failed the epoch-1 convergence deadline, and the reproduction demonstrates a snapshot transport bottleneck with queue loss and recovery congestion. No successful run was completed in this continuation, so there is no new successful per-epoch outcome table.

The next correction should remove repeated per-destination snapshot serialization and keep manifest transmission from monopolizing the close loop, then retest this same load with diagnostics disabled. Missing notarized payload recovery also needs attention. These are proposed follow-up corrections, not implemented or validated fixes. No vote threshold, deadline, membership assertion, or certified snapshot was changed.

Both failed/interrupted data directories are preserved. The original `kudzu.pdf` is untouched. The source changes in this continuation are diagnostic only.

Shutdown verification: the six diagnostic nodes remained alive more than 90 seconds after SIGTERM and were force-stopped with SIGKILL. Their data is preserved but shutdown was unclean. No benchmark or node processes remained afterward. The runner currently waits for nanospam but does not wait for every child to exit; future cleanup should account for this.
