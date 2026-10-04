# RAI on develop: implementation plan

Updated 4 October 2026 · Rui Morais

> Based on revision 35 of the Claude doc <https://claude.ai/artifact/AX9QE1benJ85L1hv46Xbpf>, exported 2 October 2026. This local revision incorporates *BFT-Archipelago with an R-Step Fast Path*, version 1.2 (3 October 2026), from [Fast_Archipelago.pdf](../Fast_Archipelago.pdf), and the completed Gate A. These edits have not been carried back to the Claude doc.
>
> Protocol source SHA-256: `56822aca1f4d72a7de594c315f0bf3ca27e105d47d22e324fbf954f973d800d5`. Section and theorem references for the election below refer to this version unless explicitly attributed to the original Algorithm 5.

## Decisions

This plan brings the RAI protocol of *RAI: Preserving Latent Finality Across Committee Handoffs* onto `develop` (ff2b32710) in 23 commits over four phases. Three decisions fix its shape:

1. **Base on `rai_kudzu` (8aaf98c2c), trimmed.** `develop` HEAD is rai_kudzu's merge base, so the import is a subset of its 52 commits, never a merge. About half of its 22.5k source lines are kept, about 3k lines are dead or off-paper and dropped, about 2.5k lines of T/G reconciliation are replaced, and about 2.5k lines of Kudzu close are left out.
2. **Report reconciliation uses a rateless IBLT** (Yang, Gilad, Alizadeh, SIGCOMM 2024) instead of the paper's pinned common projection. The reporter streams coded symbols of its frozen set; the requester subtracts its own view and peels. No shared root, no size estimate, no retry-on-failed-decode. This removes the exact failure that stalled rai_kudzu's fork runs: a node one entry short had no shared root and every difference request was refused.
3. **The checkpoint election is BFT-Archipelago with an R-Step Fast Path, version 1.2.** It replaces the paper's Kudzu election and the earlier first-vote/initial-sample design in this plan. Every rank runs R → recovery → A → B, with an independent fast listener: n−p matching immutable first votes from that rank decide directly in R. Recovery uses `Cand(S) = {v : count_S(v) ≥ |S|−f−p}` and permits A only when that set is empty or a singleton. A/B retain Algorithm 5's mutable response-on-delivery services at quorum n−f−p. The revision states safety and conditional leaderless-termination results for independent budgets of f Byzantine replicas and p temporarily suspended correct replicas; the latter requires recurring replicas, persistent relay and clean synchronized rounds. The Kudzu close is never imported. Account voting remains the paper's single-support rule.

Two facts from the code audit drive the phase order. rai_kudzu's reports are not the paper's reports: they have only N and F tags, a per-epoch T instead of the cumulative frozen ledger, and a G keyed by vote kind. The paper-conformant definitions, BuildState rules and the fork-convergence fixes are commits on `rai-cross-epoch-minimal` (b864602ed, the v43 the paper measured), and are ported in Phase 3; the equal-weight thresholds come from the same branch but move up to commit 6, because the election needs them. Second, the election sits behind a small interface (usable reports and a validated candidate in, a decision with a proof out), so Phases 2 and 3 change what feeds it and how its result is installed, not the election.

Branches: `develop` (target), `rai_kudzu` (source of the import), `rai-cross-epoch-minimal` (source of the conformance ports), `rai-review-fixes` (five fork fixes not on either).

## Current status — 4 October 2026

**Scope update:** disk persistence is deferred at the user's request. Continue Phase 1 with in-memory per-instance/rank vote locks, R/A/B state and retained evidence. Commit 10 will wire the running-process implementation; durable restart recovery is a later task. No restart-safety claim applies to this interim configuration.

Phase 0 (commits 1–5) is implemented on `rai_leaderless` through `c6f3852ea`. **Gate A passed:** the strict quiet-host run cemented 45,065 blocks on all six validators with identical final states in epoch zero, at 1,973 blocks/s and 88–89 ms p50 versus the historical baseline's 1,941 blocks/s and 88–90 ms. See the [benchmark record](benchmarks/rai-phase0-2026-10-02/README.md) for evidence and comparison limits. Phase 1 has started: commit 6 implements equal-weight membership, integer thresholds, configuration and nanospam controls. See [the Phase 1 record](RAI-phase1.md). Commit 7 now provides pure per-rank first-vote and recovery-certificate logic with explicit evidence-verifier and durable-journal interfaces. Commits 8–11 and Phases 2–3 remain; the checkpoint-election service is still unwired.

## Sources: keep, drop, port

**Kept from rai_kudzu** (paper mechanisms 1, 2, 3, 5, 6, 7; line counts are added lines against develop):

