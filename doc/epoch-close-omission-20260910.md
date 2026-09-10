# Epoch-close omission: concrete reproduction and cause

Status: snapshot membership fix implemented. The 4,500-block / 90 blocks/s rerun passed with matching hashes on all six PRs for exactly two epochs; its data was deleted. The ramp subsequently passed through 34,173 blocks / 684 blocks/s. The epoch-1 latency investigation and its validated scheduling correction are recorded in `epoch-one-latency-20260910.md`.

## Reproduction

RAI release build at base commit `dfc7ddf9b159cf42f28e32676c33c8360a6fd13c`, with opt-in diagnostic tracing. Six PRs, 40-second epochs, 5% forks, no priority probes, 4,500 requested blocks/accounts, target 90 blocks/s. The failure occurs during the first epoch close, before this requested run can finish. This is distinct from the earlier 2,250-block drain stall.

Four epoch-close threads panic with:

```
epoch close omitted finalized block: epoch=0
hash=9F8B06922B0A72ED879172D13F1BD54FD02453DCA3E2A9B85324B5E1BDA0AEAD
```

That panic poisons the AEC lock. Vote processing, election scheduling and other workers subsequently panic on the poisoned lock. The remaining two close-election workers stay in round 1 without the missing peers' votes. These secondary symptoms are consequences, not the initial cause.

## Exact competing snapshots

| Snapshot | Candidate ID | Members | Contains the omitted block |
|---|---|---:|---|
| Older | `A3CE645E02471DF437A4CD931BE0DEB1C231D44260B3B40CD4B11FA5763000D0` | 3,506 | No |
| Newer | `F32EB770A7EFE5FCFBB03B48B017A2DA638678302ED57645DC2B0424173AD7CA` | 3,507 | Yes |

The set difference is exactly the block above. PR0, PR1, PR3 and PR4 first proposed the older snapshot; PR2 and PR5 proposed the newer snapshot. Four equal-weight FIRST votes satisfy the 62% notarization threshold. Four FINAL votes then finalize the older close candidate. Two FIRST votes for the newer snapshot do not meet the 38%-plus-one second-look threshold.

## PR0 timeline (UTC)

| Time | Event |
|---|---|
| 11:19:29.883197 | Block reaches notarization threshold locally. |
| 11:19:29.915550 | PR0 signs FINAL for the block. |
| 11:19:29.992361 | Drain condition reports complete. |
| 11:19:30.006312 | PR0 proposes the 3,506-block snapshot, excluding the block. |
| 11:19:30.092382 | PR0 signs FINAL for that excluding snapshot. |
| 11:19:30.103043 | PR0 receives enough block votes to observe block finalization. |
| 11:19:30.208119 | PR0 selects the excluding close candidate. |
| 11:19:30.321936 | AEC close assertion detects the omitted finalized block and panics. |

PR1, PR3 and PR4 sign their close FINAL votes after they have already observed block finalization. PR0 and PR4 sign both the block FINAL and the excluding close FINAL. A PR may finalize a block absent from its own proposal: proposal membership is not the invariant. The violation is that an already finalized block is absent from the subsequently decided epoch hash. The reverse ordering must also be protected: after an epoch hash is decided, excluded blocks cannot newly finalize in that epoch.

## Causal code path

1. `ActiveElectionsContainer::pending_epoch_drain` considers a required election terminated as soon as `has_quorum()` (notarization), confirmation, or timeout is present. It does not require finalization or ledger cementation.
2. `Ledger::epoch_close_candidate` builds membership from `consensus_epochs`, which is populated through ledger confirmation/cementation. A notarized block can therefore satisfy the drain condition while still being absent from the close snapshot.
3. `Ledger::epoch_close_candidate_valid` checks dependency closure and inclusion of previously **closed** membership. It does not require inclusion of newly finalized blocks or local outstanding FINAL commitments for the epoch being closed. It accepts the older snapshot even after the newer block is cemented.
4. `State::valid` caches a successful validation in `Candidate::validated`. Later finalization does not invalidate that cache. Removing the cache alone is insufficient because the underlying fresh validation also accepts the omission.
5. The close-election decision path does not consult AEC finalized membership before returning the selected snapshot. Workload finalization and epoch-hash decision are processed separately. This allows a certificate for a snapshot omitting a now-finalized block to be treated as a valid decision. The important missing coordination is at finalization/decision, not a restriction that a PR must finalize only blocks in its own proposal.
6. `State::drive` decides the older close candidate. Only afterward does `AecService::close_epoch` call `assert_epoch_close` under the AEC write lock. This catches the contradiction but cannot undo the certified close decision, and panicking poisons the shared lock.

The epoch hash represents consensus ledger state, including notarized and finalized blocks and notarized forks. B was already notarized when draining completed, so its exclusion was a snapshot-construction defect. The snapshot source must not wait for finalization or cementation.

