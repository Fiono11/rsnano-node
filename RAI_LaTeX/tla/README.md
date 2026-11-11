# TLA+ models for the RAI handoff (N = 6, f = p = 1, one account position; RaiPrefix and RaiRecovery: two positions, N = 4 and 6)

The closure invariants were renamed from `Thm64*` to `ClosureThm1*` (Theorem 1 of the paper) on 2026-10-10; the archived `out_*.txt` files of runs made before that date still print the old names.

Run with `./run.sh <module> <config>`, for example `./run.sh RaiClose Close_TRUE`.
`run.sh` expects `tla2tools.jar` (TLA+ tools 1.8.0) one directory up and Java 11 or newer.
The `out_*.txt` files are the outputs of the runs reported below, including the violating traces.

| Config | Module | Switches | Result | Time (2 cores) |
|---|---|---|---|---|
| `Close_FALSE` | RaiClose | Fix B off | `ClosureThm1` violated after 19 steps | 35 s |
| `Close_FALSE_conflict` | RaiClose | Fix B off | `NoConflict` violated after 23 steps (two fast certificates) | 2 min |
| `Close_TRUE` | RaiClose | Fix B on | no error, 651,632 distinct states | 3 min |
| `Overlap_fixed` | RaiOverlap | all on | no error, 5,217,510 distinct states | 14 min |
| `Overlap_noCross` | RaiOverlap | all on, no cross-epoch guard (`CROSS <- CrossOff`) | no error, 16,314,028 distinct states, depth 34 | 15 min |
| `Overlap_noDischarge` | RaiOverlap | discharge off | `SameVerdict` violated after 15 steps (Case 2) | 1 min |
| `Overlap_noLateEv` | RaiOverlap | late votes inadmissible | `WitnessAdmissible` violated after 14 steps | 1 min |
| `Overlap_noSplit` | RaiOverlap | NC split off | `UsableReports` violated after 9 steps | 10 s |
| `Overlap_noSplit_safety` | RaiOverlap | NC split off | other four invariants hold, 5,485,444 distinct states | 14 min |
| `Overlap_probe1`, `_probe2` | RaiOverlap | all on | probes violated, as intended: overlap finality and "final block versus lock on its rival" are reachable | < 1 min |

`RaiHandoff.tla` combines early voting, two closures and a changing committee. `RULE = TRUE` is "no fast path on early votes". `BIND` (default `"final"`) says what forces a discharge witness into the manifest of S2: a selected correct reporter's settled first vote or its final vote released with the witness; `BIND <- BindSettled` keeps only the first.

| Config | Committees | Certificates | Result | Time (2 cores) |
|---|---|---|---|---|
| `Handoff_same_norule` | same five | off | `ClosureThm1b` violated after 22 steps (Case 3) | 11 min |
| `Handoff_disjoint_norule` | disjoint | off | `ClosureThm1b` violated after 23 steps | 15 min |
| `Handoff_same_rule` | same five | early-vote rule | no error, 8,347,138 distinct states | 15 min |
| `Handoff_same_bindSettled` | same five | early-vote rule, final votes do not bind the witness (`BIND <- BindSettled`, the prototype before the final-vote report rule) | `ClosureThm1b` violated after 25 steps (Case 8) | 7.4 min (4 workers) |
| `Handoff_disjoint_rule` | disjoint | early-vote rule | no error, 3,707,876 distinct states | 17 min |
| `Handoff_same_noCross` | same five | early-vote rule, no cross-epoch guard | no error, 21,898,422 distinct states, depth 38 | 17 min |
| `Handoff_disjoint_noCross` | disjoint | early-vote rule, no cross-epoch guard | no error, 3,707,876 distinct states (no member is in both committees, so the guard never applies) | 7 min |
| `Handoff_overlap3_rule` | three shared | early-vote rule | no error, 121,053,272 distinct states, depth 38 | 2 h 18 min (8 cores) |
| `Handoff_Probe*` | same five | early-vote rule | probes violated, as intended | < 5 min |

`Handoff_overlap1_rule` and `Handoff_overlap4_rule` (one and four shared members) were still running when this archive was made.

