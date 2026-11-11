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
- No six-region run at f=1, p=0: the comparison with Nano is not at equal quorum (RAI's normal path needs four votes, Nano's five); Section X says so.

## Done (Nov 10 2026)

- Drop-ins of Nov 9 merged into the sources: new title and abstract, invariants and Table I (rule, invariant, counterexample) in Section III, threat-model paragraphs in Section IV, corrected Fig. 3, the R9 run as Fig. 4, shortened Sections VIII and IX (the TLC table moved out, Table I carries the counterexamples), the pause model in Section X, the compact related work with Table III, `rai-additions.bib`, and a bibliography in the supplement. The built `main.pdf` has the same text as the fitted PDF of Nov 9 (11 pages of body; page 12 holds AI Tool Usage and the references).

## Done (Nov 10 2026, fixes)

- AI Tool Usage: Claude Code's models from the commit history (Opus 5.5, Fable 5.1 and 5.2), figure numbers by reference, the Claude drafts named.
- Headline ratio: the abstract says "on a wide-area deployment" and the conclusion "six and seven regions", since 6.7 to 38 times includes the seven-region committee change.
- Section IV: the new member ran as a standby from the start (no catch-up time was measured).
- Fig. 3: V4 reports A notarized. Section X: the load starts 2 s into epoch 1, so epoch 3 begins about 14 s in (closes begin at 6, 14, 22 s in `closes-reconf-fork10-rai.dat`).
- References: arXiv numbers for [3], [17], [18]; pages for FastPay; issue for [21]; year 2018 for the Nano white paper.

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

- The one `\fillin` left: the ChatGPT model and version in AI Tool Usage (`main.tex`).
- Check the year 2018 of the Nano white paper [5] against the source.

## Review of Nov 11 2026 (external review, checked against the code)

Done in the text:

- Supplement title matches the main paper's. The supplement says its nine modules are the main text's eight models. Section VIII says Theorem 3 is split in two in the supplement. Figure 5's caption names the Warsaw validator.
- Workload: every measured block is a representative change (`tools/nanospam/src/high_prio_check.rs`: constant balance, `Link::ZERO`), so the runs have no send/receive pairs. Section X (setup, threats) and the supplement's limitations now say so.
- Report scope: `T_i` is defined over the whole ledger, but checks and `Build` read only what differs from `S_{e-1}` (`CertifiedState` base + delta, `changed_entries`); stated in Appendix C and the implementation notes. Scaling in `N` is listed as not measured.

Done (review concern 1, manifest omission; option A chosen Nov 11):

- The reviewer's literal trace does not run in the prototype: after installing `S_{e-1}` a correct validator casts no first or final vote for a block that bypasses a surviving record unless the bypassing block is final at it (`at_closed_lock`), and then its report tags that block `F`, which is justified only with the discharge witnesses.
- The variant that did run, by code reading: final votes cast before `S_{e-1}` is installed, for a block `B` whose rival `S_{e-1}` holds under a recovery record of origin `e-1`, with `XW_{e-1}(B)` completed by late notarizations. The reports tagged `B` `N`, an `N` entry needed no witness, and `build_state` dropped a claim bypassing an undischarged record, so a manifest without the epoch-`(e-1)` votes for `B` was accepted and `S_e` kept the rival without `B`.
- Fix: a reporter lists its own final vote next to the `N` tag when the block bypasses a record of `S_{e-1}` (`ResidualVotes::derive_with`, `EpochLedger::bypasses_record`, `complete_report_on`); `well_formed` allows a `Final` record next to an `N` tag; `unjustified` asks the witnesses for every listed final vote, for usability and for a proposal's manifest alike; `evidence_committed` checks every claim when the manifest shows less than this node holds and a selected report lists a final vote. Tests: `a_listed_final_vote_bypassing_a_record_needs_the_matching_origin_witness`, `a_final_vote_may_be_listed_next_to_an_n_tag`. The supplement's departures describe the rule and say the main text's runs predate it.
- Not done: a model of the rule (the TLA+ models still assume the manifest discharges every lock a selected correct vote relied on), and a scripted multi-node case for the trace.
- Other validators derive a reporter's `G_i` without the listed final vote and fall back to the residual stream for such a report; passing the predecessor to `ReportExchange::derive_residual` would save that.

Open, other review points:

- Close cost breakdown on the WAN runs (reconciliation time, manifest bytes, `Build` and signature time, round time) and scaling beyond `N = 6`: not measured. `EPOCH_VALIDATE_TIMING` has evidence and build times per close in the node logs.
- Committee change in every epoch and a cold newcomer across regions: only the one-host limitation is reported.
- Stored signatures instead of re-signing after a restart (needed for per-epoch key erasure): not implemented.
- Stake-weighted quorums: the selection of exactly `N-f` reports in identity order has no direct weighted counterpart (subsets reaching the weight threshold differ in size); Section IV only says the safety intersections carry over.
- Framing: the introduction does not say that agreement runs once per epoch off the payment path, and Section IV does not say that `p = 0` gives `N = 3f+1`.
- Receive dependencies under Rule 3 (does `M` carry send finality proofs for promoted receives): the prototype's `build_state` has no finalized-send closure to check this against.