## Implemented correction

Certificate application now synchronously registers notarized block payloads and dependencies in the ledger's consensus snapshot index, under the same AEC lock used by draining. Snapshot construction combines this index with cemented dependencies and prior canonical membership. Validation accepts verified notarized payloads, including forks absent from the account ledger, and checks dependency inclusion. Timeout routing hashes are not block members. Canonical membership is enumerated independently of cementation metadata so uncemented forks survive subsequent closes and ledger reopen.

This preserves the distinction between local proposals and the decided hash. It does not make local proposals immutable membership limits, wait for every notarized block to finalize, or alter a digest after certification. Existing finalization omission assertions remain enabled.

## Deterministic regression

`node/src/consensus/epoch_closer.rs::tests::epoch_close_must_include_late_finalized_block` adapts the existing split-snapshot test to the observed 4/2 split. It records B’s notarization before taking four snapshots, cements B before taking the remaining two, then runs signed close votes. It asserts that both pre-cementation and post-cementation snapshots, and every resulting decision, include B. The old cementation-only constructor fails the pre-cementation assertion.

```sh
cargo test -p rsnano_node --lib --features rai_protocol epoch_close_must_include_late_finalized_block -- --nocapture
```

The original injected 4/2 cemented-snapshot reproduction failed in 0.26 seconds with `certified close omitted a block already finalized in this epoch`. The updated regression exercises the corrected snapshot source: notarization precedes the snapshot, finalization follows it. Ledger tests additionally cover two notarized forks, dependency omission, later closes, and canonical membership after reopen.

## Evidence

All paths below are local:

- `/private/tmp/nanospam-debug-20260910/repro.log`: six-node failure and first panic.
- `/private/tmp/nanospam-debug-20260910/trace/18811.jsonl` through `18816.jsonl`: per-node proposals, signed close votes, applied workload votes, decisions, and omission diagnostics. PID order maps to PR0 through PR5.
- `/private/tmp/nanospam-debug-20260910/failure-timeline.json`: extracted timestamps for each PR.
- `/private/tmp/nanospam-debug-20260910/regression-test.log`: deterministic test failure.
- `/private/tmp/nanospam-debug-20260910/repro-data/`: preserved ledgers and configuration for the failed run. Test processes have been stopped.
- `/private/tmp/nanospam-ramp-20260910/`: original sweep evidence, preserved separately.

## Reporting work

The requested low-overhead reporting uses a compact PR0-only `election_outcome` WebSocket subscription. It records publication time before the first network write and reports fork/non-fork termination and finalization counts, mean, p95 and maximum latency by voting epoch. Replays and republication do not reset times. Full vote tracing and audit are unnecessary for these metrics; they remain diagnostic-only. This changes observation, not consensus decisions.

## Corrected 4,500-block rerun

Six PRs, 40-second epoch duration, exactly two verified epochs, 5% forks, no priority probes, accounts = blocks = 4,500, target rate 90 blocks/s. Full trace and audit disabled. Exit 0; no panic. All node ledgers for this successful rerun were deleted.

| Epoch | Roots | Terminated | Mean / p95 ms | Finalized | Mean / p95 ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 3471 | 63.1 / 95.8 | 3451 | 77.2 / 104.9 |
| 0 | Fork | 194 | 139.2 / 180.0 | 0 | — |
| 1 | Nonfork | 819 | 116.5 / 191.4 | 818 | 146.2 / 242.1 |
| 1 | Fork | 101 | 13165.3 / 35693.9 | 7 | 4655.1 / 5487.0 |

Counts are workload election roots, deduplicated within each epoch; a root can appear in both epochs. Timings measure first publication to PR0 WebSocket receipt. The 60.05-second workload observation recorded 4,276 account-ledger confirmations (71.21/s); this differs from consensus termination counts because notarized forks need not cement.

Results including maximum latencies: `/private/tmp/nanospam-fix-20260910/b4500-r90.json`. Log: `/private/tmp/nanospam-fix-20260910/b4500-r90.log`.

## Validation checkpoint

Epoch-close tests: 11 passed. Drain/snapshot integration regression: passed. Ledger epoch-closure tests: 2 passed. Vote-generator recovery tests, including the cross-epoch batching regression: 5 passed. Nanospam publication/epoch timing regression: passed. RAI release builds and feature-disabled checks passed before the final scheduling correction; the corrected RAI release passed the controlled six-PR rerun.

The ramp passed 4,500/90, 6,750/135, 10,125/203, 15,188/304, 22,782/456, and 34,173/684 blocks/rate. Every successful run verified exactly two epochs and deleted its node data. Detailed results are in `nanospam-snapshot-fix-results-20260910.md`. The 51,260/1,026 run had not started at this commit checkpoint.