The header comment of each module lists its abstractions. No model covers more than one position, a third epoch, restarts, or any N other than 6.

`RaiPrefix.tla` checks the prefix-wide overlap guard on two positions (parent P with rival Pr at position 1, children C1, C2 of P at position 2). Switches: `GUARD = "prefix" | "target"`, `FRESHCHILD = TRUE | FALSE`. Configs `Prefix4_*` use N = 4 (f = 1, p = 0), `Prefix6_*` use N = 6; `same`, `disjoint`, `sharedK` describe the two committees.

| Config | Result | Time (8 cores) |
|---|---|---|
| `Prefix4_same_{fixed,target,nofresh}` | no error, 278,534 distinct states each | 10 s |
| `Prefix4_disjoint_fixed`, `_target` | no error, 1,622,276 distinct states | 1.7 min |
| `Prefix4_disjoint_nofresh` | `NoConflict` violated after 12 steps | 5 s |
| `Prefix4_shared1_fixed`, `_target` | no error, 3,960,746 distinct states | 3.5 min |
| `Prefix4_shared1_nofresh` | `NoConflict` violated after 12 steps | 8 s |
| `Prefix4_shared2_fixed` | no error, 2,434,078 distinct states | 1.7 min |
| `Prefix4_shared2_target` | `NoConflict` violated after 13 steps | 19 s |
| `Prefix4_shared2_nofresh` | `NoConflict` violated after 14 steps | 12 s |
| `Prefix4_*_probes`, `Prefix4_shared2_Probe*` | probes violated, as intended | < 2 s |
| `Prefix6_disjoint_nofresh_sim`, `Prefix6_shared3_target_sim`, `Prefix6_shared3_nofresh_sim` | `NoConflict` violated (random simulation) | < 1 s |
| `Prefix6_same_probes`, `Prefix6_shared3_probes` | probes violated, as intended | 14 s, 3.7 min |
| `Prefix6_same_fixed`, `Prefix6_shared3_target` (BFS) | unfinished at archive time (13.4 M and 35.8 M distinct states, no violation so far) | - |
| `Prefix4_same_noCross`, `_shared2_noCross`, `_shared1_noCross` | no cross-epoch guard (`CROSS <- CrossOff`) | no error, 1,172,954 / 6,388,260 / 6,464,176 distinct states, depth 28 | 42 s, 5.1 min, 7.4 min |
| `Prefix6_same_noCross_sim`, `Prefix6_shared3_noCross_sim` | no cross-epoch guard, random simulation, depth 100 | no violation in 2,268,734 and 2,471,301 traces | 30 min each |

`RaiRecovery.tla` checks owner recovery on top of a recovery-locked block: conflicting X and Y at position 1, children CX (on X) and CY (on Y) at position 2, one closure, installation, early and settled votes; a final child finalizes its parent. Switch: `EXEMPT = TRUE` is the earlier rule (a tip retained under a recovery record alone may always be extended), `EXEMPT = FALSE` the re-notarization rule (such a tip is a parent in epoch 2 only with an epoch-2 NC). `recovery.sh` runs every configuration below.

| Config | Result | Time (6 workers) |
|---|---|---|
| `Recovery4_same_exempt` | `NoConflict` violated after 16 steps (short Case 5: no early votes) | 6 s |
| `Recovery4_{disjoint,shared1,shared2}_exempt` | `NoConflict` violated after 16 steps | < 1 min |
| `Recovery4_same_fixed` | no error, 734,980 distinct states | 17 s |
| `Recovery4_disjoint_fixed` | no error, 755,978 distinct states | 42 s |
| `Recovery4_shared1_fixed` | no error, 3,337,378 distinct states | 2.5 min |
| `Recovery4_shared2_fixed` | no error, 3,922,566 distinct states | 2.2 min |
| `Recovery4_same_Probe{FinalVsLock,Recovery,RecoveryContested}` | probes violated, as intended | < 5 s |
| `Recovery6_same_exempt_sim` | `NoConflict` violated (random simulation) | < 1 s |
| `Recovery6_same_ProbeRecovery` | probe violated (random simulation) | < 1 s |
| `Recovery6_same_fixed` (BFS) | stopped after 75 min (time cap): 87,462,063 distinct states to depth 25, no violation | - |

