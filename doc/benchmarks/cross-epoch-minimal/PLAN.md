# Minimal implementation plan — revised R/N/F reports

Authoritative protocol: `RAI.pdf`, supplied 2026-09-24, SHA256
`4d853d675a2efff31a9903c9f6ea8af0bade2c37fa909eb6148e8102d190d021`.
This supersedes the earlier DOCX as the implementation target. In particular,
PDF §§5.2–5.5 place inherited recovery-protected blocks in the frozen report
ledger set `T_i` with tag R. Baseline remains `rai_kudzu` commit
`5e037cfe0527d7b06b573456c7c876568ce179ea`; never rebuild it from the dirty tree.

## Required semantic change

* `T_i = {(hash(B), status_i(B)) : B in L_i}` uses R, N, F with strength
  `F > N > R`. Status is committed and immutable after report freeze.
* R means inherited recovery protection from the **verified predecessor**,
  without an NC or finality proof. It is not a fresh first vote, an NC, or
  finality, regardless of how many reporters include it.
* `G_i = V_i^(e) \\ keys(T_i)`: exclude a hash present under **any** T tag.
  G commits hashes, not vote-kind/placement tuples. Retained signed votes
  separately justify membership; only a reporter's own verified first vote
  contributes to Rule 2. Received votes from others do not contribute for it.
* The live reconstruction projection includes relevant inherited ledger data
  with current justified tags, excludes successor work, and has canonical
  block-hash ordering. A frozen report keeps its original tags even when the
  live view subsequently upgrades R → N → F.
* Usability requires predecessor membership for R, signed-vote NC evidence
  for N, explicit FC/FF evidence for F (possibly a selected descendant), and
  the reporter's signed votes for G. Root equality alone proves none of this.
* BuildState starts from the predecessor. R entries authenticate carried
  protection, never add fresh recovery support. Rule 3 may remove a recovery
  lock only with the required predecessor-backed conflicting NC. Adding R
  does not remove the paper's fresh-child recovery mechanism (§4.4).

## Stages, dependencies, and evaluation gates

0. **Freeze baseline and preserve work.** Already done: dirty-tree backup,
   clean branch, pinned binaries, shared measurement client and paired runner.
   Preserve the old DOCX provenance and all failed/interrupted attempts.

1. **Certificate-only finality and owner recovery.** Implemented as an
   intermediate revision; completion/performance evaluation remains a gate.
   Keep explicit checkpoint lock kinds, structural ancestry checks, genesis
   history, and maximum-depth fresh-child recovery. This does not establish
   full revised-protocol safety. Existing runs are pre-R-report experiments.

1b. **R/N/F report ledger and exact T/G partition — new immediate stage.**
   Before thresholds or signing changes, extend the report model, freeze path,
   report root/difference encoding, reconstruction, and BuildState together.
   Seed T from the verified predecessor's represented ledger, then merge
   justified closing-epoch observations with F > N > R. Do not merely append
   R entries to the existing per-epoch certified-election inventory.
   R validation and exact key subtraction are required in this stage.
   Preserve frozen snapshots and signed context bindings. Explicitly version
   the changed report commitment/wire format; reject unknown tags/versions.
   Keep report status distinct from checkpoint storage tags (currently 0 is
   checkpoint ancestry, 1 final, 2 notarized, 3 recovery). Ancestor-only
   storage records do not automatically supply an NC or a new report status;
   derive their report representation from justified selected-path evidence.

   Minimal implementation touchpoints:
   - `election/certified_state.rs`: explicit R/N/F report status, strength,
     canonical commitment and frozen snapshot. Prefer a report-ledger type
     name; recovery is not certification. Audit `is_finalized`: currently it
     treats anything other than Notarized as final, which would misclassify R.
   - `active_elections/{epoch_states,active_elections_container}.rs`: merge
     predecessor ledger with relevant evidence for both freeze and live
     epoch projection; do not rely on surviving active elections.
   - `election/residual_votes.rs`, `active_elections/vote_records.rs`: exact
     hash-set G, separate signed evidence and first-vote support accounting.
   - `messages/src/report.rs`, `consensus/reports/*`: R-aware report decoding,
     authenticated canonical differences, immutable snapshots, predecessor
     context checks and reconstruction. Do not reuse checkpoint tag meanings.
   - `election/epoch_ledger.rs`: exhaustive R/N/F handling. Its current
     non-final branch assumes N; R must never enter that branch as an NC.

   Tests before measurement: R survives multiple closes without fresh votes;
   R excludes the same hash from G; many R reporters cannot reach r; forged R
   absent from the predecessor or bound to another predecessor is rejected;
   R/N/F upgrades change live roots but not frozen roots; late votes cannot
   change frozen G; N/F require their own evidence; duplicate hashes/reporters
   cannot add support; successor work cannot alter the closing projection;
   reconstruction reproduces mixed-tag snapshots and rejects changed tags;
   inherited lock omission and Rule 3 eligibility remain deterministic.

