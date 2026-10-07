# RAI Phase 1: the Kudzu close

7 October 2026

On 7 October 2026 the checkpoint election went back to the Kudzu close of `rai_kudzu`, the joint epoch election the paper measured, instead of Fast Archipelago. The Fast Archipelago commits stay on `rai_leaderless`; this branch, `rai_kudzu_close`, continues from commit 6.

## Commit 6: equal-weight committees (`313068c9c`)

Ports the membership and configuration portions of `79ece3fbc` and `e1f271c49`. `committee_model = "equal_weight"` with `committee_f` and `committee_p` selects the largest N = 3f + 2p + 1 positive delegated balances, breaking ties by public key, each with weight one. Genesis and derived committees use it; delegation changes keep the two-epoch lag; setup before the epochs stays stake weighted. Account and report thresholds are q = 2f+p+1, fast = N−p, many = f+p+1 and reports = N−f. An undersized membership produces an inert committee instead of lowering thresholds. Nanospam exposes `--committee-model equal_weight --committee-f 1 --committee-p 1`.

The commit also added `CheckpointThresholds` for Fast Archipelago. Commit 8 removes them; the equal-weight constructor now checks the population directly.

## Commit 7: Kudzu vote kinds and slot rules (`918905fef`)

- `VoteKind` gains Notar, Timeout and Abstain, carried in the vote timestamp's duration bits (0xD, 0xE, 0xC). They are parsed only with `rai_protocol`; without it every non-final vote reads as a first vote, so the legacy wire format is unchanged.
- `single_support.rs` returns to `kudzu.rs` with rai_kudzu's epoch-slot rules: timeout and cross-committee conflict certificates, three notarization votes per representative, second looks (`many_votes`), the line-32 timeout rule with maxVotes over non-timeout blocks, and the settled predicate that requires termination outside account domains. Commit 6's equal-weight thresholds stay.
- Account elections keep single support: they return `Ignored` for every kind but first and final, and the vote records skip the close kinds.
- Notarization, timeout and abstain vote generators are created with the feature on only.

## Commit 8: the close election (`2464504bf`)

- `epoch_close.rs` from rai_kudzu, without the close-proof retention: rounds, leaders at (e + r) mod n over the union of the close committees, proposal validity (a valid parent chain with skip evidence, copy-parent payload, slot binding), second looks, the abstain at Δ_E, and the exit final vote.
- Close rounds travel in the vote's epoch field (`ConsensusEpoch::close_round`, top bit set). A close vote for an epoch this node has not left is `Indeterminate`, waits in the vote cache, and is replayed when the node advances (`required_epoch`).
- `EpochProp` (message 0x16) carries the leader's `X = (h_p, Q_e, d_e)`. The epoch decision service marks a close ready once it can derive a value from N−f usable reports, builds the leader's proposal, validates received proposals by re-deriving `BuildState`, and repeats its own proposals every 200 ms for the last four epochs.
- The container creates a close when it leaves an epoch, ticks the closes in epoch order, counts close votes in `EpochCommittees::for_close` (C(e−2) jointly with C(e−1)), recounts them when a committee is derived, and installs the decided state through `install_decided_checkpoint`. An ended epoch is left once the close of the epoch before has a certificate.
- `close_round_timeout_ms` (default 2,000) in `[node.active_elections]`; `final_state` reports each epoch's close (ready, value, started, round, closed value and round); stats `close_rounds` and `close_conflicts`.
- Not restored, as Phase 0 decided: count-based epochs, following votes into later epochs, `epoch_advance`, close proofs and checkpoint transfer.

## Validation

- `cargo fmt --all --check`; `cargo check --tests` with and without `rsnano_node/rai_protocol`, for each of commits 7 and 8 on its own.
- Feature-off workspace library suite: passed.
- Feature-on library suites after commit 8: node 729, messages 85, types 132 tests passed.
- Feature-on integration suites: `tests::election` 8 (including the new `an_epoch_closes_on_the_report_of_its_committee`, where one node reports, proposes, votes and installs epoch 0's state) and `tests::vote_processor` 5.
- New container tests: the close runs once its epoch is left, closes run in epoch order, and an early close vote waits in the vote cache.

## Gate B

Gate B runs all-online fork0 with six equal-weight representatives, 2,000 blocks/s, 45,000 blocks and 8 s epochs: `tools/rai/run_gate_b.py <output>`.

**Passed after Phase 2** ([record](benchmarks/rai-gate-b-2026-10-07/README.md)). Before reconciliation, epoch 0's six reports at full load had five different certified roots, only 1–2 were usable of the 5 needed, and the close never became ready; a low-load smoke closed epochs 0 to 2 and then stalled the same way. With the rateless reconciliation of Phase 2 (`172460c73`) the strict quiet-host run settled: 45,065 blocks cemented on all six nodes, epochs 0 to 2 decided in round 0 with one value everywhere, 1,931 blocks/s and p50 87–89 ms against Gate A's 1,973 and 88–89. The last close certificate came 1.1 to 1.75 s after the epoch was left.