`RaiResolve.tla` checks termination at a forked position: one position with X and Y, three epochs, two closures, N = 4, a Byzantine validator that double-votes or withholds, epochs flagged good (synchronous) or not. `RULES` switches the two termination rules (checkpoints decide; no early votes at a contested position) and the new Rule 2 (keep the side with most reporter votes). `TwoGood`: two consecutive good epochs make some block final. `Spec` explores every pattern of good epochs (simulation); `Spec23` starts from an arbitrary fork epoch with epochs 2 and 3 good (exhaustive, deadlock checking on). `resolve.sh` runs them.

| Config | Result | Time (6-7 workers) |
|---|---|---|
| `Resolve4_norules_sim`, `Resolve4_norules_23_sim` | `TwoGood` violated (the stall) | < 2 s |
| `Resolve4_rules_sim` | no violation in 493,465 random runs (88 M states) | 15 min (stopped) |
| `Resolve4_rules_23` | no error, no deadlock, 4,635,660 distinct states, depth 33 | 8.8 min |
| `Resolve4_rules_23_earlykept` | `TwoGood` violated (an early vote for the kept block is allowed) | 4 min |
| `Resolve4_rules_23_noContested` | `TwoGood` violated after 20 steps: rule (ii) off (`CONTESTED <- RuleOff`), rules (i) and (iii) on | 8 min |
| `Resolve4_rules_23_noR10` | `TwoGood` violated after 20 steps: rule (ii) and the cross-epoch rule off (`CROSS <- RuleOff`) | 31 min |
| `Resolve4_rules_23_ProbeOneGood` | violated after 10 steps: one good epoch is not enough | 1 min |
| `Resolve4_rules_23_ProbeGood23` | violated after 23 steps: the antecedent is reachable | 1 min |

`MAXKEEP = TRUE` (Rule 2 keeps the block with most reporter votes, ties by hash) is a switch in RaiClose, RaiOverlap, RaiHandoff and RaiRecovery; existing configs set it FALSE. `maxkeep.sh` runs:

| Config | Result | Time (auto workers) |
|---|---|---|
| `Close_TRUE_maxkeep` | no error, 278,404 distinct states | 68 s |
| `Overlap_fixed_maxkeep` | no error, 4,959,763 distinct states | 6.3 min |
| `Handoff_same_rule_maxkeep` | no error, 5,862,017 distinct states | 9.5 min |
| `Handoff_disjoint_rule_maxkeep` | no error, 2,686,030 distinct states | 10.6 min |
| `Recovery4_{same,disjoint,shared1,shared2}_fixed_maxkeep` | no error, 637,038 / 620,222 / 2,622,687 / 3,245,504 | < 3 min |

## RaiRecover and RaiRecoverMany: fresh children, replayed rivals, recovery (2026-10-28)

`RaiRecover.tla` asks two questions on one account: whether voting a fresh child of a kept block while the rival at the parent's position still has live votes breaks safety, and whether a forked account recovers under a proposed patch. Position v holds the conflicting blocks A and A' (`A2`), position v+1 holds C on A' and D on A, and the parent of v is final at genesis. N = 6, f = p = 1 (validators 1-5 correct, 6 Byzantine), four voting epochs, three closures, one committee. Each validator crosses its own boundary and installs checkpoints on its own; a first vote is settled if its signer had installed the predecessor, early otherwise. Built in: the cross-epoch rule, late notarization, the early-vote rule, the prefix-wide overlap guard, Fix B, Rules 1-3 with origin-stamped records, matching-origin discharge and promotion. BuildState applies Rule 1 at every position before Rule 2, as Algorithm 1 does; applying both at v before v+1 drops a fast-final child's certificate (an early draft of the model did this). The owner signed A and A' in epoch 1 and publishes C (D) once a decided checkpoint keeps A' (A).

