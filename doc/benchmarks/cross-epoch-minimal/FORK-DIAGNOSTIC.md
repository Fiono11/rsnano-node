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


## Signed residual replay implementation

Validated account-vote batches are now retained with their original signatures,
indexed by epoch, reporter, block hash and vote kind. Retention begins before
block placement, so an initially unknown fork does not lose its signed message.
The existing epoch retention boundary also trims these message references.
Once per second, each node replays the original batches covering its own frozen
G using the existing certificate-evidence message flag. Receivers still verify
signatures; the flag permits retained evidence past the duplicate filter. A
batch is sent once per report per tick even if it covers several G hashes.
Retained reports continue serving lagging peers after local checkpoint closure.

This is periodic gossip of retained signed votes, not a new hash/sketch trust
path. It does not weaken the signed G-root or first-evidence checks. The archive
is in memory, not crash-durable storage, and is bounded by retained epochs rather
than a byte budget. Replaying a batch may also carry votes for hashes summarized
by T because changing its hash list would invalidate its signature. Its memory,
network and verification costs still require paired performance evaluation.

Opt-in diagnostics now include each report's expected G root, locally derived
G root, vote-kind entries and first-evidence completeness, plus the reporter's
frozen G evidence. These allow direct comparison of missing/extra hashes and
missing first votes without treating a hash-only commitment as signed evidence.


## Signed replay diagnostic: 0d933ad5c

The single predeclared run reached the 156-second deadline (157.13 seconds
including controller overhead). The input-complete marker was not observed.
It recorded 1,124 generated/published fork pairs, 2,248 branch hashes, zero
checkpoint-supported terminal branches and 2,248 unresolved branches. No node
accepted a checkpoint. Termination **p50 / p95 / p99 are all unavailable**.
No completed client latency histogram or paired performance comparison exists.

Near-deadline snapshots distinguish the remaining reconstruction conditions:

| Node | Peer T reconstructed / 5 | Peer G complete / 5 |
|---|---:|---:|
| 0 | 4 | 4 |
| 1 | 5 | 0 |
| 2 | 4 | 0 |
| 3 | 4 | 4 |
| 4 | 4 | 4 |
| 5 | 4 | 1 |

For the 12 incomplete G reconstructions whose T was available, the derived
root differed from the signed root. Comparison against the reporters' frozen
G evidence found missing hashes, no extra hashes, and no missing-first-vote
condition among the locally reconstructed hashes. Across those reconstructions,
72 distinct missing hashes were all published fork branches. None of a node's
missing hashes appeared in its other reconstructed G sets or its own frozen G.
The raw data distinguish this from a matching-root/missing-first-evidence failure.
They do not establish whether each missing vote packet was received.

Three nodes entered close round 0, while the others remained below the usable
report threshold. One reporter's T also could not be reconstructed on five
peers; logs include `TooLarge(766)` for the existing bounded difference reply.
This run therefore does not establish that signed replay alone restores closure.
The earlier run had different randomized forks and timing: the change in peer
completion counts is descriptive, not a controlled effectiveness comparison.

The source exposes a remaining availability/placement gap: `record_votes` places
a hash only through an active election or retained finalized instance, otherwise
skipping the metadata record. Evidence replay also skips election application
for hashes absent from the vote router. The new signed archive preserves the
message, but replaying signatures does not itself transfer a missing owner-signed
fork block or establish its account/height/parent. The run is consistent with
this gap; a per-hash received-message/placement trace is still needed to prove
which path affected each missing branch. Reconstructed G metadata must not be
used as an unauthenticated replacement for block data.

Next implementation work is retained fork-block transfer and authenticated
placement/replay, plus bounded multi-part T-difference reconstruction for the
observed oversized reply. These are separate changes requiring focused tests;
no repeated performance pairs should start before fork termination works.
This in-memory signed replay is not the paper's complete durable evidence layer.

Evidence: `/tmp/rai-cross-epoch-artifacts/fork-residual-replay-diagnostic-v1/`.
The attempt retains `result.json`, per-fork manifest/outcomes, all six snapshots,
`residual-analysis.json`, its analysis script, logs, RPC, saved configuration and
`data-cleanup.json`. Generated databases were deleted after process shutdown.
The release binary and unchanged repaired client are hashed in the run manifest;
source and build details are in the artifact-root `build-manifest.json`.
Validation: 799 RAI node tests passed, default RPC-server check passed, release
build passed. No other benchmark or replacement attempt was run for this change.


