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

- (Done Nov 23) Restart and handoff runs across six regions: see below.

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
- Workload (corrected Nov 25 2026): this note used to say every measured block is a representative change, citing `high_prio_check.rs`. That file publishes one priority block every 10 s, and every run passes `--no-prio`. The measured blocks come from `block_factory.rs`: no run passes `--change` (0 of 336 summaries in gcp-matrix2/3/4 and the n12 matrix), so the strategy is `SendReceive`. That means sends of random amounts to random accounts, plus receives once the send is confirmed (`account_map.rs` `confirmed_receivable`). Change mode ignores `is_fork`, and the runs did fork. Section X and the extended version's limitations now describe it.
- Report scope: `T_i` is defined over the whole ledger, but checks and `Build` read only what differs from `S_{e-1}` (`CertifiedState` base + delta, `changed_entries`); stated in Appendix C and the implementation notes. Scaling in `N` is listed as not measured.

Done (review concern 1, manifest omission; option A chosen Nov 11):

- The reviewer's literal trace does not run in the prototype: after installing `S_{e-1}` a correct validator casts no first or final vote for a block that bypasses a surviving record unless the bypassing block is final at it (`at_closed_lock`), and then its report tags that block `F`, which is justified only with the discharge witnesses.
- The variant that did run, by code reading: final votes cast before `S_{e-1}` is installed, for a block `B` whose rival `S_{e-1}` holds under a recovery record of origin `e-1`, with `XW_{e-1}(B)` completed by late notarizations. The reports tagged `B` `N`, an `N` entry needed no witness, and `build_state` dropped a claim bypassing an undischarged record, so a manifest without the epoch-`(e-1)` votes for `B` was accepted and `S_e` kept the rival without `B`.
- Fix: a reporter lists its own final vote next to the `N` tag when the block bypasses a record of `S_{e-1}` (`ResidualVotes::derive_with`, `EpochLedger::bypasses_record`, `complete_report_on`); `well_formed` allows a `Final` record next to an `N` tag; `unjustified` asks the witnesses for every listed final vote, for usability and for a proposal's manifest alike; `evidence_committed` checks every claim when the manifest shows less than this node holds and a selected report lists a final vote. Tests: `a_listed_final_vote_bypassing_a_record_needs_the_matching_origin_witness`, `a_final_vote_may_be_listed_next_to_an_n_tag`. The supplement's departures describe the rule and say the main text's runs predate it.
- Model: `RaiHandoff` switch `BIND <- BindSettled` (final votes do not bind the witness) violates `ClosureThm1b` after 25 steps, the trace of the fix; the default is the existing model. Scripted: Case 8 in Appendix F and `case_8_a_listed_final_vote_keeps_a_block_a_manifest_could_drop` (`epoch_ledger.rs`).
- Other validators derive a reporter's `G_i` with its listed final votes (`ReportExchange::derive_residual_with`, predecessor from the report service).

Open, other review points:

- Close cost breakdown (Nov 11): `tools/close_breakdown.py`, `sections/eval-data/close-breakdown.dat`, supplement "What a close costs". One host, N = 6 / 10 (f=1, p=3) / 11 (f=p=2, `run_gate_b.py --committee-f 2`) at 500 blocks/s, 5% forks, one run each (`doc/benchmarks/rai-close-scale-onehost-2026-11-11`): check 19 / 34 / 47 ms, reconcile 227 / 302 / 302 ms, agree 519 / 521 / 627 ms, close 824 / 991 / 1149 ms. No WAN run beyond seven validators. A first N = 10 attempt failed in setup (one PR held 1,059 of 1,061 setup blocks; nanospam gave up), the retry settled.
- Committee change in every epoch and a cold newcomer across regions: only the one-host limitation is reported.
- Stored signatures (Nov 11, 9a96863fa): own first and final votes are written, synced, before release; a restart serves them and signs only unreleased recorded votes (`EPOCH_VOTES_RESTORED`). No per-epoch keys. The restart runs in the paper predate it.

Done (Nov 11, later):

- Section IV: reports are selected by weight, as the prototype does (identity order until the report weight is reached, `epoch_decision.rs`); the supplement's implementation notes give the rule. The supplement's new subsection "Weighted committees" (Appendix D) restates every counting step of the proofs for weights `W >= 3F+2P+1`; outside it: A1-A4 for weights, `F` as a bound on Byzantine weight, and models and runs with equal weights only.
- Introduction: the checkpoint agreement is RAI's only agreement, once per epoch and off the payment path. Section IV: `p = 0` gives `N = 3f+1` without the one-round path.
- Appendix C, Rule 3: the send of a promoted receive is already in `F*` (the `p+1` selected correct first voters tag it `F` with its proof), so the dependency closure adds no finality. The limitations say the prototype's construction does not compute the closure and that receives are neither modelled nor measured. The prototype votes a receive only once its send is final in its own ledger (`unattached_dependency`, `Unattached::Link`, in `node/src/consensus/attachment.rs`, with unit tests); the limitations say so.

