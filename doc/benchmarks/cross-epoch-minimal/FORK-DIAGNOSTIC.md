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