| Area | Files | Lines |
| --- | --- | --- |
| Feature flag, epoch on the wire | types/consensus_epoch.rs, types/vote.rs, messages/{confirm_req, confirm_ack, publish, network_filter, message_deserializer}.rs | ~900 |
| Single-support voting, epochs, committees | election/{kudzu, committee, election, election_id, election_state, block_tallies}.rs, active_elections/{slot_states, epoch_states, epoch_committees, root_container, vote_router, apply_vote_helper}.rs | ~5,000 |
| Boundary, freeze, install | active_elections_container.rs (A/B parts), aec_service.rs, aec_fact_processor.rs, ledger.rs roll_back_batch_unchecked | ~3,500 |
| Vote plumbing | vote_generator(s).rs, voting_scheduler.rs, aec_voter.rs, local_vote_history.rs, vote_cache/\*, vote_applier.rs, request_aggregator\*.rs, confirm_req_sender.rs, confirmation_solicitor\*.rs, aec_fork_inserter.rs, winner_block_broadcaster.rs | ~1,600 |
| Reports, epoch value, candidate derivation | messages/report.rs (Report only), reports/{mod, report_service, report_plugin}.rs, epoch_decision.rs (can_derive, derive_fresh and validate only), certified_state.rs core, epoch_ledger.rs, epoch_value.rs | ~3,900 |
| Required RPC | epoch_start (the only caller of start_epochs) | ~70 |
| Harness (optional) | tools/nanospam/\*, final_state and confirmation_info RPCs, diagnostics.rs, aec_stats.rs | ~1,700 |

**Dropped from rai_kudzu:**

- The Kudzu close: epoch_close.rs, messages/epoch_prop.rs (EpochProp 0x16), propose, handle_proposal and repeat_proposals in epoch_decision.rs, VoteKind Notar/Timeout/Abstain, as_close_round and required_epoch, the three extra vote generators, the close arms in the container, solicitor and request aggregator, and the close paths of kudzu.rs, which lands as single_support.rs. An account election already ignores every vote kind but first and final. About 2,500 lines. Replaced in Phase 1.
- The close-proof and checkpoint-transfer path: message types 0x19 to 0x1c, messages/{close_proof, checkpoint}.rs, reports/{checkpoint, close_proof}.rs, EpochLedger::{checkpoint_entries, from_checkpoint_entries}, the retain_\*/close_proof wrappers. Nothing sends CloseProofReq and the transferred ledger is never read. About 1,000 lines.
- The reconciliation path: ReconReq 0x14, ReconReply 0x15, ResidualSketchReq/Reply 0x17/0x18, election/sketch.rs, CertifiedState::{difference, apply, CertifiedDelta}, shared_sources, the history bridge with MAX_HISTORY, ReconRefusal. About 2,500 lines including 900 of tests. Replaced in Phase 2.
- Off-paper knobs: count-based epoch advance, the epoch_advance RPC, observe_epoch, epoch_terminated_elections, final_state.rs close_value, and the joint close committee (C(e−2) plus C(e−1) with a conflict rule: Certificates::conflict, committees_notarize_different_values, RoundConflict, close_conflicts). Only C(e−2) votes in the new election, as §5 requires.
- Account-domain leftovers: ResidualKind::Notar, ElectionState::TimedOut, stats.timed_out.

**Ported from rai-cross-epoch-minimal** (Phase 3, except the equal-weight model in commit 6; all independent of the reconciliation mechanism): dd1e743e4 and c0f4c0057 (R tag, F > N > R, cumulative frozen T, exact G = V \\ keys(T)); 85c076aac with ledger_evidence.rs and EvidenceReq 0x1d, fb09aee67, 755fcae88 (evidence verification, committee binding); 3c3bc45d6 and the epoch_ledger half of b607acd48 (BuildState per Figure 3); attachment.rs, the container half of b607acd48, 803d4157c, f0f78db53, 727493343 (attachment, overlap exceptions, provisional recheck, cross-epoch first-vote lock); 562fd2213, 16c0a6585 correctness half, 8b36f9cc0, b680c965f, 7f4f5bf00, checkpoint_installer.rs (installation without rollback); ed69aa3d5 (boundary stop); 79ece3fbc and e1f271c49 (equal-weight committee model); b864602ed plus a2873c7bf, cdf479fdc, 2d4184c59, 597fc3f69, 1a11543b4 from rai-review-fixes (fork convergence).

**Not ported:** the sketch commits (80c710b7f, b6946a31b, 6658e807c, c1e931849, 30c452080, the paging half of c9668a484), the lock-contention and diagnostics commits, and the unique-branch variant 93ab52b95, which has a documented counterexample.

## Verification and gates

Every commit passes the same five checks before it lands; benchmarks run only at the four gates.

**Per commit:**

- `cargo fmt --all --check`
- `cargo check --tests` with and without `--features rsnano_node/rai_protocol`
- `cargo test --lib -q` without the feature: develop's suite, unchanged
- `cargo test -p rsnano_node --features rai_protocol --lib` (also `-p rsnano_messages`, `-p rsnano_types`)
- new logic in a pure module with unit tests and deterministic keys (`PrivateKey::from(1)`), no nullable setup