## Fork data and paged T reconstruction

The next isolated change addresses the two observed gaps:

* T differences retain their known source and signed target roots, but are split
  into pages of at most 600 edits, with at most 256 pages per transfer. One
  bounded partial transfer is kept per report. Pages can arrive out of order or
  repeat. No partial transfer is usable; all removals precede all additions and
  the completed reconstruction must hash to the signed T root. A failed root
  check drops the partial transfer so a correct retry can recover. There is no
  empty-source or full-target fallback. Periodic requests retry missing pages.
* Each reporter retains and periodically publishes block data for its frozen G
  alongside original signed votes. For received signed votes without election
  placement, reconstruction consults validated ledger data or verifies a state
  block's owner signature and same-account parent ancestry from locally held
  fork data. Only a matching vote already retained after signature validation
  can acquire this placement. This establishes account/height/parent metadata,
  not notarization, eligibility or finality. Unknown ancestry and invalid
  signatures leave the vote unplaced. Legacy/epoch-signer variants are not
  inferred; raw ancestry traversal is bounded to 256 steps.

T reply framing now includes page number/count. The instrumented node and client
must be built together for this experimental wire revision; mixed-version RAI
peers are not claimed compatible. Checkpoint pages keep their outer paging and
encode each embedded difference as one page. Frozen baseline binaries remain
unchanged. Retained blocks and signatures are in memory, not crash-durable.

Predeclared evaluation: one candidate-only fork diagnostic with the existing
156-second baseline-derived ceiling, six nodes, 5% probabilistic forks, 45,000
requested publications and 8-second epochs. Rebuild the client for the wire
change without changing workload logic. This is not a matching baseline
calibration or a performance comparison. Preserve the attempt whether or not
termination succeeds; delete its generated databases after evidence capture.


### First data-transfer attempt and earlier retention fix

Candidate `c9668a484` reached the 156-second deadline (157.88 seconds with
controller overhead): 1,069 fork pairs / 2,138 branches, all unresolved under
checkpoint tracking; no checkpoint or input-complete marker. Termination
p50/p95/p99 remain unavailable. All six nodes reconstructed all five peer T
sets. Completed peer G counts were 4, 3, 2, 3, 0, 4. Remaining G mismatches
contained missing hashes, no extra hashes and no missing-first condition among
reconstructed hashes. Evidence is in `fork-data-diagnostic-v1/` in the artifact
root; databases were deleted after shutdown.

The new data counters exposed the retention timing gap directly: one reporter
could serve only 824 of its 852 frozen G block hashes, and other reporters also
had deficits. Copying blocks at the report boundary cannot recover a branch
already discarded by the election and ledger. The follow-up retains each
locally voted block at `mark_kudzu_voted`, under the same AEC lock as the local
vote record, before election deletion. These payloads survive election erasure
and use the existing vote-epoch retention boundary. A focused regression test
erases the election after its first vote and verifies the original block remains
available. This remains in-memory retention, not durable vote persistence.

A second 156-second diagnostic is justified by this specific new source change;
it is not a replacement for the first attempt and both results remain reported.


### Earlier-retention follow-up: a808dba3b

The follow-up reached its 156-second diagnostic deadline (162.07 seconds with
RPC completion, evidence collection and shutdown). It recorded 1,746 published
fork pairs / 3,492 branch hashes; the input-complete marker was not observed.
All six nodes installed the same epoch-0 checkpoint. Every reporter could now
serve every frozen G payload in epochs 0 and 1. Epoch 1 did not close; logs
still showed unplaced votes and the final RPCs showed unchecked ledger blocks.

Large diagnostic RPCs timed out on four nodes. The original online result
therefore records zero all-node terminal observations, but this is not evidence
that zero branches terminated. A separate, reproducible post-run corroboration
uses one full checkpoint snapshot, the exact matching epoch/root in all six
final RPCs, and all six matching `EPOCH_CLOSED` events (logged after checkpoint
installation). It establishes 867 terminal branches: 761 included and 106
safely discarded by a conflicting finalized checkpoint entry. The remaining
2,625 are unresolved by the conservative classifier. The raw online result
remains unchanged alongside `corroborated-result.json` and per-branch evidence.