`RaiRecoverMany.tla` replaces A and A' by B = `BLOCKS` (2 or 3) rivals A1..AB, each first-voted in epoch 1 by one validator (validator i votes Ai, the others nothing), and a child Ci of each. The owner signs Ci only once a checkpoint keeps Ai or Ai is final. Validator 6 may deliver any block to anyone at any time; the model lets a correct validator vote any admissible block whenever it may vote, which gives validator 6 the same power.

Switches (both modules): `IMPLIED` (`off`; `all`: a vote for a child also supports its parent, early votes included; `committed`: early first votes exempt), `SETTLED` (Rule 2 and the pick count settled first votes only), `PICKRULE` (`none`: Rule 2 only, a block alone reaching r; `hash`: else most reporter votes, then lowest hash; `prev`: else the previous checkpoint's block if still eligible, then as `hash`; a pick stamps a record of the closing epoch that is never promoted), `ATTACH` (`strict`: after installation nothing conflicting with a carried record is voted; `permissive`: allowed with a matching-origin witness), `RENOTARIZE` (the paper's re-notarization rule), `BYZ` (`all`: every Byzantine signature available, for safety; `release`: Byzantine first votes released one at a time, final votes never, for recovery), `READY` (epochs in which every correct validator installs the predecessor before voting and leaves only when no vote is still allowed: the paper's good epoch), and in RaiRecoverMany `NOEARLY` (the paper's "no early votes at a contested position", the half that reads the installed checkpoint). The module headers list the abstractions and whether each adds or removes behaviours. `RaiRecoverScenarios.tla` and `RaiRecoverManyScenarios.tla` add action constraints that confine a run to the roles of an expected trace; a constraint only removes behaviours, so a violation found under one is a run of the full model. TLC also checks invariants in the state a constraint-violating step reaches, so scenario invariants and probes name their premise explicitly.

Invariants: `NoConflict` (no two conflicting blocks final, counting finality through a descendant), `ClosureThm1` (each checkpoint protects every certificate of its epoch or earlier), `OneKept`, `FlipBound` (the block a checkpoint keeps at v changes at most once), `KeptOrFinal` (with a pick, every checkpoint keeps or finalizes exactly one block at v), `Recovered`/`RecoveredMany` (with `READY`, some block at v is final when no correct validator or closure can move), `RecoveredEarly` (with `READY = {2, 3}`, final once every correct validator has left epoch 3, i.e. by S3), `Progress`, and probes expected to be violated.

**Nothing below is exhaustive.** Even E = 2 exceeds the BFS budget. "sim" runs are random simulation (TLC `-simulate`, depth 150, 2 workers, stopped at the time cap); "bfs" runs are breadth-first and stopped at a time cap, so they are complete only to the depth reached. A violation found by either is a genuine run; "no error" means none was found.

`recover.sh all` reruns the RaiRecover batch (`recover_sim.txt`, `recover_bfs.txt`); `recover.sh all <simlist> <bfslist>` runs any other list: `many_sim.txt` (the patch), `many_routeA_sim.txt` and `many_routeA2_sim.txt` (route A and the upward Rule 2 over all first votes), `many_order_sim.txt` and `many_order2_sim.txt` (Rule 2 order, forking owner), `many_hole_sim.txt` (the all-votes upward counterexample), `many_routeA3_sim.txt` (top-down route A: early finality, epochs 2 and 4 good), `many_routeA4_sim.txt` (top-down route A with one part of the new Rule 2 removed), `many_routeA_bfs.txt` (bounded breadth-first searches of top-down route A and, as a calibration, of the upward order, E = 3, four workers, run alone). Times are wall clock with four runs of two workers each sharing eight cores; the machine slept during one batch, whose runs are listed at their 10-min CPU cap.

### RaiRecover results

Switch sets: `off_patch` (IMPLIED off, settled, prev, strict), `off_base` (off, all votes, none, permissive), `off_renot` (`off_patch` with re-notarization), `committed` (committed, settled, prev, strict), `all_unsettled` (all, all votes, hash, strict), `all_prev` (all, settled, prev, strict), `all_prev_perm` (`all_prev`, permissive), `all_hash` (all, settled, hash, strict). Checks: `safe` (`NoConflict`, `ClosureThm1`, `OneKept`), `flip` (`FlipBound`), `fork` (`Recovered`, epochs 1 and 4 ready), `two` (`Recovered`, epochs 3 and 4 ready), `two23` (`RecoveredEarly`, epochs 2 and 3 ready). `Scen4_*` are scripted.

