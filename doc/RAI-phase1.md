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

## Next

Implement per-rank signed first votes and resolved recovery certificates (commit 7), followed by the source-faithful R/A/B state machine, persistent evidence relay and synchronizer (commit 8). The v1.2 conformance and simulator exit criteria in the implementation plan still apply before service wiring.