Open, needs the fleet (Nov 11): the controller (34.158.229.20) does not answer and there is no gcloud here, so the fleet is off or has a new address. Waiting on it: the six-region matrix on the fixed build, restarts across regions, a committee change in every epoch and a cold newcomer across regions, and a run with more than seven validators across regions (e.g. two node processes per VM). Done on one host instead (Nov 11): restart with stored signatures (settled, audit clean), N = 6 vs 10, and five consecutive committee changes at 500 blocks/s (settled; 52 of 24,000 blocks confirmed > 60 s after the load; control without changes 132 s vs 186 s), records in `doc/benchmarks/rai-close-scale-onehost-2026-11-11`. Pitfall: `python3 -I -c 'import run_sweep'` from tools/rai fails (no cwd on the path), which left `--rotation` empty in a first attempt.

## Twelve seats across six regions (Nov 11 2026, evening)

- 14 VMs (six regions twice + two Toronto standbys), f = p = 2 on 12 seats. At 2,000 blocks/s RAI first saturated (475 blocks/s, p95 31.7 s) while Nano kept up (1,937, p50 523, p95 579). Lock profile: the elections lock was the bottleneck, not CPU.
- Fixes f1cf174de (solicitor scan per 64 elections, request lookups per request) and 1b8e3e391 (reconciliation streams start at 64 and double): RAI 1,912 blocks/s, p50 455, p95 979, p99 1,111 ms, one run. The remaining p95 gap to Nano is the 3-4 s after each epoch boundary. Records and table in doc/benchmarks/gcp-n12-2026-11-11.
- Not done: repeats, the fork and Byzantine cells and the committee change at 12 seats, and the 108-run matrix (task stopped by the user's instruction to turn the fleet off).
- Nov 12: common epoch origin (51e4683fc): one 12-seat run at 2,000/s settled at 1,946 blocks/s, p50 425, p95 764, p99 949 ms (Nano 1,937 / 523 / 579 / 879); epoch-end spread 0.6-0.7 s -> 0.1 s. Still one run per build; no fork, Byzantine or committee-change cells at 12 seats.

## Twelve validators in the paper (Nov 18 2026)

- Matrix `doc/benchmarks/gcp-matrix-n12-45k-2026-11-16` (ended at 76 of 108 runs on credit; the Byzantine committee-change cells were not run). `tools/n12_numbers.py <matrix> sections/eval-data/n12` writes `numbers.txt` (medians and ranges) and `table.tex`.
- Section X: new paragraph "Twelve validators" (RAI's p95 above Nano's without forks; with 10% forks RAI confirms less than pause; closes 25-157 s). Paid for by trimming the prototype's synced-signing sentence, the settled-first-votes sentence (in Appendix "Counting settled first votes"), the restarts paragraph, the epoch-3 timing sentence (Figure 8 marks it), the equal-quorum rate (Table in the supplement), and the opening sentence of the boundary paragraph. Still 11 pages of body.
- Abstract and conclusion: the 6.7-38x claim is scoped to six validators; the conclusion's limitations say RAI confirms less than a pause at twelve validators with 10% forks.
- Supplement: Appendix "Twelve validators across six regions" with Table VI, the failed run, every repeated run (including the outlier repeated at the authors' request), the builds, the fork-grace limit change, and the diagnosis; the limitations and close-cost text no longer say no run had more than seven validators; reproduction names the set and that the node logs were not kept. A `\clearpage` before the bibliography puts the floats that used to land after the references before them.

Nov 19: the twelve-validator matrix is complete (108 runs, three per cell, on the rebuilt fleet for the last 32). Table VI regenerated from it (`tools/n12_numbers.py`), its caption no longer says "one in the last two rows"; Section X says all 108 runs passed and gives the committee-change cells with forks (slowest closes 93-157 s, RAI 227-1,031 blocks/s against pause's 503-1,007); the supplement says the last 32 runs used the rebuilt fleet. On the authors' instruction the supplement no longer describes failed or repeated runs (only the harness's no-progress limit, 180 s, 30 s in the first runs). Same pagination (11 pages of body, Table VI on supplement page 25).

Open: the vote-recording lock change that should lift the saturation.

## Submission without a supplement (Nov 23 2026)

- HotCRP takes no supplementary file. The main text now cites the anonymized extended version as `\cite{extended}` (`rai-additions.bib`) in the eleven places that said "supplement" or "artifact"; the pointers are no longer than before, and the body still ends on page 11. `supplement.tex` is retitled "Extended Version of ...".
- `\balance` removed: it split reference [36] across the columns of the last page and overran the margin.
- Extended version: table identifiers may break at `_` and `,` (Tables II and III overlapped their result column); the reproduction paragraph is `\sloppy` with breaks in the harness flags.
- `tools/make_anon_bundle.sh <out>` (from the repo root, after building both PDFs) assembles paper/, tla/, source/ and the seven cited records, redacts user names and local paths, and fails on any term from a private `IDENTITY_TERMS` list (binaries included). 315 MB, ~100 MB zipped.

Open before submission: host the bundle anonymously, put its URL in the `extended` entry (replacing the `\fillin`), rebuild both PDFs and the bundle, upload again so the hosted copy carries the final paper.

## Restarts across six regions, catch-up fix, lock A/B (Nov 23 2026)

- `run_sweep --set restart-wan` (four cells x 3, six seats + Toronto standby, 2,000 blocks/s): all 12 runs settled on the build with `catchup.patch`, every fork resolved, no conflicting signature from a correct validator. Records `doc/benchmarks/gcp-restart-wan-fix-2026-11-23` (on branch `rai_n12_lock`, with the code). Section X's restarts paragraph, contribution (4) and the conclusion no longer say one host; the extended version has the paragraph "Across six regions", the defect, Table `tab:restart-wan`, and the reproduction line.
- Defect fixed: a close's catch-up requests went only to peers known as representatives; a restarted validator learns that mapping from crawler replies, which members stop giving once epochs run, so it could ask no one (`known_reps=0` in every run). Close rounds are now asked of every connected peer (`EPOCH_CLOSE_SOLICIT` / `EPOCH_CLOSE_SERVE` diagnostics).
- Harness: the vote audit leaves the Byzantine node's own log out (`audit_votes.audit_correct`), counting its conflicts as `byzantine_conflicts`.
- Twelve seats: moving vote recording out of the exclusive elections lock made no difference (each build saturated once in four runs, `doc/benchmarks/gcp-n12-lock-ab-2026-11-23`); dropped. The twelve-validator numbers stay those of the matrix. Storms remain about one run in four; cause not established.

## Pre-submission review, text fixes (Nov 25 2026)

A DSN-style review (weak accept) was checked against the paper, the records and the code; the text fixes below went in with matching cuts, and the body still ends on page 11 (AI Tool Usage now starts on page 11).

- Section X: the workload is sends and dependent receives (see the corrected note above), fork siblings differ in their representative; threats: the recipe does not add the sends of Step 1 (installation cements a receive after its send), and the stale "restarts were not run across regions" sentence is gone; epochs end later when the previous checkpoint is not installed; the report-witness rule postdates the runs; N=12 fork p95 cell medians (RAI 0.9-80 s, pause 12-51 s, RAI worse in 4 of 8).
- Section VI: a report ends epoch e only after installing S_{e-1} (code: `try_advance_epoch` / `previous_epoch_closed`); the report-witness rule for blocks that bypass a lock of S_{e-1} (Case 8); an empty keep-pass position is voted afresh; agreement members support a candidate only after reconstructing and recomputing.
- Section IV: definition of a good epoch (from the extended version's Theorem 4). Section V: parent rule (final, notarization lock, or notarized in this epoch; code `attachment.rs`). Section VII: late supports need members of both committees, so a full replacement sends post-boundary blocks to the gate; R10 detects siblings only. Section VIII: top-down order, Lemma 5, Lemma 6 in Theorem 1's sketch. Section IX: Prefix, Recovery and Resolve are exhaustive only at N=4. Conclusion: 8 s epochs.
- Cuts: Section V "Why the checkpoint must decide" (one clause moved to VI), owner extension and fork-workload sentence in VI, second-set paragraph, origin-release sentence, header-authentication sentence, multi-origin repeat in VII, Fig. 4 narration, model list in IX, garbage collection, pause mean, services pointer at the end of III, durable-votes pointer, quorum aside in "What we compare", R10 termination sentence.
- Abstract unchanged: a 144-word version with "8-second epochs" was proposed for the abstract registration; if it is registered, copy it into main.tex.
- Extended version: limitations paragraph describes the real workload and the missing send closure.

## Second pre-submission review: termination bound, pause wording, R5 (Nov 25 2026, evening)

A second pasted review found that Theorem 3's "two consecutive good epochs" is false. A good epoch allows early first votes at a position not yet contested; Rule 2 counts settled votes only (paper, extended version, prototype `epoch_ledger.rs` keep pass skips `EarlyFirst`), so the checkpoint after the fork epoch can keep nothing and the next epoch's settled votes can split again. The extended version's proof assumed "in a good epoch every correct first vote is settled"; its own Appendix "Counting settled first votes" measures the effect (396-529 empty positions).

- Bound now: three good epochs from the fork; two once a checkpoint holds a block at the position. Changed: abstract (150 words by wc; adds 8-second epochs and "a paused variant"), Sections II, IV, VI (Step 3, R5 paragraph), VIII (Theorem 3 and sketch), IX, X (pause model), Table III, conclusion; extended version Theorem (b) and its proof (an epoch e+1 step and the two-epoch case), formal-account.
- R5 matters for safety: the report rule asks for witnesses only behind final and final-voted blocks because, after installation, a correct validator first-votes a bypassing block only once it is final. Stated in Section VI and the extended version; Table I tags the row I1, I3.
- Pause: Table III "drained" -> "as RAI"; "stop-and-drain" removed from the abstract, contributions, Section X and conclusion. Figure 8's caption says epoch 3 starts 14 s into the load (the reviewer misread the two lines).
- RaiResolve: Rule 2 counts settled votes only (switch SETTLED); the keep pass keeps the predecessor's block if still eligible, also after its record is discharged; S_0 may start uninstalled (PREINST); new SpecGood; invariants ThreeGood and TwoGoodKept; E <- Four for four epochs. Results in the extended version's TLC table. The old exhaustive TwoGood pass came from counting every first vote; R10 off alone no longer gives a counterexample, so Table I drops "R10 alone (20)".
- Space: tightened the Theorem 3 sketch and the R5 sentence, shorter pause phrase in the conclusion. The body ends on page 11; AI Tool Usage now starts on page 12, references on pages 12-13.
- Section IV "Joining and leaving": a new member votes only after installing the checkpoint its seat comes from (review W4(a), truncated; the prototype leaves an epoch only once the previous close is decided, `previous_epoch_closed`).
- Open TLC runs at commit time (uncapped reruns in the background): Resolve4_rules_234, Resolve4_rules_early_noContested, Resolve4_rules_early_noR10; the table gives the stopped runs' reach.
- Register the abstract as it stands in main.tex.

## Keep-pass fallback to early votes: two epochs again (Nov 25 2026, night)

On the user's instruction ("make the changes but do not run anything"), the keep pass now falls back to early first votes where no eligible block has a settled vote and the previous checkpoint holds nothing at the position. This restores the two-epoch bound: in a good fork epoch every correct member first-votes, and at least N-2f correct reports list those votes, so the checkpoint always keeps a side.

- Prototype: `epoch_ledger.rs` `build_state_of` counts `EarlyFirst` weight apart (`Evidence::early`); keep pass uses it only under the two conditions; helper `most_supported`; new test `without_settled_votes_the_keep_pass_counts_early_votes`. Not compiled, tests not run.
- Model: `RaiResolve` switch FALLBACK (on by default); every existing Resolve config pins `FALLBACK <- RuleOff` so its recorded result stays reproducible; new `Resolve4_fb_{early,23,234,sim}` check TwoGood, ThreeGood, TwoGoodKept with the fallback. Not run.
- Paper: bound back to two epochs (abstract, II, IV, VI Step 3, VIII, X, Table III, conclusion); Section IX says all model runs predate the fallback; Section X says the fallback was added after the runs. Extended version: recipe text, Theorem (b) and proof (two epochs, the fallback case), flip-lemma note, Appendix "Counting settled first votes", RaiResolve description, TLC table row "not yet run", results paragraph.
- To do before relying on it: `cargo test --lib -q` (or the new test), the four fb TLC configs, and RaiRecoverMany safety configs with the fallback (not added there yet); a fleet rerun if the evaluation should measure it.
- Space: shorter Theorem 3 sketch, R5 sentence, conclusion sentence, Step 3 wording, eval and IX disclosures. The body ends on page 11.

## Send closure: implied, not missing (Nov 25 2026, night)

The morning's note that the prototype's recipe "does not compute the finalized-send closure" was too pessimistic. The recipe adds no sends itself, but a correct validator first-votes a receive only once its send is final (`attachment.rs`, `Unattached::Link` checks the confirmed set), and final-votes only what it first-voted. A receive the recipe finalizes (certificate-backed F entry, or promotion over an NC_e) has q votes, at least q-f correct; a selection omits at most f members, so at least q-2f = p+1 >= 1 selected correct reports show the send final (or S_{e-1} holds it), and Step 1 finalizes it. A receive kept only under a lock may lack a final send, as Build allows. An explicit closure was not implemented: the reports do not carry a receive's source, and looking it up in the local ledger would make the derived state depend on what each validator holds. Section X's threats sentence and the extended version's limitations paragraph now give the argument.