**Benchmark protocol:** six equal-weight representatives, 2,000 primary blocks/s, 45,000 accounts, 8 s epochs, one arm at a time, quiet host (check `top` first: an editor file watcher once made five runs look six times worse). A fork run *settles* when every running validator has the same cemented state and the same three checkpoint decisions; finalizing every conflicting position is not required. Records go under doc/benchmarks with the node SHA-256 and client flags.

| Gate | After commit | Variants | Pass criterion |
| --- | --- | --- | --- |
| A | 5 | fork0 in one epoch longer than the run, so nothing closes | every block cemented on all validators; goodput and p50 within noise of rai_kudzu's own fork0 record |
| B | 11 | fork0, offline1 | settles; fast path taken in fork0; goodput and p50 within noise of Gate A; close duration recorded against the Kudzu leader gate on record (0.1 to 1.3 s) |
| C | 16 | fork0, fork5, offline1, byz1 | settles; zero reconciliation refusals; symbols per report near 1.35 × difference |
| D | 23 | the nine paper variants | all settle; non-fork p50 within noise of v43 (101 to 109 ms) |

Before commit 1, move the stale untracked copies out of the working tree (node/src/consensus/reports/, consensus/attachment.rs, bootstrap/block_pulls.rs, bootstrap/missing_block_fetcher.rs, the epoch_locks RPC files), since the imports check out the same paths.

```text
Phase 0: import, no Kudzu close   (commits 1 to 5)    -> Gate A: single epoch
Phase 1: Fast Archipelago election (commits 6 to 11)   -> Gate B: epochs close
Phase 2: rateless IBLT             (commits 12 to 16)  -> Gate C: four variants
Phase 3: v43 conformance ports     (commits 17 to 23)  -> Gate D: nine variants
```

Phases 0 to 3 run in sequence and each ends in a gate. The election's two logic commits (7 and 8) touch no node code. Phase 0 and Gate A are complete. Commits 6–7 are implemented. Commit 8 is in progress: volatile vote locks and pure A/B evaluation/witness helpers are implemented. The full R/A/B engine, recursive verifier, relay and synchronizer remain. The [certificate-persistence conformance finding](RAI-archipelago-v1.2-conformance.md) must be resolved before service wiring.

## Phase 0: import rai_kudzu, trimmed, without the Kudzu close (commits 1 to 5)

Five commits bring the trimmed rai_kudzu tree onto develop in dependency order; each compiles with the feature on and off, and commit 5 ends in Gate A. The Kudzu close is left out: commits 1 to 3 cut it from the files they bring over, and Phase 1 supplies the election.

| # | Commit | Content | ~Lines | Verification |
| --- | --- | --- | --- | --- |
| 1 | Feature flag and epoch on the wire | `rai_protocol` in types, messages, node, rpc/server, daemon, cli; ConsensusEpoch without as_close_round and required_epoch; VoteKind with First and Final only, and epoch in vote.rs; epoch in confirm_req; evidence flag in confirm_ack and publish; network_filter cutoff; message_deserializer; stats enums | 900 | types and messages tests both ways; wire format identical with the feature off |
| 2 | Single-support account voting and timed epochs | kudzu.rs as single_support.rs, committee.rs, election.rs, election_id, election_state, block_tallies, slot_states, epoch_states, epoch_committees, root_container, vote_router, apply_vote_helper, the container and aec_service, all vote plumbing, config toml, ledger roll_back_batch_unchecked, aec_fact_processor. Left out: the close rounds in the container, solicitor, request aggregator and vote generators, EpochCommittees::for_close, count-based advance, observe_epoch, epoch_terminated_elections, close-proof wrappers, ResidualKind::Notar in account slots, TimedOut | 10,000 | the kudzu, election and container unit tests that do not exercise the close; gated tests in node/tests election.rs and vote_processor.rs |
| 3 | Reports, epoch value, installation | messages/report.rs with Report only, reports/{mod, report_service, report_plugin}, can_derive, derive_fresh and validate from epoch_decision.rs, certified_state.rs core, epoch_ledger.rs, epoch_value.rs, node.rs wiring, network dispatch. A decided value enters through the `CheckpointElection` interface, which has no implementation until commit 10. Reconciliation reduced to the identical-root shortcut. Not imported: epoch_close.rs, epoch_prop.rs, propose, handle_proposal, repeat_proposals, ReconReq/Reply, ResidualSketch\*, sketch.rs, difference/apply, checkpoint.rs, close_proof.rs, messages 0x19 to 0x1c, the unread `transferred` field | 4,500 | report and build_state tests; installation tests fed a decided value directly |
| 4 | Feature off means develop behaviour | gate record_votes (VoteRecords grows without bound today), the vote_processor validate skip, the priority_scheduler early return, check_vacancy and vacancy, ledger_event_processor ordering | 100 | `cargo test --lib -q` equals develop; no `cfg!` left on a hot path |
| 5 | Harness | epoch_start RPC, final_state (without close_value) and confirmation_info RPCs, diagnostics.rs, nanospam (byzantine without the close vote kinds, offline, silent, delegation, epoch knobs), run_variant and run_matrix tools | 2,500 | **Gate A** |

