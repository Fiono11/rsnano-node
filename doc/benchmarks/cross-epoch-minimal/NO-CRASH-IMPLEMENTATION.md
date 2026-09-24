# No-crash RAI implementation boundary (2026-09-24, second pass)

Branch `rai-cross-epoch-minimal`, continuing from `b202576c8`. Baseline stays
`5e037cfe0527d7b06b573456c7c876568ce179ea`. Protocol reference: the
2026-09-24 R/N/F PDF (SHA256 `4d853d67…`, kept at
`/tmp/rai-cross-epoch-artifacts/protocol/RAI-2026-09-24-RNF.pdf`); the
original checkout's `RAI.pdf` was the older revision.

Scope assumption: **nodes do not crash during nanospam**. Every logical
signing, evidence-retention and terminal-state requirement is implemented
in memory. Disk persistence, fsync and restart recovery are deferred; no
crash-durability claim is made, and evaluation is labelled "no-crash
performance with volatile vote records".

## Mechanisms, by commit

| Commit | Mechanism | Paper section |
|---|---|---|
| `3245f30a8` | Variant specification and counterexample | — (variant) |
| `80c710b7f` | T reconstruction by a sketched authenticated difference when no root is shared; reproduction of the observed stall | §5.3 |
| `85c076aac` | N/F entries verified from retained signed votes (NC/FC/FF assembled locally, predecessor finality/locks, selected-descendant prefix); evidence request relays original signed batches | §5.3, §5.5 |
| `803d4157c` | Cross-epoch first-vote lock on the per-epoch first-vote record; checked at listing and at recording | §4.2 |
| `b607acd48` | Attachment on complete current-epoch parents; both overlap exceptions; provisional recheck on predecessor arrival; Rule 3 with predecessor backing from retained first votes; explicit `BuildRules` | §4.3, §5.5, §6.2 |
| `fb09aee67` | Report bound to the committee digest; exact `G = V \ keys(T)`; R against the verified predecessor; own first vote for every G hash | §5.2, §5.3 |
| `79ece3fbc` | Equal-weight committee model, explicit `q, r, N−p, N−f`; closing committee decides alone, successor verifies and installs | §3.1, §3.2, §5.1 |
| `93ab52b95` | Unique-branch variant behind `checkpoint_finalization`; `Derived` origin in state, wire, RPC and fork classifier; install-time conflict detection | variant |

## What each binary contains

* **Baseline** (`bin/baseline/rsnano`, frozen HEAD): weighted joint
  agreement, certificate-free checkpoint finalization of sole survivors,
  root-only T reconstruction, no N/F evidence verification, no cross-epoch
  lock, no overlap exceptions. Its confirmations are not equivalent to the
  candidate's.
* **Candidate** (this branch): all mechanisms above. Run-time switches:
  `checkpoint_finalization = certificate_only | unique_branch`,
  `committee_model = weighted | equal_weight` with `committee_f`,
  `committee_p`. The nanospam client passes them to every node
  (`--checkpoint-finalization`, `--committee-model`, `--committee-f`,
  `--committee-p`).

## Where the implementation still departs from the PDF

* **Rule 3 determinism.** Predecessor backing is read off the signed first
  votes a validator retains, not off the selected reports. Two validators
  with different retained vote sets can disagree on a superseding value
  until the finite epoch vote set reaches both (evidence relay requests it).
  The paper's BuildState is a function of `(S_{e-1}, Q)` alone; this is the
  one place the implementation adds a private input, and a proposal that
  rests on it is refused, not accepted, by a validator lacking the votes.
* **Late finality window.** F entries are justified by certificates of the
  closing epoch and the epoch before it; older late certificates are not
  searched (vote records are kept for four epochs).
* **Evidence relay is best effort.** Missing votes are asked for at most
  once a second per epoch and 1,024 hashes at a time; nothing is inferred
  from a missing reply.
* **Checkpoint page transfer** (`CheckpointReq/Reply`) still exists but
  never installs a state: installation always reconstructs the selected
  reports and recomputes BuildState.
* **Provisional recheck** discards only instances that conflict with the
  decided finality, reopen a retained position, or continue a branch the
  checkpoint excluded at the parent position; an instance whose parent the
  checkpoint does not mention is left to the ordinary eligibility rules.