2. **Equal-weight committees and old-committee-only agreement.** Explicit
   N=3f+2p+1 parameters and integer q, fast, recovery and report thresholds.
   The successor verifies and installs rather than joining the old committee's
   decision. Compare this revision to HEAD and to the accepted stage 1b.

3. **Durable signing and cross-epoch first-vote exclusion.** Persist per-identity
   decisions and terminal state before signatures escape; retain signed
   evidence across election eviction/restart. Compare matched durability
   configurations; inherited `nosync_unsafe` runs cannot support durability claims.

4. **Complete eligibility and provisional recheck.** Validate selected ancestry,
   complete current-epoch parents, finalized receive dependencies, both overlap
   exceptions, and Rule 3. R must not block an eligible superseding NC forever,
   nor permit unsupported displacement. Keep checkpoint and live finality compatible.

5. **Full signed-evidence usability and independent successor installation.**
   Finish verifiable own-vote retention/gossip and NC/FC/FF reconstruction,
   independently rebuild selected reports and BuildState, validate all context
   bindings and perform exactly-once installation. R's predecessor check from
   stage 1b remains required; it never replaces N/F proof checks. No full-protocol
   claim until this and the preceding stages are complete.

6. **Paper evaluation.** Fault-free, forks, delayed/withheld votes/reports,
   actual committee replacement, restart, repeated carried R entries, latent
   finality exposure, and growing retained history. Compare every accepted
   stage with frozen HEAD using the same client/configuration; additionally
   compare adjacent stages to attribute cost. No advancement after correctness
   failure or unexplained performance regression/inconclusive gate.

## Additional measurements for the R change

Preserve the offered-load and paired-order controls. Record R/N/F counts,
T/G cardinalities, inherited/repeated R fraction, frozen/live root work,
report/reconstruction bytes, difference sizes, reconstruction and closure
latency, retained signed-evidence bytes, peak memory and disk growth. Vary
carried R history separately from newly admitted operations; a no-recovery
workload is only a control, not the evaluation of this change.

Primary goodput and confirmation p50/p95/p99 include recovery waiting; report
fresh-child overhead separately. Five-pair 95% bootstrap screening uses the
existing margins (goodput ratio at least .95; p99 at most 1.10). A pass is
bounded evidence for the tested workload, not a universal performance guarantee.
Baseline's weak checkpoint confirmations are not automatically equivalent to
certificate-backed confirmations; separate that semantic difference in the paper.
Keep every attempted run, even if a later protocol revision supersedes it.

Disk policy remains: check available space before each attempt, stop below
8 GiB, and ask before deleting. Proposed cleanup priority: redundant task Git
history, superseded task build outputs, task benchmark databases after retaining
required evidence, then pre-existing caches. Preserve dirty-tree backup and
source/result manifests.
