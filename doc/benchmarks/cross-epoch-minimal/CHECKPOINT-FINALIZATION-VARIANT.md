# Protocol variant: checkpoint finalization of a unique preserved branch

Status: requested experimental variant, 2026-09-24. It **deliberately departs**
from RAI.pdf §5.4/§5.5 ("The checkpoint does not resolve a branch merely
because it is the sole survivor of the selected evidence"; "No sole-survivor
round creates finality"). The paper's safety proof (Theorem 7.4 and its
lemmas) does **not** cover this variant. Nothing below claims it does.

The certificate-only behaviour remains the default and the control:
`checkpoint_finalization = "certificate_only"`. The variant is
`checkpoint_finalization = "unique_branch"` (`[node.active_elections]` in
the node TOML; `--checkpoint-finalization unique_branch` in nanospam).

## The rule (exact)

Let `BuildState(S_{e-1}, Q)` run Algorithm 2 steps 1–5 unchanged: predecessor
verification, selected-report reconstruction, support accounting (Rules 1–2),
inherited-lock preservation, Rule 3 supersession, and pruning of every branch
excluded by explicit finality. Call the resulting retained (non-final) set the
*preserved branches*. Then, only under the variant and only after those steps:

For each account `a`, walk the account positions `v` upward from the last
finalized position `d_a^F` + 1. At position `v`, let `P_v` be the set of
preserved blocks at `(a, v)`.

* If `|P_v| = 1` and every preserved block at `(a, v)` has its parent equal to
  the block finalized (already, or by this rule) at `(a, v-1)`, the sole
  block `B_v` is **checkpoint-finalized**: `S_e` marks `(a, v) ↦ B_v` final
  with origin `Derived`, and the walk continues at `v+1`.
* Otherwise (`|P_v| ≠ 1`, or the only block does not attach to the finalized
  prefix) the walk stops for this account; nothing at `v` or above is
  finalized by this rule.

Consequences of the definition:

* A branch consisting of one block is finalized when it is the only preserved
  block at its position. It is **not** finalized when a rival is preserved at
  the same position, whatever the lock kinds are.
* Uniqueness is decided **only** among preserved branches, after conflict
  pruning. A branch omitted from `S_e` because it fell below the Rule 2
  threshold does not count as a rival. This is the point at which the rule
  infers from absence within the selected evidence (see "Unsafety" below); it
  never infers from *missing* evidence: an unusable report, an incomplete
  page transfer or one node's partial view cannot make a report selectable,
  and `BuildState` is a function of `(S_{e-1}, Q)` alone.
* "Selected contiguous prefix" means the parent-closed path from the finalized
  frontier; a preserved block whose parent position holds two preserved
  rivals is never finalized, because the walk stopped below it.

## Interactions

* **Recovery-only locks (Rule 2).** A unique Rule-2 branch is finalized. This
  is the unsafe case (below).
* **Represented notarization locks (Rule 1).** A unique Rule-1 branch is
  finalized. The tip is safe against closing-epoch rivals (Lemma 7.1), but
  its prefix may contain inherited recovery-only ancestors, which is again
  the unsafe case.
* **Inherited locks.** Inherited unresolved forks are preserved first
  (Algorithm 2 step 5); if two inherited branches survive at a position the
  walk stops there. A single inherited R branch at the frontier is finalized.
* **Rule 3.** Supersession runs before uniqueness. A predecessor-backed NC
  that displaces an inherited recovery lock leaves one preserved branch, which
  the variant then finalizes.
* **Late certificates.** A certificate assembled after the checkpoint for the
  finalized block or a selected descendant is compatible and has no further
  effect. A certificate for a *conflicting* block at a `Derived` position is
  a safety violation; installation rejects it and logs
  `CHECKPOINT_DERIVED_CONFLICT` (the ledger keeps the certificate-backed
  block, the state hash of the node diverges, and the run's settlement check
  fails). Such a certificate is exactly what the counterexample produces.
* **Application replay.** `Derived` positions are replayed exactly like
  certificate-backed ones (once, in dependency order) and derive committees.
  Installation never re-applies an operation already applied live.
