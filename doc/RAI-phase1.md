# RAI Phase 1

4 October 2026

## Commit 6: equal-weight committees

The first Phase 1 change ports the membership and configuration portions of `79ece3fbc` and `e1f271c49`, adapted to Phase 0 and Fast Archipelago v1.2. It does not import the source branches' close or checkpoint-finalization changes.

`committee_model = "equal_weight"` with `committee_f` and `committee_p` selects the largest N = 3f + 2p + 1 positive delegated balances, breaking ties by public key. Each selected identity has weight one. Genesis and derived committees use this policy; finalized delegation changes retain the existing two-epoch lag. Setup before epoch activation remains stake weighted. The default stays `weighted`, and feature-off node behavior ignores the new policy.

Account/report thresholds are q = 2f+p+1, fast = N−p, recovery support = f+p+1 and reports = N−f. `CheckpointThresholds` separately supplies election Q, F_fast, P and W, validates N, and computes the v1.2 dynamic recovery support m−f−p only for Q ≤ m ≤ N. It does not implement recovery or elections yet.

Unlike the source port, an undersized equal-weight membership does not lower thresholds: it logs an error and produces an inert committee with no voting members and the configured thresholds. Direct equal-weight construction rejects incorrect distinct-member counts. Equal-weight committee digests bind the fault budgets and membership policy; historical weighted digests remain unchanged.

Node TOML rejects unknown model names. Nanospam exposes `--committee-model equal_weight --committee-f 1 --committee-p 1` and requires `--prs` to equal N. No checkpoint-election wire messages or services are introduced.

## Validation

The commit is checked with formatting, feature-on and feature-off `cargo check --tests`, the feature-off workspace library suite, the feature-on node/messages/types library suites, and the nanospam CLI regression. Results are recorded in [phase1-commit6-validation.json](phase1-commit6-validation.json).

New tests exercise exact certificate formation, outsider exclusion, duplicate and undersized membership, deterministic balance/identity selection, fault-budget digest binding, both simulator populations, dynamic recovery thresholds, configuration round trips, feature-off isolation and committee delegation lag. Gate B remains pending commits 7–11; no performance claim is made for this change.

## Commit 7: first votes and recovery certificates

The feature-gated `consensus/checkpoint_election` module now provides pure logic for signed first votes, fast certificates, and both v1.2 recovery-certificate forms. Signatures bind a domain tag, the full instance, rank, signer, value and typed legal-introduction reference. Rank zero requires proposal ancestry; higher ranks require a previous B or fast certificate. Equal-weight membership and the committee digest are checked when creating the verification context.

A recovery snapshot retains one validated vote per signer. Fast detection separately retains signed votes per signer and value, so an equivocator's earlier vote cannot hide a valid fast certificate for another value. Missing application payloads or proof dependencies are parked and retried; invalid signatures are rejected before parking, and dependencies found invalid are discarded.

Recovery uses the changing threshold m−f−p for Q ≤ m ≤ N. A singleton may resolve at Q. An ambiguous snapshot waits, even if one value is larger. An empty snapshot needs independently verified ordinary R evidence and its recomputed maximum; it never substitutes the largest first-voted value. A resolved certificate can be verified by another node without changing that node's immutable first votes. Fast and recovery proof records have serde round trips; these are not the network envelopes planned for commit 9.

Two explicit interfaces remain unwired. `CheckpointEvidence` must validate RAI application values, complete legal-introduction ancestry and ordinary R certificates including complementing witnesses. There is no production implementation or permissive default; commit 8 supplies the R/A/B verifier and commit 10 connects application validation. `FirstVoteJournal` requires durable, atomic insertion by (instance, rank, signer) before a vote is released; an existing slot returns the original vote without invoking the signing closure. Tests exercise the contract with an in-memory journal, including uncertain write outcomes; actual disk persistence remains commit 10.

Eleven deterministic tests cover signed-field tampering, rank/instance replay, membership, ancestry binding, immutable signing, pending evidence, equivocation, both recovery forms, certificate verification/serialization, and the report's hidden-fast example. Bounded exhaustive tests cover n=6 and n=9: every fast-certificate identity set, recovery identity subset and permitted conflicting-vote subset for hidden-fast preservation; integer partitions of snapshot support for uniqueness at P; and explicit ambiguity at P−1. These are static recovery checks, not a full R/A/B model checker or proof of termination. See [the commit 7 validation record](phase1-commit7-validation.json).

## Next

Commit 8 adds the source-faithful R/A/B state machine and complete ancestry/witness verifier, persistent evidence relay logic, and leaderless synchronizer with simulator evidence. Before node wiring, resolve the plan's finite proposal-universe and selected-R-rank obligations and define resource bounds for pending/relayed evidence. The existing `CheckpointElection` service still has no implementation; no epoch closes end to end yet. The v1.2 conformance and simulator exit criteria still apply before commit 10.