| Config | Mode | Result | Time |
|---|---|---|---|
| `Recover4_off_patch_safe`, `_off_base_safe` | sim | `NoConflict` violated after 44 / 57 steps | 2 s, 5 s |
| `Scen4_trace1` | bfs | `NoConflict` violated after 20 steps (trace 1 without epoch-1 votes) | 2 min |
| `Scen4_trace1_exact` | bfs | `NoConflict` violated after 26 steps (trace 1 as expected) | 19 s |
| `Recover4_off_renot_safe` | sim | no error, 29,129 traces | 20 min |
| `Recover4_committed_{safe,flip,fork}` | sim | no error, 31,658 / 15,050 / 31,745 traces | 10-25 min |
| `Recover3_committed_safe` | bfs | no error to depth 19, 9,212,095 distinct states (cap) | 30 min |
| `Recover4_all_unsettled_safe` | sim | no error, 18,453 traces | 15 min |
| `Recover4_all_unsettled_flip`, `Scen4_trace3_flip` | sim | `FlipBound` violated after 43 / 36 steps | 3 s, 7 s |
| `Scen4_unsettled_fork` | sim | `Recovered` violated after 59 steps | 18 s |
| `Recover4_all_unsettled_{fork,two,two23}` | sim | no error, 32,835 / 8,880 / 21,708 traces | 5-10 min |
| `Recover4_all_prev_{safe,flip,fork,two,two23}` | sim | no error, 31,677 / 31,880 / 50,645 / 8,354 / 19,071 traces | 5-25 min |
| `Scen4_prev_fork` | sim | no error, 47,937 traces | 15 min |
| `Recover3_all_prev_safe` | bfs | no error to depth 19, 8,819,865 distinct states (cap) | 30 min |
| `Recover4_all_prev_fork` | bfs | no error to depth 19, 2,180,907 distinct states (cap) | 30 min |
| `Recover4_all_prev_perm_{safe,flip}` | sim | no error, 18,180 / 24,415 traces | 15 min |
| `Recover4_all_prev_perm_{fork,two}`, `Scen4_perm_fork` | sim | `Recovered` violated after 78 / 74 / 65 steps | 10 s |
| `Recover4_all_prev_perm_two23` | sim | `RecoveredEarly` violated after 75 steps | 11 s |
| `Recover4_all_hash_safe` | sim | no error, 17,868 traces | 15 min |
| `Recover4_all_hash_flip` | sim | `FlipBound` violated after 45 steps | 3 s |
| `Scen4_hash_fork` | sim | `Recovered` violated after 60 steps (random `fork` run: none in 32,067 traces) | 5 s |
| `Recover4_all_hash_{two,two23}` | sim | no error, 13,000 / 32,587 traces | 5 min |
| `Scen4_novotes_live` | sim | `Recovered` violated after 49 steps (no votes before the last epoch) | 2 s |
| `Recover4_all_prev_probe_{early,flip,c}` | sim | probes violated, as intended | < 2 s |

### RaiRecoverMany results

Switch sets: `patch` (IMPLIED all, settled, prev, strict, no re-notarization), `none`, `hash`, `perm` (the patch with that one change), `routeA` (IMPLIED off, settled, prev, strict, re-notarization, NOEARLY), `allup` (the upward Rule 2 over all first votes: IMPLIED off, all first votes, hash, strict, re-notarization, NOEARLY). `_down`: Rule 2 top-down; `_any`: the owner signs every child at once. Checks: `safe`, `flip`, `kof` (`KeptOrFinal`), `live` (`RecoveredMany`, epochs 3 and 4 ready), `two23` (`RecoveredEarly`, epochs 2 and 3 ready), `gap24` (`RecoveredMany`, epochs 2 and 4 ready), `early` (scripted early finality of A1 under the overlap rule: `NoConflict`, `ClosureThm1`, `FlipBound`, `KeptOrFinal`, `YieldNext`), probes; `_E3_bfs`: E = 3, `NoConflict`, `ClosureThm1`, `OneKept`, `FlipBound`, breadth-first. All other runs are random simulation; none is exhaustive, for B = 2 or B = 3.