* **Equal-weight model** takes the `3f + 2p + 1` largest holders of
  delegated weight as members (ties by identity). The first diagnostic
  attempt used "every nonzero holder", which admitted the genesis funding
  representative as a seventh member and required 6 of 7 reports; that
  attempt is preserved as `no-crash-fork-paper-v1` and superseded. A
  member count other than `3f + 2p + 1` is still logged.
* **Two protocol revisions.** The branch follows the 2026-09-24 R/N/F PDF.
  The later EuroSys manuscript narrows the overlap exception (closing-epoch
  and current-epoch NC on every unresolved block of the finalized prefix,
  no predecessor-backed route in the main theorem, matching-origin lock
  discharge) and adds a promotion rule for uniquely retained notarized
  prefixes. The promotion rule is available as
  `checkpoint_finalization = notarized_unique_prefix`; the narrower overlap
  rule is not implemented.
* **No durability**, as stated above. Retained signed votes, blocks and
  observations are bounded by the four-epoch retention window; nothing
  still needed for a closure within that window is evicted, and nothing
  older is served.
* Application replay of finalized operations is the ledger's cementing;
  balances and receive-once are the existing ledger rules, not a separate
  BuildState replay.

## Tests added in this pass

`reports::` (sketch reconstruction, paging and growth, stale-snapshot and
tampered-page refusal, evidence gating, committee/partition membership),
`reports::ledger_evidence` (inherited justification, N/F certificates, late
finality, selected-prefix walk), `vote_records` (support index),
`active_elections_container` (cross-epoch lock, complete-parent attachment,
both overlap exceptions with controls, provisional recheck, the variant
counterexample), `epoch_ledger` (Rule 3 supersession and pruning, the
variant's sole-branch rule and origin), `committee`/`epoch_committees`
(paper thresholds, equal members, closing-committee-only decision),
`messages` (evidence request, ledger sketch wire).

## Predeclared evaluation (before any run)

All runs: six nodes on one host, 45,000 primary blocks over 45,000 accounts
at 2,000 blocks/s, 8-second epochs, the one shared instrumented client built
from this branch, LMDB `nosync_unsafe` as inherited. Deadlines are the
existing baseline-derived 156-second ceiling. Every attempt is preserved,
including failures; generated databases are deleted after evidence capture
and process-group shutdown. Free disk is checked before every build and run
and nothing starts below 8 GiB.

1. **Correctness diagnostic, paper model.** One bounded 5 % fork diagnostic
   of the candidate with `--committee-model=equal_weight --committee-f=1
   --committee-p=1 --checkpoint-finalization=certificate_only`. Success is
   every recorded branch included or discarded by a certificate-backed
   witness on all six nodes with a common installed checkpoint. This is the
   prerequisite for any matched comparison; it is not a performance result.
2. **Variant diagnostic.** The same diagnostic with
   `--checkpoint-finalization=unique_branch`, reported apart: inclusion and
   discard witnesses by origin, `finalized_derived` counts and any
   `CHECKPOINT_FINALITY_CONFLICT`.
3. **Matched zero-fork comparison.** Five alternating pairs, frozen baseline
   vs candidate (paper model, certificate-only), gate `p50-p95-v1` (goodput
   ratio ≥ 0.95; p50 and p95 ratios ≤ 1.10; p99 diagnostic). Reported with
   completion counts, unresolved work, checkpoint progress and recovery-child
   overhead. A pass is bounded evidence for this workload only.
4. **Matched 5 % fork comparison.** One baseline calibration with the
   repaired client (the earlier calibration failed with the pre-repair
   client, and remains recorded), then the candidate under the same
   deadline; termination reported apart from confirmation latency and from
   certificate vs derived finality. If the baseline calibration fails again
   it is recorded as such and no candidate run is relabelled a pass.

Percentiles over completed subsets are conditional and labelled so. The
earlier five-pair gate (`step1-reconciliation-refresh-p50-p95`) stays
inconclusive. Changing finalization semantics (item 2) is never counted as
performance preservation for equivalent outcomes.
