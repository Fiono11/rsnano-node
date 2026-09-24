# Bounded per-fork diagnostic

`fork_diagnostic.py` runs one instrumented candidate with 5% probabilistic forks,
45,000 primary publications, six nodes, and 8-second epochs. This is not a paired
performance experiment. Its 156-second ceiling comes from the prior baseline
ceiling; it is not a successful calibration of this changed client/workload.

The client logs generated and published fork pairs independently of confirmation,
with account, parent, both hashes and publication time. The controller requires
all generated pairs to have publication records and the generator to finish.
It queries accepted checkpoint snapshots across six nodes, even after the client
finishes, and records each branch as included, safely discarded by a conflicting
finalized checkpoint block with the same account/parent, or unresolved. R/N/F
inclusion counts as terminal here. Absence alone never counts as safe discard.
Other possible discard justifications are not inferred; they remain unresolved.

Completion requires every recorded branch to have a supported outcome on every
node and a common checkpoint epoch/hash across the nodes. This is a local-node
instrumentation check, not independent validation of full signed decision proofs
or evidence that the remaining protocol gaps are implemented. Non-fork pending
work remains visible separately in RPC snapshots.

`final_state` accepts opt-in `diagnostic: true`, returning schema-1 checkpoint
entries plus per-epoch live/frozen reconstruction entries and report usability.
Ordinary RPC calls do not construct these large snapshots. Logs carry process
IDs, and the controller preserves the node-to-PID mapping. Snapshots are taken
on checkpoint changes and once near the deadline. This intentionally adds
observation overhead; do not compare its throughput or latency to earlier runs.

Termination p50/p95/p99 are sampled observation upper bounds, approximately the
2-second polling interval plus snapshot/RPC time. They describe only branch
hashes observed terminal; unresolved hashes remain explicitly censored and fail
completion. All timestamps and raw outcomes are preserved for inspection.

Outputs include `fork-manifest.json`, `fork-outcomes.json`, `observations.jsonl`,
full per-node snapshots, `node-pids`, logs and result JSON. Generated node data
is deleted after process cleanup and evidence/configuration preservation.

## Recorded diagnostic: 1d9680f64

One run used the instrumented node and repaired client, with a 156-second
baseline-derived deadline (157.43 seconds including controller overhead).
It stopped at the deadline; the input-complete marker was not observed.
This is not a completed 45,000-operation workload or a performance comparison.

| Observation | Result |
|---|---:|
| Generated and published fork pairs | 1,123 |
| Recorded branch hashes | 2,246 |
| Branches with supported terminal outcomes on all nodes | 0 |
| Unresolved branch hashes | 2,246 |
| Accepted checkpoints across six nodes | 0 |
| Termination p50 / p95 / p99 | unavailable / unavailable / unavailable |

All six nodes had the same live T root (15,009 entries) and frozen T root
(14,881 entries). Each reconstructed all five peer T snapshots. Completed
peer G reconstructions by node were only 2, 1, 2, 1, 2, 2; with the local
report, usable reports remained below the closing weight threshold. Epoch 0
never closed. The stall was visible before the large near-deadline snapshots.

This narrows the observed blocker to G reconstruction/evidence, rather than
T-view divergence. These snapshots do not expose the derived G working root
or missing first-vote evidence, so they cannot distinguish those failure
conditions. ReportService retries derivation from locally recorded votes;
it has no explicit missing-G signed-vote retrieval path. That is an implementation
gap consistent with this observation, not proof of the sole cause.

All branches are unresolved under the conservative checkpoint classifier;
this does not prove that none obtained ordinary block finality or another
unrecorded discard justification. No confirmation latency is substituted for
termination latency, and no performance-pass claim is made.

Evidence is retained under
`/tmp/rai-cross-epoch-artifacts/fork-termination-diagnostic-v1/pair-00-candidate/`:
`result.json`, `fork-manifest.json`, `fork-outcomes.json`,
`reconstruction-analysis.json`, six raw snapshots, logs, RPC and configuration.
Generated node databases were removed after shutdown; `data-cleanup.json`
records cleanup. The original dirty-tree backup remains intact.

Validation: 30 client tests passed (one existing ignored), 797 RAI node tests,
324 RPC-message tests, 28 RPC-server tests and 16 Python controller/harness
tests passed. The default RPC-server build check and instrumented release
build also passed.

Next: expose expected/derived G roots and missing first-vote evidence, then
repair authenticated epoch-scoped vote availability/replay with focused tests.
Do not weaken evidence checks or treat missing branches as discarded. Run one
bounded termination diagnostic after the repair; resume paired fork performance
measurements only after completion and a matching baseline calibration.
