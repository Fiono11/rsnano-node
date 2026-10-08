# TLA+ models for the RAI handoff (N = 6, f = p = 1, one account position)

Run with `./run.sh <module> <config>`, for example `./run.sh RaiClose Close_TRUE`.
`run.sh` expects `tla2tools.jar` (TLA+ tools 1.8.0) one directory up and Java 11 or newer.
The `out_*.txt` files are the outputs of the runs reported below, including the violating traces.

| Config | Module | Switches | Result | Time (2 cores) |
|---|---|---|---|---|
| `Close_FALSE` | RaiClose | Fix B off | `Thm64` violated after 19 steps | 35 s |
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
| `Handoff_same_paper` | same five | paper | `Thm64b` violated after 22 steps (Case 3) | 11 min |
| `Handoff_disjoint_paper` | disjoint | paper | `Thm64b` violated after 23 steps | 15 min |
| `Handoff_same_rule` | same five | early-vote rule | no error, 8,347,138 distinct states | 15 min |
| `Handoff_disjoint_rule` | disjoint | early-vote rule | no error, 3,707,876 distinct states | 17 min |
| `Handoff_overlap3_rule` | three shared | early-vote rule | stopped at depth 18 with 9 million states and a growing queue; no violation up to there | not completed |
| `Handoff_Probe*` | same five | early-vote rule | probes violated, as intended | < 5 min |

`Handoff_overlap1_rule` and `Handoff_overlap4_rule` (one and four shared members) were still running when this archive was made.

The header comment of each module lists its abstractions. No model covers more than one position, a third epoch, restarts, or any N other than 6.