| Config (B = 2; B = 3) | Result | Time |
|---|---|---|
| `Many2_none_live` | a: `RecoveredMany` violated after 60 steps; `ProbeNothing` violated after 14 | 8 s |
| `Many2_patch_{safe,flip,live,two23}` | b: no error, 7,942 / 11,120 / 10,698 / 29,971 traces | 10 min |
| `Many2_hash_flip`, `_hash_kof` | c: `FlipBound` violated after 41 steps; `KeptOrFinal` after 24 | < 10 s |
| `Many2_patch_early` | d: no error, 5,881 traces (S2 yields to the early-final block) | 10 min |
| `Many2_patch_early_probe`, `_early_s1`, `_probe_witness` | d: early finality reachable; S1 keeps a rival of the early-final block; a checkpoint keeps A_j while A_i has an epoch-1 witness | < 2 s |
| `Many2_perm_{safe,flip}` | e: no error, 8,068 / 10,674 traces | 10 min |
| `Many2_perm_live`, `_perm_probe_regain` | e: `RecoveredMany` violated after 76 steps; a replayed rival regains a settled vote | < 1 min |
| `Many2_patch_probe_regain` | no error, 4,755 traces (strict: no regained vote) | 5 min |
| `Many{2,3}_routeA_{safe,flip,kof}` | no error, 13,677 / 18,986 / 18,050; 5,676 / 8,969 / 8,104 traces | 10 min |
| `Many{2,3}_routeA_{live,two23,gap24}` | no error, 13,106 / 33,413 / 22,570; 5,370 / 13,532 / 9,876 traces | 10 min |
| `Many{2,3}_routeA_early` | no error, 13,200; 4,468 traces | 10 min |
| `Many{2,3}_routeA_probe_{flip,early}`, `_early_s1` | probes violated, as intended | < 10 s |
| `Many{2,3}_routeA_any_up_safe` | `ClosureThm1` violated after 20; 24 steps | 2 min; 45 s |
| `Many{2,3}_routeA_any_down_safe` | no error, 7,782; 1,894 traces | 10 min |
| `Many{2,3}_routeA_down_{flip,kof,live,two23}` | no error, 19,392 / 18,039 / 13,033 / 32,776; 10,100 / 13,088 / 5,338 / 13,143 traces | 10 min |
| `Many{2,3}_routeA_down_{early,gap24}` | no error, 9,227 / 24,019; 3,497 / 11,111 traces | 10 min |
| `Many{2,3}_routeA_down_early_s1` | probe violated, as intended (S1 keeps a rival of the early-final A_1) | 1 s |
| `Many2_routeA_down_hashpick_flip` | `FlipBound` violated after 35 steps (hash order instead of the previous checkpoint's block) | 3 s |
| `Many2_routeA_down_allvotes_flip` | no error, 37,326 traces (every first vote counted; NOEARLY already keeps early votes away from a kept position) | 10 min |
| `Many2_routeA_any_down_E3_bfs` | bfs: no error to depth 17 (level 18 reached), 21,330,431 distinct states, 12.4 M left on queue (cap) | 3 h |
| `Many2_routeA_any_up_E3_bfs` | bfs: `ClosureThm1` violated after 15 steps, the shortest such run (calibration: the upward order fails within the depth the down-order search cleared) | 18 min |
| `Many2_routeA_any_down_allvotes_safe` | no error, 32,503 traces (every first vote counted, downward order, forking owner; 30-min run) | 30 min |
| `Many2_routeA_any_down_allvotes_E3_bfs` | bfs: no error to depth 15 (level 16 reached), 7,134,325 distinct states (cap) | 1 h |
| `Many2_allup_safe` | `ClosureThm1` violated after 32 steps (B = 3: none in 6,113 traces) | 6 min |
| `Many2_allup_nc`, `Many2_allup_hole_tight` | `NoConflict` violated after 57 / 40 steps (B = 3 `nc`: none in 7,323 traces) | 3 min, 1 min |
| `Many2_allup_hole`, `_hole_down` | no error, 32,539 / 36,553 traces (looser script; `_down` with Rule 2 top-down) | 15 min |
| `Many{2,3}_allup_any_up_safe` | `ClosureThm1` violated after 21; 32 steps | 2 min; 4 min |
| `Many{2,3}_allup_any_down_safe` | no error, 8,294; 2,025 traces | 10 min |
| `Many{2,3}_allup_flip`, `_allup_kof` | `FlipBound` violated after 35; 41 steps; `KeptOrFinal` after 38; 37 | < 30 s |
| `Many{2,3}_allup_{live,two23,gap24}` | no error, 14,268 / 32,803 / 20,241; 5,489 / 14,797 / 10,921 traces | 10 min |

### Traces

**Trace 1 (`Scen4_trace1_exact`, IMPLIED off, no re-notarization).** Epoch 1: 1, 3, 4 vote A; 2, 5 vote A'. Epoch 2, before S1: 1, 3, 4 re-vote A early; with 6, A has an epoch-1 exclusion witness and an epoch-2 NC, so 1, 3, 4 final-vote A under the overlap rule and FC_2(A) forms with 6. Closure 1 selects 2, 5, two of 1, 3, 4 and 6: A' alone reaches r and is kept (R1). 1, 2, 3, 5 install S1 and vote C on the kept A' at once; with 6 that is FF_2(C), which finalizes A'. A and A' are both final. `Scen4_trace1` finds the same with no epoch-1 votes at all. The re-notarization rule (`off_renot`) or ancestor support (`IMPLIED = all`) removes it.

**Experiment 2 (`committed`) contradicts the expectation.** No violation was found. In the model an early first vote for a child adds no support at the parent's position, but a fast certificate counts settled first votes only, which are not exempt, so the expected run needs an `XW(A)` and an `FF(C)` of one epoch, seven correct validators; across epochs C gets a fresh record of the new origin.

**Flip-flop (`Scen4_trace3_flip`, all votes, hash).** 1, 2 vote A; 4, 5 vote A'; 3 votes only after installing. S1 keeps A (P1). In epoch 2, 4 and 5 vote A' early: with 6 that is an epoch-1 witness for A' (late notarizations), which discharges P1, and Rule 2, counting early votes, keeps A' (R2). S3 keeps D, hence A, again: two flips. `Many2_hash_flip` is item c: S1 keeps A2 on validator 6's tie-breaking listing, 3 and 5 late-notarize A1, the epoch-1 witness discharges the record, and S2 keeps A1 by hash among unsettled votes; S3 finalizes A2.

**Literal `Recovered` fails in every configuration (`Scen4_novotes_live`).** If nobody votes before the last epoch, one ready epoch splits 3 to 2 and validator 6 withholds the fourth final vote. Recovery needs two good epochs, as the paper's theorem says.

**Item a (`Many2_none_live`).** S1, S2, S3 keep nothing. In epoch 3 A1 gets 3 first votes and A2 2; validator 6 releases its A1 vote only after 2 and 4 have left, so only 3 final-votes; epoch 4 splits again.

**Item e (`Many2_perm_live`).** S1 to S3 keep A1, but under permissive attachment validators holding an epoch-1 witness for A2 vote it after installing; epochs 3 and 4 split.

**The upward Rule 2 over all first votes loses a fast-final child (`Many2_allup_hole_tight`, `Many2_allup_safe`).** Epoch 1: 1 votes A1, 2 votes A2; S1 keeps A2 (P1, 6's listing breaks the tie). Epoch 2, before S1: 2 and 4 vote A2 early, 3 and 5 vote A1 early (each may have seen one block only). After S1: 1 votes A2 settled, so A2 has an epoch-2 NC {1, 2, 4, 6} and its child C2 may be voted under the re-notarization rule; 1, 2, 4, 5 vote C2 settled: with 6, FF_2(C2), so C2 and A2 are final. Closure 2 selects 2, 3, 4, 5, 6. Rule 2 at v counts all first votes: A1 has 3, 5, 6 = r, A2 has 2, 4; A1 is kept (R2), and C2, whose parent conflicts, is not eligible at v+1 (`ClosureThm1`). Every correct validator installs S2 and votes only A1: in epoch 3, 1, 2, 3, 4 vote it settled, FF_3(A1) with 6 (`NoConflict`). The proof's fast-certificate case bounds rivals at B's own position by f + p; a rival of an ancestor that has only an NC can reach r. `Many2_allup_nc` finds the mirror image unscripted. With settled votes only (route A) the early votes for A1 do not count, but a forking owner that signs C1 at once reaches the same shape in the first fork epoch, where nothing is kept yet (`Many2_routeA_any_up_safe`: 2, 3, 4 vote A1, 1 and 5 vote A2, then 1, 2, 4, 5 vote C1; S1 keeps A2). Running Rule 2 from v+1 down, with a retained block keeping its ancestry and no Rule 2 at a held position, removes both (`_any_down_safe`, `_hole_down`); so would ancestor support, under which no correct validator supports A1 at v and votes a child of A2 at v+1 in one epoch.

### Abstractions (both modules; details in the headers)

Adds behaviours: global finality from formable certificates (1); every published block reaches everyone, and a correct validator votes any admissible block (2); a correct report may omit an N tag for a held NC (3); `BYZ = all` (4); a Byzantine report lists any published blocks (5); the manifest discharges any dischargeable set containing the forced records (6); children published as soon as a checkpoint keeps their parent, before the owner could know (7); `NOEARLY` models only the checkpoint half of the paper's rule. Removes behaviours: correct reports carry no F tag for an observed certificate (3); `BYZ = release` never releases Byzantine final votes, so the recovery runs miss attacks that need one; the owner policy (`OWNER = policy`) signs children only of kept or final blocks, so safety against a forking owner is only checked by the `_any_` runs; in RaiRecoverMany epoch-1 votes are scripted unless `OWNER = any`; one account, two positions, one committee, N = 6, E = 4.

## `RaiCloseRestart`: restarts before the report freezes

`RaiClose` with a validator that restarts before freezing its report. Its final vote stands; `RESTART` selects what happens to the notarization behind it: `durable` (kept), `lose` (the report built after the restart has no N tag and lists the first vote in G), `hold` (the report waits until the notarization can be assembled again without the Byzantine validator: from correct first votes, or from a correct validator that holds it), `release` (as `hold`, but the report is also signed without the tag once frozen reports of others leave too few possible final voters). `MAXLOST` bounds such restarts per closure. `P = 1` with five correct validators is N = 6; `P = 0` with three is N = 4. `_keep` and the `_safe`/`_stuck` configs set `MAXKEEP` (Rule 2 keeps a side). `NoStuck`: no state where every correct validator still to report can never freeze while the Byzantine validator stays silent. Run with `TLA2TOOLS=... ./restart.sh <configs>`.

| Restart | N = 4 | N = 6 |
|---|---|---|
| `durable`, safety | no error, 40,652 states | no error, 278,404 states (the same as `Close_TRUE_maxkeep`) |
| `durable`, `NoStuck` | no error | no error |
| `lose`, one restart | `NoConflict` violated, 13 steps | no error, 1,217,147 states |
| `lose`, two restarts | not run | `NoConflict` violated, 19 steps |
| `hold`, safety, up to 3 restarts | no error, 0.1 M states | no error, 0.8-1.2 M states |
| `hold`, `NoStuck`, one restart | violated, 7 steps | violated, 10 steps |
| `release`, safety, up to 3 restarts | no error, 0.1 M states | no error, 1.1-2.2 M states |
| `release`, `NoStuck`, one restart | no error | no error |
| `release`, `NoStuck`, two restarts | violated, 8 steps | no error |
| `release`, `NoStuck`, three restarts | violated, 8 steps | violated, 12 steps |

`release` is stuck only when every correct final voter of a block restarted and lost the notarization: q - f validators, two at N = 4 and three at N = 6.

The model has one position, two closures, no early voting, and no committee change, and `MAXKEEP` approximates the keep pass (most reporter votes of any kind, not settled votes with the predecessor's block preferred).
