# TLA+ models for the RAI handoff (N = 6, f = p = 1, one account position; RaiPrefix: two positions, N = 4 and 6)

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
| `Overlap_noDischarge` | RaiOverlap | discharge off | `SameVerdict` violated after 15 steps (Case 2) | 1 min |
| `Overlap_noLateEv` | RaiOverlap | late votes inadmissible | `WitnessAdmissible` violated after 14 steps | 1 min |
| `Overlap_noSplit` | RaiOverlap | NC split off | `UsableReports` violated after 9 steps | 10 s |
| `Overlap_noSplit_safety` | RaiOverlap | NC split off | other four invariants hold, 5,485,444 distinct states | 14 min |
| `Overlap_probe1`, `_probe2` | RaiOverlap | all on | probes violated, as intended: overlap finality and "final block versus lock on its rival" are reachable | < 1 min |

`RaiHandoff.tla` combines early voting, two closures and a changing committee. `RULE = TRUE` is "no fast path on early votes".

| Config | Committees | Certificates | Result | Time (2 cores) |
|---|---|---|---|---|
| `Handoff_same_paper` | same five | paper | `ClosureThm1b` violated after 22 steps (Case 3) | 11 min |
| `Handoff_disjoint_paper` | disjoint | paper | `ClosureThm1b` violated after 23 steps | 15 min |
| `Handoff_same_rule` | same five | early-vote rule | no error, 8,347,138 distinct states | 15 min |
| `Handoff_disjoint_rule` | disjoint | early-vote rule | no error, 3,707,876 distinct states | 17 min |
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
