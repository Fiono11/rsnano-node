# Fork-run diagnosis using termination rather than universal finalization

Run: `step1-reconciliation-refresh-forks5-smoke/pair-00-baseline/` under
`/tmp/rai-cross-epoch-artifacts/`. This ran frozen HEAD with the old shared
recovery client. Candidate was not run. The generated databases were deleted
as requested; logs, RPC snapshots, configuration and failure summary survive.
`termination-diagnosis.json` now extracts the relevant evidence.

## Findings from the saved run

The client stopped making confirmation progress at 44,284/45,000. Its completion
condition demands a confirmation for each primary publication. It has no
checkpoint-inclusion or justified-discard outcome, so 716 missing confirmations
cannot be interpreted as 716 unterminated forks.

Nevertheless, this run was not merely waiting for safely discarded forks:

* All six nodes reported `all_terminated=false`.
* No node had closed epoch 2. Its close was not ready or started.
* Five epoch-2 reports were held, but several nodes had only four usable
  reports, weight 226645542765589078064753158236664579174 versus required
  275413069270999409019212035103937639479.
* They repeatedly requested reporter `65A90023…`'s frozen root
  `5EC7425967BBF49227E673A0593FEA88365CAF5B8C38019B01B6B67BA163D342`.
  No successful reconstruction or residual derivation for that report is
  logged. The combined log also repeatedly shows UnknownSource refusals.
* PR2 had learned the epoch-1 close value, but had not installed the predecessor
  state required for epoch 2. It reported 13 pending entries in epoch 1 and
  had cemented 31,333 blocks versus 44,364 on the other nodes.
* Epoch 2 still had about 411 pending entries and roughly 200 locally empty
  outcomes per node. Local Empty/Single outcomes are not by themselves proof
  of checkpoint inclusion or a justified discard under the revised protocol.

Thus there is both an incorrect completion metric and a reconstruction/closure
stall. Logs combine nodes and rate-limit some events; they identify the blocking
stage and target report, not the exact missing/differing entries. The old run
contains neither a per-generated-fork inventory nor full checkpoint membership,
so no reliable included/discarded/unresolved count can be reconstructed from it.
The earlier calibration-timeout label is retained as a measurement outcome,
not relabeled as proof that all unconfirmed fork hashes failed to terminate.

## Independently reproduced client bug and correction

`SpamLogic` queued only the primary hash for latency/republication. It mapped
an alternative hash back to that primary only after recovery selected a lock.
`AccountMap::confirm` likewise ignored a directly confirmed alternative because
its unconfirmed map was keyed by the primary hash. A fork winning directly
could therefore leave the primary awaiting confirmation and the account's
bookkeeping stuck, despite a successful winning-branch confirmation.

The client now registers the alternative when publishing a fork, counts the
first confirmation of either member once, stops republishing the primary, and
moves account frontier/receivable bookkeeping to the actual confirmed hash.
Duplicate or late observations do not increment the counter or roll back the
frontier. This does not declare the losing hash finalized.

Metrics explicitly label their unit as a primary publication or its confirmed
fork alternative, include `alternative_confirmed`, and state
`checkpoint_termination_tracked=false`. This is still a confirmation metric,
not the required fork-termination metric. The pinned client used for earlier
runs remains unchanged, and earlier results are not recomputed.

Validation: 30 nanospam tests passed, including the new direct-alternative and
exactly-once confirmation/republication regressions; one existing test ignored.
No additional benchmark batch was run to try to obtain a passing result.

## Required before the next fork measurement

1. Record each generated fork's account, position/parent, both hashes, admission
   epoch and publication time. Persist this small manifest independently of
   the deletable node databases.
2. Observe the verified checkpoint state and explicit per-hash disposition:
   included (R/N/F as justified), safely discarded with its conflict/selection
   justification, or unresolved. Omission, local timeout and cementing counts
   alone are insufficient. Preserve checkpoint identity and evidence references.
3. Stop on completion of those dispositions, not confirmation of every hash.
   Report fork-termination p50/p95/p99 separately from confirmed-operation
   goodput and latency. A retained R block has terminated for this checkpoint
   criterion even though owner recovery/finality may happen later.
4. Capture per-node frozen/live reconstruction roots and differences for a
   bounded diagnostic run to identify why the common source is missing. The
   existing candidate's on-demand refresh correction addresses stale cached
   sources but was not exercised by this frozen-baseline run and is not proof
   that it resolves this fork case.

Do not advance to a five-pair fork performance gate using the current
confirmation-only completion and all-blocks-cemented settlement predicate.