With no close on the tree, an epoch that ends cannot be decided, so Gate A runs fork0 in a single epoch set longer than the run. It checks account voting, vote plumbing and the feature gating before any election code exists. Reports, BuildState and installation have only unit tests until Gate B.

## Phase 1: R-Step Fast-Path Archipelago checkpoint election (commits 6 to 11)

Six commits bring the equal-weight thresholds and version 1.2 election onto the trimmed tree and end in Gate B, the first run in which an epoch closes. Commits 7 and 8 are pure logic behind `CheckpointElection`, tested in a simulator. Commit 10 wires them in only after the exit criteria below are met. Line counts are estimates and must accommodate recovery, relay and synchronization.

**RAI binding.** The instance is I = (session, closing epoch e, H(S_{e−1}), committee digest of C(e−2)); only members of C(e−2) propose and respond. A value is v = (Q_reports, μ_e, d_e) as in the RAI paper's Equation 5, ordered by H(v). `Valid_I(v)` checks N−f distinct usable reports, the manifest and the BuildState digest. This is an application-validity condition in addition to version 1.2's authenticated rank-zero ancestry, not a claim that an honest replica proposed the value. Missing payloads or evidence remain pending until locally reconstructed and verified.

With n = 3f+2p+1, use the following thresholds. `F_fast` below is the report's F, distinguished from RAI's finality tag F; `P` is a worst-case recovery bound, not a mandatory initial sample size.

| Threshold | Size | n = 6 (f = p = 1) | Used for |
| --- | --- | --- | --- |
| Q | n−f−p = 2f+p+1 | 4 | ordinary R/A/B responses and strong complementing witnesses |
| F_fast | n−p = 3f+p+1 | 5 | same-rank fast certificate |
| P | n−f = 2f+2p+1 | 5 | recovery snapshot size guaranteed to have at most one candidate |
| W | f+1 | 2 | original Algorithm 5 weak complementing witnesses |
| recovery support | m−f−p, for Q ≤ m ≤ n | 2 at m=4; 3 at m=5 | membership in Cand(S) |

- *Candidate proposal.* As a RAI integration policy, each replica builds one rank-zero candidate from the N−f lowest-identity usable reports. It broadcasts the authenticated payload and starts R when N−f reports are usable and either all N are usable or Δ_c (initially 500 ms) has elapsed since the (N−f)th became usable. This is proposal timing, not a first-vote timer. Identical candidate sets help the fast path but do not guarantee identical first responses under every delivery schedule. The finite rank-zero proposal universe required by §8.3 must be made explicit in the verifier and reviewed under Byzantine proposal equivocation before wiring.
- *R at every rank.* Rank zero is a real R exchange; the old `FIRST_R0` → `Start(S)` → A(0) shortcut is removed. A legal rank-zero introduction has authenticated, application-valid proposal ancestry. At rank i>0 it carries a preceding-rank B certificate whose recomputed result carries the value, or FC_(i−1) for that value. Verify complete, finite predecessor ancestry. Update the ordinary certified R maximum as Algorithm 5 specifies and collect Q eligible R responses; do not freeze that maximum when recording a first vote.
- *First votes and fast decisions.* On its first valid R response for rank i, a correct responder persists one signed `FIRST(I, i, v, H(J))` record for the supported R value and its legal justification J before transmission. Include it in the ordinary R response. Later responses may carry a newer R maximum but must retain the same first vote for that rank. Bind the proof to the supported value, including when a previously observed maximum differs from the triggering request. The background listener counts distinct valid identities per value and rank; F_fast matching records form FC_i(v) and decide immediately, independently of recovery or A/B. Byzantine equivocation never counts an identity twice in one certificate or recovery snapshot.
- *Hidden-fast recovery before every A.* Start with m≥Q distinct, valid first-vote records for that rank and recompute `Cand(S) = {v : count_S(v) ≥ m−f−p}`. If it is `{x}`, form a transferable singleton RC_i(x) and use x. If empty, form an empty RC_i(x) carrying both S and ordinary Q-response R evidence whose certified maximum is x. If multiple candidates remain, do not choose a maximum or advance on a timeout: await another distinct first vote or import a valid resolved RC. At P records there is at most one candidate; do not require P when Q already resolves. Independently recheck the snapshot, candidate set, rank, signatures and legal ancestry for every imported RC.
- *A/B services.* Preserve Algorithm 5's bounded mutable A[i] and B[i] state updates and response-on-delivery behavior. A returns `(true,v)` only when all Q counted responses contain only v; otherwise it returns `(false,max union)`. B commits when all Q counted responses contain only `(true,v)`, adopts a valid true value if any counted response contains one, and otherwise adopts the maximum false value. Preserve learned true evidence and nondecreasing maxima; late responses must not recreate eliminated lower-only certificates. No prepare phase, `ABORTFAST`, `SEEN` phase, or sealed `AFINAL`/`BFINAL` response is introduced.
- *Certificate reliability.* Preserve the original Algorithm 5 partial and complementing certificate checks: W responses for the broadcast-admission checks and Q for the originating broadcasts behind an eligible response. W is an existing reliability threshold, not a new phase-admission protocol. Extend validation with resolved RC evidence for A and fast-certificate predecessors for R. Signature and provenance checks alone are insufficient. Missing witnesses stay pending and are rechecked; no witness shortcut may bypass recovery or legal ancestry.
- *Relay, persistence and installation.* Retain first votes per instance and rank, R/A/B safety state, and decisions in memory for the interim implementation. Durable storage before signature release remains deferred. Fairly relay unresolved historical first votes, RCs, predecessor evidence and decisions under rotating suspension, including after advancing or deciding. Do not garbage-collect evidence until a safe rule is established. A decision proof is FC_i(v) or a committing B certificate with the full recovery, ancestry and complementing evidence needed for independent verification. C(e−1) verifies the proof and `Valid_I`, then invokes installation. A decided replica stops proposing but continues response and evidence services.
- *Leaderless driver.* Retransmit the same signed requests without timeout votes. Rank advancement needs certified predecessor evidence, never a clock alone. Implement and demonstrate a synchronizer that eventually aligns at least Q correct participants in clean R/A/B ranks, with requests from awake correct participants delivered before their responses in clean A/B rounds (§2.1). Preserve response-on-delivery services within that round discipline; timers and retries alone do not establish this liveness premise.