For those **867 supported branches only**, publication-to-observation upper
bounds are p50 **10,731 ms**, p95 **13,165 ms**, p99 **13,611 ms**. The observation
time is the later of the first full checkpoint snapshot and the latest matching
six-node close event. These are post-run reconstructed, conditional upper bounds,
not a full-workload latency distribution or a baseline performance comparison.
Unresolved branches are censored, not silently counted as successful.

The two successful full snapshots covered epoch 0 and had all five peer T/G
sets reconstructed. They do not establish epoch-1 reconstruction completeness.
The report resolver had another data-access gap: an evidence block waiting in
the ledger's unchecked queue for its predecessor was neither in the ledger nor
the fork cache and could not be placed. This source gap is consistent with the
run, but the incomplete snapshots do not identify every affected epoch-1 hash.
Evidence: `fork-early-retention-diagnostic-v1/`; generated databases were deleted.

### Pending-ledger evidence and observation repair

A bounded 65,536-block cache now retains owner-signature-verified state blocks
received as evidence after ingress work validation, independently of ledger
admission. Placement still requires verified same-account ancestry and an
already validated voter signature; this does not make the block eligible or
final. Reporters also retain/replay available same-account ancestry, bounded
to 256 steps per chain. Tests cover child-before-parent arrival and subsequent
placement while both blocks remain absent from the ledger, plus invalid owner
signatures. This remains an experimental in-memory availability layer.

The lightweight `epoch_locks` RPC now reports the installed checkpoint hash
from the same atomic checkpoint snapshot as its epoch and locks. The controller
can reuse contents only for an exact matching epoch/root reported independently
by each node. It retains historical all-node terminal witnesses across later
checkpoints. The controller still requires complete workload generation and
supported outcomes for every recorded branch before declaring completion.

Predeclared next attempt: one 156-second diagnostic for this specific cache and
ancestry change, with the same client/workload. Preserve all preceding attempts;
this changes observation overhead and does not support a performance comparison.


### Pending-ledger evidence result (0b6a0cba5)

The 156-second attempt completed publication of 45,000 primary blocks and
2,218 fork pairs (4,436 branches). The observer supports 870 branch outcomes:
761 included and 109 discarded by a conflicting finalized checkpoint entry.
The remaining 3,566 lack an all-node witness; this does not establish that all
of them failed to terminate. Conditional publication-to-observation upper
bounds for the 870 supported branches are p50 17,231 ms, p95 20,200 ms and
p99 20,621 ms. This candidate-only diagnostic is not a performance comparison.

Five nodes installed epoch 2; PR2 remained at epoch 0. Full checkpoint contents
were obtained only for epoch 0; eight diagnostic RPCs timed out. The old raw
result's `checkpoint_consistent=true` describes cached epoch-0 contents, not
the latest installed network state. The controller now checks latest reported
installed roots independently from its historical contents cache.

The controller also previously waited only for its client leader after sending
SIGTERM. A child could remain alive after the leader exited and RPC ports
closed. The latest log continued about 156.6 seconds beyond the reported end.
All recorded task node PIDs were subsequently checked and had exited. The
49-run timestamp audit found no logged activity overlapping the next recorded
run, but log silence does not prove process exit. Historical raw results are
preserved. Future cleanup waits for the entire task process group, escalates
to SIGKILL if necessary, and preserves databases if cleanup fails.

Evidence: `fork-raw-evidence-diagnostic-v1/` and
`process-lifetime-log-audit.json`; generated databases were deleted.

### Election-lifetime certificate retention

PR2 held six epoch-1 report commitments but only its own was usable. Other
replicas rejected its advertised sources as unknown. A regression test shows
that removing a notarized election erased its N entry from the live epoch
projection. Checkpoint installation removes retained/omitted elections, so
nodes at different closure progress can lose their common reconstruction view.
This is a concrete source bug; the logs alone do not prove it is the only cause.

Retain historical certificate observations across election deletion, scoped to
the original epoch and the existing vote-evidence retention window. Apply the
usual selected-final-prefix projection afterwards; do not change a frozen
report, create finality from N, or import successor observations. The regression
test fails before this change and passes afterwards. This archive remains
in-memory and is not full durable certificate verification.

The observer requests checkpoint contents without the expensive report inventory
snapshot. Predeclare one bounded diagnostic at the same 156-second baseline-
derived ceiling to test this fix. Preserve its result even if incomplete. Only
after a complete correctness outcome should matched fork performance comparisons
resume. Baseline instrumentation must be identified separately from its frozen
protocol source; inclusion and explicit finality must be reported separately.
