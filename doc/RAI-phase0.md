# RAI phase 0

This working-tree implementation follows phase 0 of [the implementation plan](RAI-implementation-plan.md). The import is based on `rai_kudzu` at `8aaf98c2c`, over `develop` at `ff2b327105`. No source branches were merged and no phase 1–3 ports were applied.

The five commits separate wire formats; account voting and timed epochs; report exchange and checkpoint installation; feature-off isolation; and the harness with benchmark evidence. Shared certified-state and epoch-ledger definitions land with account voting because the boundary code depends on them. Report networking and the decision-installation entry point follow in the third commit. Each commit is checked with both feature configurations before creation; the [validation record](benchmarks/rai-phase0-2026-10-02/validation.json) lists the results.

## Implemented

- The `rai_protocol` feature propagates through types, messages, node, RPC server, daemon and CLI. Votes and confirmation requests carry an epoch only with that feature. Account votes have only `First` and `Final` kinds.
- Single-support voting, committee weights, account-slot vote locks, timed epoch boundaries, immutable boundary reports, certificate evidence and epoch-aware vote routing are imported. The voting implementation is `node/src/consensus/election/single_support.rs`.
- Report headers use message ID `0x13`. A report becomes usable only when both signed roots match locally reconstructed contents. Phase 0 has no remote difference or sketch exchange.
- Candidate selection, BuildState validation and checkpoint installation are retained. `CheckpointElection` supplies a decided value and proof; its verifier and local value validation must both succeed before installation. **There is no checkpoint-election implementation in phase 0.** Ending an epoch cannot itself decide its checkpoint.
- The Kudzu close, close vote kinds, round encoding, count-based epoch advance, close-proof/checkpoint transfer, residual sketches and reconciliation messages are absent. Epoch values carry the epoch, selected report roots and derived state digest, without Kudzu placement rounds or parents.
- Feature-off paths preserve legacy vote validation, scheduler capacity behavior and ledger-event ordering. Received votes are not recorded for reports with the feature off.
- `epoch_start`, `final_state` and expanded `confirmation_info` RPCs, nanospam fault/delegation controls, and a bounded single-epoch benchmark runner are available.

The imported N/F report semantics, stake thresholds and rollback installation are intentional phase-0 behavior. Equal-weight committee thresholds, evidence binding, cumulative T, exact G and installation without rollback belong to later phases.

## Verification

All checks below passed. The feature-on library suites passed 683 node, 82 messages and 131 types tests; the integration suites passed seven election and five vote-processor tests.

Verification commands:

```sh
cargo fmt --all --check
cargo check --tests
cargo check --tests --features rsnano_node/rai_protocol
cargo test --lib -q
cargo test -p rsnano_node -p rsnano_messages -p rsnano_types --features rsnano_node/rai_protocol --lib
cargo test -p rsnano_node --features rai_protocol --test lib tests::election::
cargo test -p rsnano_node --features rai_protocol --test lib tests::vote_processor::
```

The workspace's HTTP tests and node integration tests need localhost socket access. The new pure report tests cover identical and different roots, both-root usability, invalid signatures, frozen snapshots and duplicate reporters. Container tests cover the timed boundary without a close implementation and direct, immutable checkpoint installation.

Feature-off regression tests also check that votes do not accumulate report records and that the scheduling source retains control of vacancy checks during cooldown.

The imported winner-change integration fixture was corrected to keep its majority above the certificate threshold after the send, and to inject its first vote without racing an automatically generated final vote.

## Gate A

```sh
cargo build --release -p rsnano_cli -p nanospam --features rsnano_cli/rai_protocol
tools/rai/run_matrix.sh doc/benchmarks/rai-phase0-2026-10-02/new-run
```

The runner uses six representatives, 45,000 primary blocks, up to 45,000 accounts and 2,000 blocks/s. Its epoch lasts longer than the entire deadline. It checks that all running validators cement every block and have identical final-state hashes while still in epoch zero. It saves binary SHA-256 values, client flags, host load, node configs, RPC counters and per-node finalization p50 values. The supplied matrix contains only phase 0's `fork0`; multi-epoch and fork gates require later phases.

A busy host is rejected before launching nodes. `--allow-busy` permits a correctness run but explicitly marks it ineligible for the performance gate. Performance requires comparison with the [recorded rai_kudzu baseline](benchmarks/rai-phase0-2026-10-02/baseline.json); a successful correctness run alone does not assert that comparison passed.

The [benchmark record](benchmarks/rai-phase0-2026-10-02/README.md) contains the successful six-node correctness run, blocked quiet-host attempts, and the strict quiet-host run of 4 October. **Gate A passed:** all six validators cemented the workload with identical final states in epoch zero, at 1,973 blocks/s and 88–89 ms finalization p50, compared with the historical baseline's 1,941 blocks/s and 88–90 ms.

## Preserved files

The stale untracked reports, attachment, bootstrap fetchers and epoch-lock RPC files identified in the plan were moved intact to `/tmp/rai-phase0-untracked-backup/`, preserving their relative paths. Existing papers and benchmark directories were left intact.
