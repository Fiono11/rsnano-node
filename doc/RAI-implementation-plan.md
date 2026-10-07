# RAI on develop: implementation plan

Updated 7 October 2026 · Rui Morais

> On 7 October 2026 the user chose to go back to the Kudzu close election for the checkpoint election, instead of Fast Archipelago. This revision replaces Phase 1 accordingly. The Fast Archipelago work (commits 7 to 10 of the previous revision, `f26d99e5b` to `8cba757bf`) stays on branch `rai_leaderless` and is not part of this plan. The Kudzu close is built on branch `rai_kudzu_close`, on top of Phase 0 and commit 6.
>
> Earlier history: based on revision 35 of the Claude doc <https://claude.ai/artifact/AX9QE1benJ85L1hv46Xbpf>, exported 2 October 2026, and the completed Gate A.

## Decisions

This plan brings the RAI protocol of *RAI: Preserving Latent Finality Across Committee Handoffs* onto `develop` (ff2b32710) in 21 commits over four phases. Three decisions fix its shape:

1. **Base on `rai_kudzu` (8aaf98c2c), trimmed.** `develop` HEAD is rai_kudzu's merge base, so the import is a subset of its 52 commits, never a merge. About half of its 22.5k source lines are kept, about 3k lines are dead or off-paper and dropped, and about 2.5k lines of T/G reconciliation are replaced. Phase 0 left the Kudzu close out to keep Gate A to one epoch; Phase 1 brings it back.
2. **Report reconciliation uses a rateless IBLT** (Yang, Gilad, Alizadeh, SIGCOMM 2024) instead of the paper's pinned common projection. The reporter streams coded symbols of its frozen set; the requester subtracts its own view and peels. No shared root, no size estimate, no retry-on-failed-decode. This removes the exact failure that stalled rai_kudzu's fork runs: a node one entry short had no shared root and every difference request was refused.
3. **The checkpoint election is the Kudzu close of rai_kudzu**, the joint epoch election the paper measured (`epoch_close.rs` is identical on rai_kudzu and on v43, `b864602ed`). Each epoch's close is a multi-round Kudzu instance. The leader of round r is the member at (e + r) mod n of the union of the old and new committees, in public key order. The leader derives `BuildState(S_{e-1}, Q_e)` from N−f usable reports and proposes `X = (h_p, Q_e, d_e)`; a follower re-derives the state and first-votes a valid proposal, or abstains after Δ_E. Second looks, timeout certificates, the cross-committee conflict clause and the exit final vote follow rai_kudzu. A child of a complete placement copies its parent's `(Q_e, d_e)`. Account voting keeps the paper's single-support rule.

Two facts from the code audit drive the phase order. rai_kudzu's reports are not the paper's reports: they have only N and F tags, a per-epoch T instead of the cumulative frozen ledger, and a G keyed by vote kind. The paper-conformant definitions, BuildState rules and the fork-convergence fixes are commits on `rai-cross-epoch-minimal` (b864602ed, the v43 the paper measured), and are ported in Phase 3; the equal-weight thresholds come from the same branch but move up to commit 6. Second, the close consumes usable reports and a validated value and installs the state it decides through one function, `install_decided_checkpoint`, so Phases 2 and 3 change what feeds it and how its result is installed, not the close.

