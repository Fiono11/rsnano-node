# Implementation boundary and remaining work

This branch starts at `5e037cfe0527d7b06b573456c7c876568ce179ea` and implements
the first closure correction and the measurement gate. It does **not** yet
implement every requirement of `RAI_revised_cross_epoch_lock.docx`.

## Implemented in step 1

* Only selected F-tagged evidence extends finalized prefixes. A sole surviving
  candidate, represented N entry, or recovery threshold does not finalize.
* The checkpoint distinguishes retained ancestry (wire tag 0), explicit finality
  (1), represented notarization (2), and recovery-only protection (3).
* Lock metadata is part of a versioned state commitment. Checkpoint pages carry
  the extra tags; report reconstruction continues to accept only N/F tags.
* Parent traversal rejects missing ancestry, wrong account/height, and conflicting
  explicit finality. The builder rejects duplicate reporters and conflicting
  represented notarizations. Full descendants of a finalized-away fork are pruned.
* Below-threshold new residual branches can be omitted; inherited unresolved
  branches and their lock kinds survive omission from subsequent reports.
* The experimental epoch-start RPC now captures full confirmed genesis ancestry.
  The earlier frontier-only representation used synthetic zero parents, which
  strict reconstruction correctly rejected when setup reports included the same
  blocks with their actual parents. Initialization rejects unconfirmed or missing
  genesis data and leaves it outside the measured publishing window.

* A selectively adapted owner recovery path from the saved dirty tree exposes
  maximum-depth retained tips via `epoch_locks`. The shared client can publish
  a fresh change block above a tip it owns. Priority scheduling skips retained
  positions and first-vote attachment accepts a retained tip as the parent;
  receive sources still must be confirmed. This is not the full eligibility rule.
* The client reports `recovery_created` separately. Primary completion and latency
  include the time spent waiting for recovery; extra children do not inflate
  primary goodput. Both node versions must use this same instrumented client.

`build_state` is a structural builder over evidence the caller is expected to
have validated. Its F tags do not themselves constitute cryptographic finality
proofs. Complete verification of the signed evidence, transfer dependencies and
application replay is still required below. Unit-test fixtures exercise state
semantics, not a replacement for certificate verification.

## Required before claiming the revised protocol

1. Replace weighted thresholds and joint checkpoint agreement with the paper's
   equal-weight parameters and old-committee decision / successor verification.
2. Persist per-identity first/final voting decisions and terminal state before
   releasing signatures; enforce the preceding-epoch first-vote lock. Retain
   evidence independently of active-election eviction or finalization.
3. Validate the complete selected ancestry and receive dependencies. Implement
   current-epoch complete parents, full branch eligibility, both overlap
   exceptions, and predecessor-backed recovery-lock supersession (Rule 3).
4. Validate F/N memberships from retained signed votes, reconstruct G from each
   reporter's own signed votes, and verify all report context bindings. A signed
   root and internally plausible metadata are insufficient evidence.
5. Have successors reconstruct the selected reports and recompute BuildState
   before installation. Preserve compatible live finality and apply operations
   exactly once, including certificates assembled after a checkpoint decision.
6. Test real membership replacement, delayed/withheld evidence, recovery and
   growing retained history. Ordinary no-fork tests alone cannot establish this.

The checkpoint structure conservatively retains inherited locks. It does not
attempt Rule 3 supersession without the predecessor-backed evidence and
attachment rules that justify it.

## Evaluation restrictions

The current client measures block confirmations, not complete transfers. Its
random workload is not seeded. The inherited nanospam configuration uses LMDB
`nosync_unsafe`; these measurements do not establish durable-signing or crash
recovery costs. A later durability comparison must use identical explicitly
chosen durability settings on both sides and report them.

The fixed 2,000-block/s offered load is not a saturation measurement. Each
attempt records its full latency histogram, actual completion count, binary
hashes and final RPC snapshots. Five pairs and the predeclared non-inferiority
margins provide a screening gate, not a guarantee for other workloads.

Failures of an intermediate revision remain in the artifact record. A failed
handoff is a reason to diagnose and fix the implementation, not to substitute
weak checkpoint finalization or exclude the run from the results.

## Saved previous work

The original dirty checkout was preserved separately at
`/Users/ruimorais/rsnano-node/.rai-task-backups/20260924T074150Z`.
The archive includes tracked diffs and all 4,056 nonignored untracked files;
its checksums and applicability to the base commit were verified. No code from
that checkout was implicitly included in the baseline or this branch.

Potentially reusable pieces include the existing dirty `attachment.rs`,
checkpoint storage/installation helpers, and benchmark diagnostics. They need
review against the revised eligibility and lock rules before adoption.

## Disk policy

The harness records free space and refuses another run below 8 GiB. Do not
delete existing user files without approval. If space becomes limiting, propose
cleanup in this order: redundant copied Git history created for this task;
superseded task build products; retained benchmark databases after preserving
required evidence; existing repository build caches only with explicit approval.
Preserve the dirty-tree backup and source/result manifests.