**Proof status and limits.** Version 1.2 states same-rank fast uniqueness (Lemma 5.1), hidden-fast preservation (5.2), recovery uniqueness at P (5.4), and eventual recovery with recurring replicas and relay (5.6). It states same-rank fast/slow agreement (Theorem 6.4), full agreement (6.6), and authenticated-proposal validity (7.1). Its leaderless-termination result (8.5) uses independent f Byzantine and p temporary-suspension budgets, source certificate reliability, and a leaderless synchronizer producing clean ranks. The value-elimination argument (8.1–8.4) concerns clean ranks after recovery can proceed. It does **not** give a uniform post-GST time or round bound: an old first vote may be delayed for an arbitrarily long finite time. The optimistic R fast path takes two message delays, one request and one response (9.1). These are the revision's stated results; conformance review and simulation must establish that the implementation meets their premises, not treat the old padded-population proof as applicable.

**Conformance finding:** the [signed adoption-certificate trace](RAI-archipelago-v1.2-conformance.md) does not satisfy the unrestricted persistence claim in v1.2 Lemma 6.5 under the stated predecessor rules. This is not a conflicting-decision demonstration. Clarify the intended condition or revise the claim/proof before marking the review complete.

**Exit criteria before commit 10:**

1. Review commits 7–8 against version 1.2 §§2–4 and §11 and original Algorithm 5, line by line: legal R ancestry and rank selection, immutable first votes, the dynamic recovery predicate and both RC forms, A/B updates and evaluations, W/Q reliability checks, and continued service after decision. Resolve the finite proposal-universe rule under Byzantine equivocation and the binding of a first vote to an already-held R maximum. Review the synchronizer against §2.1 rather than assuming ordinary asynchronous callbacks create clean rounds.
2. Deterministic simulator evidence for (f,p)=(1,1) and (2,1): asynchronous safety with f Byzantine equivocators, hidden fast certificates, delayed and duplicate evidence, and cross-rank replay; post-GST progress with f Byzantine replicas and rotating sets of at most p temporarily suspended correct replicas. Demonstrate recurring replicas, fair historical relay, and actual request-before-response clean rounds. Assert first-vote immutability, true-value persistence, monotone false maxima, legal ancestry, fast/recovery compatibility, elimination progress and service after decision. Report recovery waits separately from clean-rank progress; impose no uniform elapsed-round deadline that the revision does not claim. A finite simulator suite supplies evidence, not exhaustive proof of all schedules.
3. Reproduce §10's n=6 ambiguous recovery: a snapshot `v,v,w,w` at m=4 must wait even while a peer holds FC(v); adding v gives `v,v,v,w,w` at m=5 and must select v. Exhaustively check small static snapshots for hidden-fast preservation and at-most-one candidate at P (Appendix B), including Byzantine equivocation and P−1 ambiguity. These bounded checks do not replace the full R/A/B simulator.