Branches: `develop` (target), `rai_kudzu` (source of the import and of the close), `rai-cross-epoch-minimal` (source of the conformance ports), `rai-review-fixes` (five fork fixes not on either), `rai_kudzu_close` (this plan's work), `rai_leaderless` (the abandoned Fast Archipelago work).

## Current status — 7 October 2026

**Scope updates still in force:** total retained-storage limits and disk persistence are deferred at the user's request, and Nanospam validation assumes every node stays online. Per-message limits remain.

Phase 0 (commits 1–5) is on `rai_leaderless` and `rai_kudzu_close` through `c6f3852ea`. **Gate A passed:** the strict quiet-host run cemented 45,065 blocks on all six validators with identical final states in epoch zero, at 1,973 blocks/s and 88–89 ms p50 versus the historical baseline's 1,941 blocks/s and 88–90 ms. See the [benchmark record](benchmarks/rai-phase0-2026-10-02/README.md).

Phase 1 on `rai_kudzu_close`: commit 6 (equal-weight committees, `313068c9c`), commit 7 (Kudzu close vote kinds and slot rules, `918905fef`) and commit 8 (the close election, `2464504bf`) are implemented. Commit 8 is validated by unit tests, container tests of the close and an end-to-end single-node integration test. Without reconciliation Gate B failed: frozen reports with different roots left fewer than N−f usable. At the user's request Phase 2 then came before Gate B.

Phase 2 on `rai_kudzu_close`: the rateless coder (`d9313079f`), reconciliation of both report inventories (`172460c73`, the plan's commits 11 to 13 in one), stream logging (`3e1fa4422`) and proportional batches (`7493aa08e`). **Gate B passed** on `172460c73`: 1,931 blocks/s, p50 87–89 ms, three epochs decided in round 0 on all six nodes ([record](benchmarks/rai-gate-b-2026-10-07/README.md)). **Gate C passed on the final binary** with one caveat: fork0, byz1 and three fork5 runs settled with zero dropped streams, but an earlier fork5 run diverged in cemented state, and symbols per item were 1.7 to 2.3 instead of about 1.35 ([record](benchmarks/rai-gate-c-2026-10-07/README.md)). Phase 3 is in progress: `5ae9c6e58` keeps retained instances collecting certificates and names every held candidate in solicitations; `fac6df5da` adds a checkpoint follower that fetches (BlocksReq, 0x1d), forces in and cements checkpoint-finalized blocks a ledger lacks. **Gate D passed functionally on `fac6df5da`:** all nine paper variants settled, one run each ([record](benchmarks/rai-gate-d-2026-10-07/README.md)). Items 15 to 18 (report semantics, evidence verification, BuildState per Figure 3, attachment and overlap) and the rest of 19 and 20 are not ported yet.

## Sources: keep, drop, port

**Kept from rai_kudzu** (paper mechanisms 1, 2, 3, 5, 6, 7; line counts are added lines against develop):

| Area | Files | Lines |
| --- | --- | --- |
| Feature flag, epoch on the wire | types/consensus_epoch.rs, types/vote.rs, messages/{confirm_req, confirm_ack, publish, network_filter, message_deserializer}.rs | ~900 |
| Single-support voting, epochs, committees | election/{kudzu, committee, election, election_id, election_state, block_tallies}.rs, active_elections/{slot_states, epoch_states, epoch_committees, root_container, vote_router, apply_vote_helper}.rs | ~5,000 |
| Boundary, freeze, install | active_elections_container.rs (A/B parts), aec_service.rs, aec_fact_processor.rs, ledger.rs roll_back_batch_unchecked | ~3,500 |
| Vote plumbing | vote_generator(s).rs, voting_scheduler.rs, aec_voter.rs, local_vote_history.rs, vote_cache/\*, vote_applier.rs, request_aggregator\*.rs, confirm_req_sender.rs, confirmation_solicitor\*.rs, aec_fork_inserter.rs, winner_block_broadcaster.rs | ~1,600 |
| Reports, epoch value, candidate derivation | messages/report.rs (Report only), reports/{mod, report_service, report_plugin}.rs, epoch_decision.rs (can_derive, derive_fresh and validate only), certified_state.rs core, epoch_ledger.rs, epoch_value.rs | ~3,900 |
| The Kudzu close (Phase 1) | epoch_close.rs, messages/epoch_prop.rs (EpochProp 0x16), propose, handle_proposal and repeat_proposals in epoch_decision.rs, VoteKind Notar/Timeout/Abstain, ConsensusEpoch close rounds and required_epoch, the three extra vote generators, the close arms in the container, solicitor, vote cache and request aggregator, the close paths of kudzu.rs, EpochCommittees::for_close | ~2,500 |
| Required RPC | epoch_start (the only caller of start_epochs) | ~70 |
| Harness (optional) | tools/nanospam/\*, final_state and confirmation_info RPCs, diagnostics.rs, aec_stats.rs | ~1,700 |

**Dropped from rai_kudzu:**

- The close-proof and checkpoint-transfer path: message types 0x19 to 0x1c, messages/{close_proof, checkpoint}.rs, reports/{checkpoint, close_proof}.rs, EpochLedger::{checkpoint_entries, from_checkpoint_entries}, the retain_\*/close_proof wrappers. Nothing sends CloseProofReq and the transferred ledger is never read. About 1,000 lines.
- The reconciliation path: ReconReq 0x14, ReconReply 0x15, ResidualSketchReq/Reply 0x17/0x18, election/sketch.rs, CertifiedState::{difference, apply, CertifiedDelta}, shared_sources, the history bridge with MAX_HISTORY, ReconRefusal. About 2,500 lines including 900 of tests. Replaced in Phase 2.
- Off-paper knobs: count-based epoch advance, the epoch_advance RPC, observe_epoch, epoch_terminated_elections and final_state.rs close_value. The close keeps rai_kudzu's joint committee (C(e−2) and C(e−1), with the cross-committee conflict rule), as v43 measured it.
- Account-domain leftovers: ResidualKind::Notar, ElectionState::TimedOut, stats.timed_out. A close round left on a timeout certificate keeps its previous state; nothing reads it.

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
| B | 9 | fork0, all nodes online, 8 s epochs | settles with at least three decided epochs; closes in round 0; goodput and p50 within noise of Gate A; close duration against the Kudzu record (0.1 to 1.3 s) |
| C | 14 | fork0, fork5, byz1, all nodes online | settles; zero reconciliation refusals; symbols per report near 1.35 × difference |
| D | 21 | the nine paper variants | all settle; non-fork p50 within noise of v43 (101 to 109 ms) |

Before commit 1, move the stale untracked copies out of the working tree (node/src/consensus/reports/, consensus/attachment.rs, bootstrap/block_pulls.rs, bootstrap/missing_block_fetcher.rs, the epoch_locks RPC files), since the imports check out the same paths.

```text
Phase 0: import, no Kudzu close   (commits 1 to 5)    -> Gate A: single epoch
Phase 1: the Kudzu close           (commits 6 to 9)    -> Gate B: epochs close
Phase 2: rateless IBLT             (commits 10 to 14)  -> Gate C: four variants
Phase 3: v43 conformance ports     (commits 15 to 21)  -> Gate D: nine variants
```

Phases 0 to 3 run in sequence and each ends in a gate. Phases 0 to 2 and Gates A to C are complete; Gate B ran after Phase 2, because without reconciliation a close only becomes ready when the frozen reports happen to share roots.

## Phase 0: import rai_kudzu, trimmed, without the Kudzu close (commits 1 to 5)

Five commits bring the trimmed rai_kudzu tree onto develop in dependency order; each compiles with the feature on and off, and commit 5 ends in Gate A. The Kudzu close is left out: commits 1 to 3 cut it from the files they bring over, and Phase 1 brings it back.

| # | Commit | Content | ~Lines | Verification |
| --- | --- | --- | --- | --- |
| 1 | Feature flag and epoch on the wire | `rai_protocol` in types, messages, node, rpc/server, daemon, cli; ConsensusEpoch without as_close_round and required_epoch; VoteKind with First and Final only, and epoch in vote.rs; epoch in confirm_req; evidence flag in confirm_ack and publish; network_filter cutoff; message_deserializer; stats enums | 900 | types and messages tests both ways; wire format identical with the feature off |
| 2 | Single-support account voting and timed epochs | kudzu.rs as single_support.rs, committee.rs, election.rs, election_id, election_state, block_tallies, slot_states, epoch_states, epoch_committees, root_container, vote_router, apply_vote_helper, the container and aec_service, all vote plumbing, config toml, ledger roll_back_batch_unchecked, aec_fact_processor. Left out: the close rounds in the container, solicitor, request aggregator and vote generators, EpochCommittees::for_close, count-based advance, observe_epoch, epoch_terminated_elections, close-proof wrappers, ResidualKind::Notar in account slots, TimedOut | 10,000 | the kudzu, election and container unit tests that do not exercise the close; gated tests in node/tests election.rs and vote_processor.rs |
| 3 | Reports, epoch value, installation | messages/report.rs with Report only, reports/{mod, report_service, report_plugin}, can_derive, derive_fresh and validate from epoch_decision.rs, certified_state.rs core, epoch_ledger.rs, epoch_value.rs, node.rs wiring, network dispatch. A decided value enters through `install_decided_checkpoint`; Phase 0 had no election to call it, commit 8 supplies the Kudzu close. Reconciliation reduced to the identical-root shortcut. Not imported: epoch_close.rs, epoch_prop.rs, propose, handle_proposal, repeat_proposals, ReconReq/Reply, ResidualSketch\*, sketch.rs, difference/apply, checkpoint.rs, close_proof.rs, messages 0x19 to 0x1c, the unread `transferred` field | 4,500 | report and build_state tests; installation tests fed a decided value directly |
| 4 | Feature off means develop behaviour | gate record_votes (VoteRecords grows without bound today), the vote_processor validate skip, the priority_scheduler early return, check_vacancy and vacancy, ledger_event_processor ordering | 100 | `cargo test --lib -q` equals develop; no `cfg!` left on a hot path |
| 5 | Harness | epoch_start RPC, final_state (without close_value) and confirmation_info RPCs, diagnostics.rs, nanospam (byzantine without the close vote kinds, offline, silent, delegation, epoch knobs), run_variant and run_matrix tools | 2,500 | **Gate A** |

With no close on the tree, an epoch that ends cannot be decided, so Gate A runs fork0 in a single epoch set longer than the run. It checks account voting, vote plumbing and the feature gating before any election code exists. Reports, BuildState and installation had only unit tests until the close returned in commit 8.

## Phase 1: the Kudzu close election (commits 6 to 9)

Phase 1 puts back the close Phase 0 cut, from rai_kudzu `8aaf98c2c`, onto the trimmed tree. The source is the measured implementation: `epoch_close.rs` and `epoch_prop.rs` are byte-identical on rai_kudzu and on v43. Only the parts the plan already dropped stay out (close proofs, checkpoint transfer, count-based epochs, following votes into later epochs).

| # | Commit | Content | Status |
| --- | --- | --- | --- |
| 6 | Equal-weight committees | Exact membership N = 3f + 2p + 1, integer thresholds, configuration and nanospam controls (from 79ece3fbc, e1f271c49). The Archipelago-only `CheckpointThresholds` were removed again in commit 8 | Done, `313068c9c` |
| 7 | Kudzu vote kinds and slot rules | VoteKind Notar, Timeout and Abstain in the duration bits, read only with the feature on; timeout and conflict certificates, second looks, the line-32 timeout rule and the epoch-slot settled predicate in kudzu.rs; account elections still take first and final votes only; three extra vote generators | Done, `918905fef` |
| 8 | The close election | epoch_close.rs (rounds, leaders, placement chain, copy-parent payload, proposal validity); close rounds in the vote's epoch field; EpochProp 0x16; propose, handle_proposal and repeat_proposals in the epoch decision service; close arms in the container, solicitor, vote cache and aggregator; EpochCommittees::for_close; `close_round_timeout_ms` (2 s); the final_state RPC's close info. A certificate installs the decided state through `install_decided_checkpoint`; the epochs close in sequence | Done, `2464504bf` |
| 9 | Gate B | All-online fork0 with 8 s epochs and equal weights (`tools/rai/run_gate_b.py`): settlement, close rounds, close duration, goodput and p50 against Gate A | Passed after Phase 2 ([record](benchmarks/rai-gate-b-2026-10-07/README.md)) |

**How a close runs.** Leaving epoch e at its boundary creates the close election of e. It takes part once this node holds `S_{e-1}` and N−f usable reports of e, and the close of e−1 has a certificate. The round leader proposes at once; a follower that derived the same state first-votes it, and every replica abstains after Δ_E without a valid proposal. A certificate decides the value, and the state it names is installed: blocks it finalized are cemented, the committee of e+2 is derived, and epoch e+1's finality, held by the predecessor gate, is released. An ended epoch is left only once the close of the epoch before it has a certificate.

**Known costs, from rai_kudzu's record.** A round led by a representative that does not propose costs Δ_E (2 s). Under byz1 in September one epoch-1 close took four rounds and 11 s. Account finality of e+1 waits for the close of e; the boundary cost 0.5 to 2.6 s per epoch on rai_kudzu. The close needs N−f usable reports, so a report nobody can reconstruct stalls it: that is what Phase 2 fixes.

## Phase 2: rateless IBLT reconciliation (commits 10 to 14)

Five commits put T and G reconstruction on a rateless coder and end in Gate C. The signed report header and the usability rule (both roots rebuilt and checked, evidence verified) do not change; only how the sets are obtained does.

**Coder.** A symbol is `{count: i64, sum: [u8; 105], check: u64}`; the sum is the XOR of 105-byte items and the check the XOR of a 64-bit Blake2 of each item. Every item maps to index 0; the next index after i is i + ⌈(1.5 + i)·((1 − r)^−1/2 − 1)⌉ with r drawn from splitmix64 seeded by the item digest (the paper's ρ(i) = 1/(1 + 0.5 i)). The encoder keeps a min-heap of (next index, item) so symbol i costs only the items due at i, and caches the prefix per frozen set. The decoder subtracts the peer's symbol i from its own symbol i, peels with a work queue that revisits only the cells of a recovered item, rejects an item recovered twice or peeled from a position it does not own, and is finished when symbol 0 is empty. Count +1 means present only at the reporter (insert); −1 means present only locally (delete). The paper's overhead is 1.35 to 1.72 symbols per difference.

**Sizes.** Per-epoch T on rai_kudzu is 13k to 16k entries; the paper's cumulative T reaches 45k. Differences between running nodes were tens to hundreds of entries (an N to F change counts twice), so a stream is a few tens of KB. A node thousands of entries behind finishes in one pull loop instead of being refused for ever.

| # | Commit | Content | ~Lines |
| --- | --- | --- | --- |
| 10 | Rateless coder (logic) | node/src/consensus/election/rateless.rs: symbol, item trait (105-byte encoding, 64-bit check), index mapping, Encoder, Decoder, insert/delete split. Tests: roundtrip for d in {1, 4, 50, 1000, 5000} with symbols ≤ 2d; linearity enc(A) ⊕ enc(B) = enc(A △ B); identical prefix from independent encoders of one set; garbage input stops; regression for a linear check (a byte-slice check made mixed cells look pure and hung a test for 18 minutes on Sept 15) | 500 |
| 11 | Wire messages | `ReportSymbolsReq{epoch, target, kind: T or G, from: u32, count: u16}` at 0x14; `ReportSymbolsReply{epoch, target, kind, from, symbols}` at 0x15; 500 symbols per reply under the 65,535 B frame; serde round trips; header sizes; dispatch to the report thread | 350 |
| 12 | T over the stream | ReportExchange: per report a Decoder seeded from a clone of the local view taken when the stream starts (the base must not move under the decoder), a cursor and a 300 ms retry; on finish apply deletes then inserts to the base clone and require root() equal to the signed root; an Encoder cached per own frozen snapshot and per reconstructed report so any holder answers; requests go to the reporter first, then to any PR that announced the same root. Keep the zero-message shortcut for an identical local root. Tests: reconstruct with no shared root; N to F counts as two symbols; 3,000 entries short finishes in one loop; a reply for another target is ignored; a stream resumes after a lost reply | 900 |
| 13 | G over the stream | Port 0d933ad5c (retain original signed vote batches in VoteRecords). Items are the reporter's signed epoch-e statements (hash 32, kind 1, epoch 8, signature 64 = 105 bytes). The requester subtracts the statements it holds from i, verifies each recovered signature, derives Ĝ = hashes \\ keys(T̂) and checks the G root, which stays a root over hashes so the signed header is unchanged. Tests: a lost first vote is recovered and counts as support; a relayed third-party vote is not; a Byzantine reporter's stream that never matches stays unusable | 600 |
| 14 | Benchmark record | **Gate C** | docs |

**As built (7 October 2026).** Commit 10 is `d9313079f`; commits 11 to 13 landed as one, `172460c73`, with three differences from the table:

- The residual inventory streams the reporter's residual records (block, kind, parent: 105 bytes), whose root the report signs, as rai_kudzu's sketch fallback did. Verifying each record against a signed vote stays with the evidence ports of commit 16.
- Requests go to every principal representative; any node holding an inventory with the requested root answers, and the decoder ignores replies for another offset. Encoders are cached per root, at most 16 per epoch.
- A stream asks for 16 symbols first, then half of what it received so far, at most 500 (`7493aa08e`).

Gate C measured 1.7 to 2.3 symbols per recovered item; most differences were ten to forty items, where the first batch dominates.

What this deletes from the paper's text: §4.2's pinned common projection and "retry after gossip if no root is shared", and the k_i·h root-negotiation term in Equation 9. §8.5 then describes the mechanism that is actually measured.

## Phase 3: paper conformance from rai-cross-epoch-minimal (commits 15 to 21)

Seven commits port the v43 semantics onto the trimmed tree and end in Gate D, the nine-variant matrix. Each is a manual port: the source commits sit on top of the sketch machinery and the Kudzu close, so their hunks in either are dropped and the rest adapted.

| # | Commit | Source commits | Notes |
| --- | --- | --- | --- |
| 15 | Report semantics per §3.1 | dd1e743e4, c0f4c0057 | R tag with F > N > R, cumulative T frozen once the predecessor is known, exact G = V \\ keys(T). The coder is agnostic to entry contents, so only certified_state, epoch_certified and the roots change |
| 16 | Evidence verification of every entry | 85c076aac (ledger_evidence.rs, EvidenceReq 0x1d), fb09aee67, 755fcae88 | N needs an NC or inherited notarization, F needs a finality proof, R must match the predecessor; the report binds the committee digest; usable only after all checks |
| 17 | BuildState per Figure 3 | 3c3bc45d6, epoch_ledger half of b607acd48 | explicit F\* closure, Rules 1 to 3 with RetainedKind, promotion only on a closing-epoch NC, checkpoint wire status 3 |
| 18 | Attachment and overlap | attachment.rs, container half of b607acd48, 803d4157c, f0f78db53, 727493343 | complete current-epoch parents, both overlap exceptions, provisional recheck on install, cross-epoch first-vote lock, scheduler walks past complete blocks |
| 19 | Installation without rollback | 562fd2213, 16c0a6585 (correctness half), 8b36f9cc0, b680c965f, 7f4f5bf00, checkpoint_installer.rs | J′ = J ∪ F_e; retained branches followed; checkpoint-finalized blocks the ledger lacks are fetched, forced and cemented; a superseded recovery lock never rolls back live finality |
| 20 | Boundary stop and fork convergence | ed69aa3d5; b864602ed; a2873c7bf, cdf479fdc, 2d4184c59, 597fc3f69, 1a11543b4 from rai-review-fixes | no new account vote in a left epoch; solicitations name every held candidate; BlockPulls back-off; fetch from the voter's channel; carried instances solicited; fork-cache placement. a2873c7bf's residual retry is moot after commit 13; keep only its test intent |
| 21 | Full matrix and paper text | — | **Gate D**; update §4.2, Equation 9 and §8.5 for the rateless mechanism; §5 keeps the Kudzu close the paper describes and measured |

The minimal branch's lock-contention work (report tick on its own thread, evidence gossip off the network thread, vote signature cache) is not in the plan. Add it only if Gate C or D shows p50 regressing against Gate B, one commit per measured cause.

## Risks and open items

- **The close was cut out at import and put back by hand.** On rai_kudzu the close runs as elections inside the AEC, so commits 7 and 8 restore its arms in the container, solicitor, request aggregator, vote generators and kudzu.rs from the diff against Phase 0, not by checking files out whole. Unit, container and single-node integration tests cover the result; Gate B is the first multi-node check.
- **Phase 3 ports are manual.** The v43 commits were written on top of the sketch machinery, the Kudzu close and the old report semantics; each port drops the sketch hunks, keeps the close hunks and adapts the rest. Budget a Gate-C-style four-variant run after commits 17 and 19 if either touches more than its listed files.
- **Close liveness needs every usable report it counts.** With N = 6 and f = 1, N−f = 5 usable reports are required. Before Phase 2 this failed even without forks, because in-flight blocks at a timed boundary differ per node; rateless reconciliation now rebuilds every honest report. A Byzantine reporter's report may stay unusable, and five honest ones suffice.
- **Local finality the checkpoint does not carry.** One fork5 run in five left a block finalized on one node and not in the decided checkpoint, and cemented states diverged. Phase 3's commits 15, 17 and 19 (cumulative T with the R tag, BuildState per Figure 3, installation that fetches and cements checkpoint-finalized blocks) address it.
- **Leader rounds.** A faulty or slow leader costs Δ_E per round, and the predecessor gate holds the next epoch's finality meanwhile. Close duration must stay well within one epoch; record it at every gate.
- **Restarts and evidence retention.** Disk persistence is deferred: slot states, close rounds and decided states live in memory only. The close history is bounded to 256 epochs.
- **Admissibility is local.** A proposal naming reports this node has not reconstructed is not voted for; the leader repeats it every 200 ms while its epoch is among the last four, and the round times out if it stays unusable.
- **Decoder base must not move.** Commit 12 seeds the decoder from a clone of the local view taken when the stream starts; reseeding from the live view mid-stream breaks the peel.
- **Frame limit.** Messages are at most 65,535 B: 500 symbols per reply, and at most 64 selected reports per proposal.

**Pitfalls already paid for** (from earlier RAI work, do not re-diagnose):

- The IBLT check must be a non-linear hash of the item; a byte slice is XOR-linear and peeling loops for ever.
- rai_kudzu's fork stalls had four causes beyond the shared-root refusal: residual-retry memo starvation, fork-candidate block floods overfilling the block processor (76k replies per node), the hinted fetcher pulling from random peers, and carried instances never solicited. Commits 13 and 20 cover them.
- Benchmarks on a busy host are meaningless: check `top` first; keep the task checkout's `target/debug` off the disk when free space nears 8 GiB; never touch the main repo's 62 GB target.
- Building baselines from the wrong tree wastes hours: the paper's numbers come from b864602ed, not the rai_kudzu working tree.
- Any rule that makes a replica's votes depend on its own clock (Δ_timeout first votes in account elections) broke byte-identical epoch states in September; account timeouts stay forbidden. Δ_E exists only in the close.
- A leader that proposes before its own instances settled spends its one-shot first vote on a value its followers moved off (`PROPOSAL_DELAY` A/B of 22 September); the close derives from N−f reports instead, which removes that wait.

**Open questions:**

- [ ] Δ_E: 2 s is rai_kudzu's default after the byz1 analysis; measure round counts at Gates B to D.
- [ ] Whether the lock-contention commits from rai-cross-epoch-minimal are needed; decide on Gate C and D numbers.
