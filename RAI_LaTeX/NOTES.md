# Notes for the paper

Points to work into the manuscript. Remove an entry once the text carries it.

## Done (Oct 21 2026)

- Protocol changes (advantage over Nano): in the conclusion, as a property of the design.
- Proposals carry the selected reports' signed commitments: a sentence in Section VI.
- Latency claim in Section X: rewritten from the c534df908 matrices (`tools/matrix_figures.py`).
- Supplement: departures nine and ten, defects (23)-(28), reproduction commands for the new matrices.

## Done (Oct 22 2026)

- Rotation with a Byzantine member that keeps its seat: Figure 7 (six scenarios), Section X, supplement defect (29) and behaviour (28) (`run_sweep --set rotation-byz`, doc/benchmarks/rai-rotation-byz-2026-10-22; build A/B in doc/benchmarks/rai-rotbyz-fix-ab-2026-10-22).

## Done (Nov 8 2026)

- Section X, Figures 4-7, abstract, contributions, conclusion and supplement read the merged matrix: gcp-matrix4 (11 publish-gap reruns) over gcp-matrix3 (voter-lock build, equal weights) over gcp-matrix2 (Nano's cells without the change). The four-vote Nano handover text, the laggard limitation and the request-cap build split are gone; the reduced offered rate for pause is stated.
- The committee change has six-region data (Figure 7); only restarts and handoff stay one host.

## Open

- Restart and handoff runs have no six-region data: the restarts paragraph is labelled as one host (doc/benchmarks/rai-ownlane-rotation-byz-2026-11-02, rai-rule2-rotation-2026-10-29).

## Done (Nov 10 2026)

- Drop-ins of Nov 9 merged into the sources: new title and abstract, invariants and Table I (rule, invariant, counterexample) in Section III, threat-model paragraphs in Section IV, corrected Fig. 3, the R9 run as Fig. 4, shortened Sections VIII and IX (the TLC table moved out, Table I carries the counterexamples), the pause model in Section X, the compact related work with Table III, `rai-additions.bib`, and a bibliography in the supplement. The built `main.pdf` has the same text as the fitted PDF of Nov 9 (11 pages of body; page 12 holds AI Tool Usage and the references).

## Done (Nov 10 2026, fixes)

- AI Tool Usage: Claude Code's models from the commit history (Opus 5.5, Fable 5.1 and 5.2), figure numbers by reference, the Claude drafts named.
- Headline ratio: the abstract says "on a wide-area deployment" and the conclusion "six and seven regions", since 6.7 to 38 times includes the seven-region committee change.
- Section IV: the new member ran as a standby from the start (no catch-up time was measured).
- Fig. 3: V4 reports A notarized. Section X: the load starts 2 s into epoch 1, so epoch 3 begins about 14 s in (closes begin at 6, 14, 22 s in `closes-reconf-fork10-rai.dat`).
- References: arXiv numbers for [3], [17], [18]; pages for FastPay; issue for [21]; year 2017 for the Nano white paper (corrected Nov 11).

## Done (Nov 10 2026, supplement and models)

- Supplement: the main paper's committee change is its Figure 8; the fast-path switch is attributed to the one-host committee-change runs.
- `tla/`: the configurations named `paper` are now `allup` (Rule 2 upward over all first votes) and `norule` (Handoff without the early-vote rule), in file names, lists, README and the supplement; `PaperHole*` is `AllUpHole*`. The archived TLC outputs no longer contain local paths or the user name.

## Done (Nov 10 2026, equal quorum)

- RAI at Nano's quorum across the six regions (`run_gate_b.py --no-fast-path --committee-p 0 --committee-seats 6`, six cells, three runs each, all settled). Medians per cell, no forks / 5% / 10% / the same with a Byzantine validator: p50 549, 538, 541, 543, 540, 540 ms; p95 774, 938, 1126, 739, 1018, 1077 ms; 1888, 1792, 1704, 1880, 1830, 1744 blocks/s; slowest close 4.8, 4.4, 4.4, 8.3, 15.1, 15.2 s. Section X's threats paragraph states it; paid for by dropping the protocol-change sentence of the conclusion and one parenthesis in Section X.

## Done (Nov 10 2026, R10 switch)

- `RaiOverlap` and `RaiHandoff` have a switch for the first half of R10 (`CONSTANT CROSS <- CrossOff`: a validator may first-vote early a block that conflicts with its closing-epoch vote). No violation: `Overlap_noCross` 16.3 M states, `Handoff_same_noCross` 21.9 M, `Handoff_disjoint_noCross` 3.7 M. In these one-position models the rule is defensive: the overlap rule's witness and the recheck after installation already exclude the switched vote's effect. Table I and contribution (3) say so; the rule stays in the protocol.
- `RaiPrefix` without the guard (parent and child positions): `Prefix4_{same,shared2,shared1}_noCross` 1.2, 6.4, 6.5 M states, no violation; N = 6 simulation, 2.3 M and 2.5 M traces, no violation.
- `RaiResolve` with R10 off and the other termination rules on: `TwoGood` violated after 20 steps (`_noContested`, `_noR10`); the archived `_earlykept` run (early vote for the kept block) also violates it. So R10 is needed for termination on its own; Table I, Section VII and the supplement say so.
- R10 is now stated as one rule, no early first vote at a contested position, with not switching sides as its consequence (rule list, Section VII, introduction); Table I keeps R10 in the termination row only, and Section VII says a switch alone breaks no safety property in the models. The supplement keeps the cross-epoch rule by name and says it follows from the termination rule.

## Open before submission


## Review of Nov 11 2026 (external review, checked against the code)

Done in the text:

- Supplement title matches the main paper's. The supplement says its nine modules are the main text's eight models. Section VIII says Theorem 3 is split in two in the supplement. Figure 5's caption names the Warsaw validator.
- Workload: every measured block is a representative change (`tools/nanospam/src/high_prio_check.rs`: constant balance, `Link::ZERO`), so the runs have no send/receive pairs. Section X (setup, threats) and the supplement's limitations now say so.
- Report scope: `T_i` is defined over the whole ledger, but checks and `Build` read only what differs from `S_{e-1}` (`CertifiedState` base + delta, `changed_entries`); stated in Appendix C and the implementation notes. Scaling in `N` is listed as not measured.

Done (review concern 1, manifest omission; option A chosen Nov 11):

- The reviewer's literal trace does not run in the prototype: after installing `S_{e-1}` a correct validator casts no first or final vote for a block that bypasses a surviving record unless the bypassing block is final at it (`at_closed_lock`), and then its report tags that block `F`, which is justified only with the discharge witnesses.
- The variant that did run, by code reading: final votes cast before `S_{e-1}` is installed, for a block `B` whose rival `S_{e-1}` holds under a recovery record of origin `e-1`, with `XW_{e-1}(B)` completed by late notarizations. The reports tagged `B` `N`, an `N` entry needed no witness, and `build_state` dropped a claim bypassing an undischarged record, so a manifest without the epoch-`(e-1)` votes for `B` was accepted and `S_e` kept the rival without `B`.
- Fix: a reporter lists its own final vote next to the `N` tag when the block bypasses a record of `S_{e-1}` (`ResidualVotes::derive_with`, `EpochLedger::bypasses_record`, `complete_report_on`); `well_formed` allows a `Final` record next to an `N` tag; `unjustified` asks the witnesses for every listed final vote, for usability and for a proposal's manifest alike; `evidence_committed` checks every claim when the manifest shows less than this node holds and a selected report lists a final vote. Tests: `a_listed_final_vote_bypassing_a_record_needs_the_matching_origin_witness`, `a_final_vote_may_be_listed_next_to_an_n_tag`. The supplement's departures describe the rule and say the main text's runs predate it.
- Model: `RaiHandoff` switch `BIND <- BindSettled` (final votes do not bind the witness) violates `ClosureThm1b` after 25 steps, the trace of the fix; the default is the existing model. Scripted: Case 8 in Appendix F and `case_8_a_listed_final_vote_keeps_a_block_a_manifest_could_drop` (`epoch_ledger.rs`).
- Other validators derive a reporter's `G_i` with its listed final votes (`ReportExchange::derive_residual_with`, predecessor from the report service).

Open, other review points:

- Close cost breakdown (Nov 11): `tools/close_breakdown.py`, `sections/eval-data/close-breakdown.dat`, supplement "What a close costs". One host, N = 6 vs N = 10 (500 blocks/s, 5% forks, one run each, `doc/benchmarks/rai-close-scale-onehost-2026-11-11`): check 19 -> 34 ms, reconcile 227 -> 302 ms, agree unchanged, close 824 -> 991 ms. No WAN run beyond seven validators. A first N = 10 attempt failed in setup (one PR held 1,059 of 1,061 setup blocks; nanospam gave up), the retry settled.
- Committee change in every epoch and a cold newcomer across regions: only the one-host limitation is reported.
- Stored signatures (Nov 11, 9a96863fa): own first and final votes are written, synced, before release; a restart serves them and signs only unreleased recorded votes (`EPOCH_VOTES_RESTORED`). No per-epoch keys. The restart runs in the paper predate it.

Done (Nov 11, later):

- Section IV: reports are selected by weight, as the prototype does (identity order until the report weight is reached, `epoch_decision.rs`); the supplement's implementation notes give the rule. The supplement's new subsection "Weighted committees" (Appendix D) restates every counting step of the proofs for weights `W >= 3F+2P+1`; outside it: A1-A4 for weights, `F` as a bound on Byzantine weight, and models and runs with equal weights only.
- Introduction: the checkpoint agreement is RAI's only agreement, once per epoch and off the payment path. Section IV: `p = 0` gives `N = 3f+1` without the one-round path.
- Appendix C, Rule 3: the send of a promoted receive is already in `F*` (the `p+1` selected correct first voters tag it `F` with its proof), so the dependency closure adds no finality. The limitations say the prototype's construction does not compute the closure and that receives are neither modelled nor measured. The prototype votes a receive only once its send is final in its own ledger (`unattached_dependency`, `Unattached::Link`, in `node/src/consensus/attachment.rs`, with unit tests); the limitations say so.

Open, needs the fleet (Nov 11): the controller (34.158.229.20) does not answer and there is no gcloud here, so the fleet is off or has a new address. Waiting on it: the six-region matrix on the fixed build, restarts across regions, a committee change in every epoch and a cold newcomer across regions, and a run with more than six validators (e.g. two node processes per VM).