| # | Commit | Content | ~Lines |
| --- | --- | --- | --- |
| 6 | Equal-weight committee model | Port 79ece3fbc and e1f271c49 without close hunks: account q = 2f+p+1, r = f+p+1, FF = N−p, N−f reports; N largest holders. Separately name election Q, F_fast, P and W and validate n=3f+2p+1. Retain `--committee-model equal_weight --committee-f 1 --committee-p 1`. | port |
| 7 | R first votes and recovery (logic) | `checkpoint_election/first_votes.rs` and recovery module: per-rank immutable records bound to legal R ancestry, distinct-identity snapshots, FC_i, dynamic Cand(S), singleton/empty RCs and independent verification. Assert 2Q−n=f+1, 2F_fast−n=3f+1, F_fast+P−n=Q, P−f−p=f+p+1 and 2Q−(n−f)=2f+1. Test both RC forms, ambiguity and imported resolution, unknown values parked, first-vote immutability despite R updates, and bounded recovery checks for n=6 and n=9. | re-estimate |
| 8 | Archipelago R/A/B, relay and synchronizer (logic) | Full R → recovery → A → B proposer, source A/B services, W/Q reliability, recursive B/FC predecessors, pending evidence, slow proofs verifiable by non-members, historical relay and leaderless synchronizer. Deterministic simulator and all exit criteria above, including invalid RC rejection, insufficient witnesses, rotating suspension, late old responses and continued service after decision. | re-estimate |
| 9 | Wire messages | Candidate payload at 0x16; signed R/A/B request/response envelopes at 0x1a, with immutable FIRST embedded in R responses; use 0x19 for relayed first-vote/recovery evidence rather than the old standalone rank-zero voting round; CheckpointProof at 0x1b. Bind instance, rank, type, triggering request and proof references. Encode RC snapshots, B/FC ancestry and evidence fetches by digest; define exact subtypes before coding and enforce the 65,535 B frame. | re-estimate |
| 10 | Candidate, persistence and service wiring | RAI candidate and Valid_I integration, rank-zero proposal policy, proof verifier, report-ticker service, per-rank first-vote locks and R/A/B state in memory, recovery/decision relay and synchronizer scheduling. Replay checks (disk persistence and restart recovery deferred) and end-to-end fork0 test; decisions invoke the existing installation interface. | re-estimate |
| 11 | Benchmark record | **Gate B**: fork0 and offline1; record fast-path frequency, R fast latency, recovery size/wait, clean-rank count, and close duration. Add a rotating-suspension recovery exercise distinct from the permanent-offline workload. | docs |

There is no Kudzu fallback. Gate B compares account-voting goodput and p50 with the completed Gate A and close duration with the historical Kudzu range (0.1–1.3 s). A permanently offline validator is not a recurring correct replica: `offline1` can fast-decide with five matching responses but is not a general test of Theorem 8.5. Deliberately split candidates in simulator coverage so the slow path is tested even when the end-to-end baseline takes the fast path.

## Phase 2: rateless IBLT reconciliation (commits 12 to 16)

Five commits put T and G reconstruction on a rateless coder and end in Gate C. The signed report header and the usability rule (both roots rebuilt and checked, evidence verified) do not change; only how the sets are obtained does.