* **Distinguishability.** `EpochLedger` records the origin per finalized
  position (`Certificate` or `Derived`); the origin is part of the versioned
  state commitment and of the checkpoint wire records (status 4 =
  derived-final). `epoch_locks`/`final_state` report both counts; the fork
  diagnostic classifies inclusion as `Finalized` or `FinalizedDerived`.

## Proof obligations (what would have to be shown, and is not)

1. *No conflicting certificate-backed finality.* For every position the
   variant finalizes, no valid FC/FF for a different block at that position
   exists or can ever be formed, in any epoch.
2. *No conflicting checkpoint finality.* Two valid checkpoints for one context
   agree (inherited from CheckpointAgree, unchanged).
3. *Compatibility with overlap.* Successor-epoch work that settled through the
   §4.3 exceptions before `S_e` was known never conflicts with a `Derived`
   position.

Obligation 1 holds for a unique Rule-1 tip against *closing-epoch* rivals
(Lemma 7.1) and, once `S_e` is installed, against later epochs (the
attachment rule closes the position). It fails for successor-epoch work that
runs while `S_e` is unknown; that is obligation 3, which the counterexample
refutes.

## Unsafety: a concrete counterexample

Parameters `f = p = 1`, `N = 6`, `q = 4`, `r = 3`, members A–E correct, Z
Byzantine. Account `a`, position `v`, parent finalized in `S_{e-2}` and hence
in `S_{e-1}`. Two owner-signed conflicting blocks X and Y at `(a, v)`.

1. Epoch `e`: A, B and Z first-vote X; C, D and E first-vote Y (the owner
   equivocated). No NC forms for either (3 < q). All six freeze reports; the
   leader selects A, B, C, D, Z. X ∈ G_A, G_B, G_Z (3 ≥ r) → Rule 2 preserves
   X. Y ∈ G_C, G_D (2 < r) → Y is omitted. X is the unique preserved branch at
   `(a, v)`; the variant finalizes X in `S_e`.
2. Epoch `e+1` opens while `S_e` is unknown (lagged committees, §5.1). The
   cross-epoch lock (§4.2) binds A and B to X at `(a, v)`. C, D, E and Z
   first-vote Y in epoch `e+1` (Z equivocates across epochs; C, D, E vote
   for their own closing-epoch choice, which the lock permits). That is 4 ≥ q
   members of `K_e`, so Y holds a *predecessor-backed* epoch-(e+1) NC. Its
   parent is finalized in `S_{e-1}`, so by the second overlap exception
   (§4.3) Y is eligible before `S_e` is known. C, D, E and Z final-vote Y:
   an FC for Y finalizes Y at `(a, v)` in epoch `e+1`.
3. `S_e` (variant) finalizes X at `(a, v)`; epoch `e+1` finalizes Y at
   `(a, v)`. Conflicting finality.

Under the paper's rule `S_e` carries X only as a recovery lock, and the
predecessor-backed NC for Y supersedes it (§4.3, §6.2), so there is no
conflict. The regression test
`unique_branch_variant_conflicts_with_a_predecessor_backed_successor_certificate`
in `epoch_ledger.rs` builds exactly this state and asserts (a) the variant
finalizes X, (b) the eligibility logic admits Y's predecessor-backed NC while
`S_e` is unknown, (c) the two finalities conflict, and (d) the certificate-only
control carries a recovery lock instead, which Rule 3 removes.

The conflict needs only one Byzantine identity and the paper's own overlap
exception. Restricting the variant to branches whose *entire* selected prefix
holds represented (Rule 1) locks would close this counterexample (a
represented NC binds ≥ q − f correct members at the position, leaving at most
2f + p < q first voters for any rival in epoch e+1), but that restricted rule
was not requested and is not implemented; it is noted as the nearest safe
sub-rule.

## Benchmark reporting

Results obtained under `unique_branch` are labelled as such. They measure a
different, weaker finality semantics than the certificate-only control and
than the paper; equality of confirmation counts between the two settings is
not evidence that performance is preserved for equivalent outcomes.