**Coder.** A symbol is `{count: i64, sum: [u8; 105], check: u64}`; the sum is the XOR of 105-byte items and the check the XOR of a 64-bit Blake2 of each item. Every item maps to index 0; the next index after i is i + ⌈(1.5 + i)·((1 − r)^−1/2 − 1)⌉ with r drawn from splitmix64 seeded by the item digest (the paper's ρ(i) = 1/(1 + 0.5 i)). The encoder keeps a min-heap of (next index, item) so symbol i costs only the items due at i, and caches the prefix per frozen set. The decoder subtracts the peer's symbol i from its own symbol i, peels with a work queue that revisits only the cells of a recovered item, rejects an item recovered twice or peeled from a position it does not own, and is finished when symbol 0 is empty. Count +1 means present only at the reporter (insert); −1 means present only locally (delete). The paper's overhead is 1.35 to 1.72 symbols per difference.

**Sizes.** Per-epoch T on rai_kudzu is 13k to 16k entries; the paper's cumulative T reaches 45k. Differences between running nodes were tens to hundreds of entries (an N to F change counts twice), so a stream is a few tens of KB. A node thousands of entries behind finishes in one pull loop instead of being refused for ever.

| # | Commit | Content | ~Lines |
| --- | --- | --- | --- |
| 12 | Rateless coder (logic) | node/src/consensus/election/rateless.rs: symbol, item trait (105-byte encoding, 64-bit check), index mapping, Encoder, Decoder, insert/delete split. Tests: roundtrip for d in {1, 4, 50, 1000, 5000} with symbols ≤ 2d; linearity enc(A) ⊕ enc(B) = enc(A △ B); identical prefix from independent encoders of one set; garbage input stops; regression for a linear check (a byte-slice check made mixed cells look pure and hung a test for 18 minutes on Sept 15) | 500 |
| 13 | Wire messages | `ReportSymbolsReq{epoch, target, kind: T or G, from: u32, count: u16}` at 0x14; `ReportSymbolsReply{epoch, target, kind, from, symbols}` at 0x15; 500 symbols per reply under the 65,535 B frame; serde round trips; header sizes; dispatch to the report thread | 350 |
| 14 | T over the stream | ReportExchange: per report a Decoder seeded from a clone of the local view taken when the stream starts (the base must not move under the decoder), a cursor and a 300 ms retry; on finish apply deletes then inserts to the base clone and require root() equal to the signed root; an Encoder cached per own frozen snapshot and per reconstructed report so any holder answers; requests go to the reporter first, then to any PR that announced the same root. Keep the zero-message shortcut for an identical local root. Tests: reconstruct with no shared root; N to F counts as two symbols; 3,000 entries short finishes in one loop; a reply for another target is ignored; a stream resumes after a lost reply | 900 |
| 15 | G over the stream | Port 0d933ad5c (retain original signed vote batches in VoteRecords). Items are the reporter's signed epoch-e statements (hash 32, kind 1, epoch 8, signature 64 = 105 bytes). The requester subtracts the statements it holds from i, verifies each recovered signature, derives Ĝ = hashes \\ keys(T̂) and checks the G root, which stays a root over hashes so the signed header is unchanged. Tests: a lost first vote is recovered and counts as support; a relayed third-party vote is not; a Byzantine reporter's stream that never matches stays unusable | 600 |
| 16 | Benchmark record | **Gate C** | docs |

What this deletes from the paper's text: §4.2's pinned common projection and "retry after gossip if no root is shared", and the k_i·h root-negotiation term in Equation 9. §8.5 then describes the mechanism that is actually measured.

## Phase 3: paper conformance from rai-cross-epoch-minimal (commits 17 to 23)

Seven commits port the v43 semantics onto the trimmed tree and end in Gate D, the nine-variant matrix. Each is a manual port: the source commits sit on top of the sketch machinery and the Kudzu close, so their hunks in either are dropped and the rest adapted.

| # | Commit | Source commits | Notes |
| --- | --- | --- | --- |
| 17 | Report semantics per §3.1 | dd1e743e4, c0f4c0057 | R tag with F > N > R, cumulative T frozen once the predecessor is known, exact G = V \\ keys(T). The coder is agnostic to entry contents, so only certified_state, epoch_certified and the roots change |
| 18 | Evidence verification of every entry | 85c076aac (ledger_evidence.rs, EvidenceReq 0x1d), fb09aee67, 755fcae88 | N needs an NC or inherited notarization, F needs a finality proof, R must match the predecessor; the report binds the committee digest; usable only after all checks |
| 19 | BuildState per Figure 3 | 3c3bc45d6, epoch_ledger half of b607acd48 | explicit F\* closure, Rules 1 to 3 with RetainedKind, promotion only on a closing-epoch NC, checkpoint wire status 3 |
| 20 | Attachment and overlap | attachment.rs, container half of b607acd48, 803d4157c, f0f78db53, 727493343 | complete current-epoch parents, both overlap exceptions, provisional recheck on install, cross-epoch first-vote lock, scheduler walks past complete blocks |
| 21 | Installation without rollback | 562fd2213, 16c0a6585 (correctness half), 8b36f9cc0, b680c965f, 7f4f5bf00, checkpoint_installer.rs | J′ = J ∪ F_e; retained branches followed; checkpoint-finalized blocks the ledger lacks are fetched, forced and cemented; a superseded recovery lock never rolls back live finality |
| 22 | Boundary stop and fork convergence | ed69aa3d5; b864602ed; a2873c7bf, cdf479fdc, 2d4184c59, 597fc3f69, 1a11543b4 from rai-review-fixes | no new account vote in a left epoch; solicitations name every held candidate; BlockPulls back-off; fetch from the voter's channel; carried instances solicited; fork-cache placement. a2873c7bf's residual retry is moot after commit 15; keep only its test intent |
| 23 | Full matrix and paper text | — | **Gate D**; update §4.2, Equation 9 and §8.5 for the rateless mechanism; rewrite §5 and the supplement's election appendix from the note (version 1.2 Lemma 5.2 supplies hidden-fast preservation; Theorems 6.6, 7.1, 8.5 and 9.1 supply the stated agreement, ancestry validity, conditional termination and two-delay fast-path claims, with their assumptions retained) |

The minimal branch's lock-contention work (report tick on its own thread, evidence gossip off the network thread, vote signature cache) is not in the plan. Add it only if Gate C or D shows p50 regressing against Gate B, one commit per measured cause.

## Risks and open items

- **The close is cut out at import, by hand.** On rai_kudzu the close runs as elections inside the AEC, so commits 1 to 3 remove its arms from the container, solicitor, request aggregator, vote generators and kudzu.rs instead of checking files out whole. Until Gate B only the unit tests and the single-epoch Gate A cover the result, and no epoch closes end to end. A stall at Gate B can therefore come from the import or from the election; the simulator evidence of commit 8 is what separates the two.
- **Phase 3 ports are manual.** The v43 commits were written on top of the sketch machinery, the Kudzu close and the old report semantics; each port drops those hunks and adapts the rest. Budget a Gate-C-style four-variant run after commits 19 and 21 if either touches more than its listed files.
- **Temporary suspension is not permanent failure.** Version 1.2 Theorem 8.5 covers f Byzantine replicas plus p temporarily suspended correct replicas under recurring participation, persistent relay and clean synchronized ranks. At n=6, f=p=1, byz1 with rotating temporary suspension fits those budgets when the remaining premises hold; a permanently offline correct replica does not. Ambiguous recovery may need P=n−f historical first votes and has no uniform completion deadline.
- **Synchronizer and source semantics are proof obligations.** The clean request-before-response rounds and original A/B reliability and state-update rules must be implemented and demonstrated. Removing W/Q witnesses, adding sealed A/B phases, or replacing recovery with an arbitrary maximum is not the version 1.2 protocol. The note's convergence claim is conditional on those premises.
- **Finite authenticated proposal universe.** §8.3 relies on at most n application values. Authentication alone does not limit a Byzantine identity to one signed proposal. Make the accepted rank-zero proposal rule and its interaction with equivocation explicit in the conformance review; do not claim that finite-value premise has been established merely by checking signatures.
- **Restarts and evidence retention.** Disk persistence is deferred. The interim implementation keeps first-vote locks, R/A/B safety state and terminal decisions only in memory; durable restart recovery remains a separate task. Keep unresolved historical votes, recovery certificates and ancestry available after advancing or deciding. A safe garbage-collection rule remains to be established; age or a timeout alone does not authorize deletion.
- **Admissibility is local.** A correct responder must be able to validate any value in circulation within the prepared window. A broadcast naming reports this node has not reconstructed is parked and the reports are requested, never dropped. Until commit 14 the only way to obtain a report is the identical-root shortcut, so Gate B is limited to fork0 and offline1.
- **Decoder base must not move.** Commit 14 seeds the decoder from a clone of the local view taken when the stream starts; reseeding from the live view mid-stream breaks the peel.
- **Frame limit.** Messages are at most 65,535 B: 500 symbols per reply. Archipelago evidence is well founded down to authenticated rank-zero proposals and grows with rank, so certificates reference signed requests and responses by digest; a certificate stays pending until every referenced item is held, and a slow proof handed to C(e−1) carries its full evidence closure across several messages.
- **Fast-path assumption.** The candidate rule gives identical values only when usable sets coincide; a split within the grace timer means the slow path, which is correct but slower. Gates B to D record how often this happens.

**Pitfalls already paid for** (from earlier RAI work, do not re-diagnose):

- The IBLT check must be a non-linear hash of the item; a byte slice is XOR-linear and peeling loops for ever.
- rai_kudzu's fork stalls had four causes beyond the shared-root refusal: residual-retry memo starvation, fork-candidate block floods overfilling the block processor (76k replies per node), the hinted fetcher pulling from random peers, and carried instances never solicited. Commits 15 and 22 cover them.
- Benchmarks on a busy host are meaningless: check `top` first; keep the task checkout's `target/debug` off the disk when free space nears 8 GiB; never touch the main repo's 62 GB target.
- Building baselines from the wrong tree wastes hours: the paper's numbers come from b864602ed, not the rai_kudzu working tree.
- Any rule that makes a replica's votes depend on its own clock (Δ_timeout first votes in account elections) broke byte-identical epoch states in September; account timeouts stay forbidden.

**Open questions:**

- [ ] Grace timer Δ_c for the candidate rule: 500 ms is a guess; measure at Gate B.
- [ ] Concrete leaderless synchronizer and evidence that it supplies §2.1 clean-round semantics.
- [ ] Rank-zero proposal acceptance under Byzantine equivocation, first-vote ancestry when R already holds a different maximum, and safe historical-evidence garbage collection.
- [ ] Map offline/byz benchmark schedules to version 1.2's recurring-participant assumptions; keep n=6, f=p=1 as the baseline and n=9, f=2,p=1 simulator coverage. Larger committees are no longer required solely to fit one Byzantine replica into the termination budget.
- [ ] Whether the lock-contention commits from rai-cross-epoch-minimal are needed; decide on Gate C and D numbers.
