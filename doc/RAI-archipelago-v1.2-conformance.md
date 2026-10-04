# Fast Archipelago v1.2: unresolved certificate-persistence claim

4 October 2026 · Phase 1, commit 8 conformance work

Disk persistence is deferred as requested. The issue below is independent of storage: all state and signed evidence are retained throughout one execution.

## Finding and scope

Version 1.2 §3.1 permits a rank i+1 R introduction carrying the value recomputed from a valid rank i B certificate. Lemma 6.5 (§6.3) says that after a B certificate carries a persistent true value v, every legal higher-rank introduction descending from that rank preserves v. The stated certificate rules do not explain why an earlier valid B adoption certificate for w becomes unusable.

The executable trace below demonstrates that gap for **adoption certificates**, using legal singleton recoveries, mutable A/B response state, signed snapshots, and sufficient W/Q witness counts. It does **not** demonstrate two conflicting decisions, and is not a complete network simulation or a full recursive-certificate verifier. Rank-zero application validity and proposal ancestry use the explicit test fixture from commit 7. The A/B request relationships are checked through their recovery/certificate evaluations and origin witness sets, rather than a completed wire protocol.

Source: [Fast_Archipelago.pdf](../Fast_Archipelago.pdf), version 1.2, SHA-256 `56822aca1f4d72a7de594c315f0bf3ca27e105d47d22e324fbf954f973d800d5`; compare §3.1, §3.4, Lemma 6.5 and §8's persistence/elimination argument.

## Reproducer

```sh
cargo test -p rsnano_node --features rai_protocol --lib \
  earlier_b_adoption_certificate_survives_a_later_true_certificate -- --nocapture
```

The test passes by asserting that both historical certificates remain verifiable under the stated evaluations. Its deterministic signed snapshots are saved in [persistence-trace.json](benchmarks/rai-phase1-conformance-2026-10-04/persistence-trace.json).

Use n=6, f=p=1, Q=4, W=2. Let 4 be Byzantine, and let correct first votes be `1:v, 2:v, 3:w, 5:w, 6:u`, with v<w<u. There is no fast certificate. Both recovery snapshots are legal at m=4, where support is two:

- `{1:v, 2:v, 3:w, 6:u}` resolves to v.
- `{1:v, 3:w, 5:w, 6:u}` resolves to w.

The following responses come from the four correct identities 1, 2, 3 and 5. Replica 6 can invoke A(v) and delay delivery of its B(true,v) request to those responders. Replica 4 can invoke A(w) and B(false,w) with the valid certificates shown below. Responders serve valid requests even when their own proposer is at another step.

| Step | Delivered request | Each correct response | Certificate result |
| --- | --- | --- | --- |
| 1 | A(v), with its resolved recovery | `{v}` | A returns `(true,v)` |
| 2 | A(w), with its resolved recovery | `{v,w}` | A returns `(false,w)` |
| 3 | B(false,w), justified by step 2 | `{(false,w)}` | Old B certificate adopts w |
| 4 | Delayed B(true,v), justified by step 1 | `{(false,w),(true,v)}` | New B certificate adopts v |
| 5 | Re-evaluate the saved step-3 certificate | Its original signed responses are unchanged | It still adopts w |

Every origin used by these certificates has Q distinct signed responses, which also meets W. No correct responder loses its learned true value or decreases its retained maximum. Mutable local state does not rewrite old signatures. Both B results are `adopt`, not `commit`.

Under §3.1's stated check, the old B certificate remains a predecessor for R(i+1,w), despite the newer B certificate carrying true v. A verifier checking complete ancestry, signatures, W/Q witnesses, and the B evaluation needs an additional reason to reject it; those checks alone do not supply one.

## Required clarification before service wiring

Determine whether “persistent true value” has an additional certificate condition not expressed by this trace, whether predecessor admissibility has a missing rule, or whether Lemma 6.5 should be restricted to committed decisions and the convergence argument revised accordingly. A weaker persistence statement may be sufficient, but that must be reconciled with the stated clean-rank progress argument.

Do not silently add freshness deadlines, phase seals, quorum locks, or rejection based solely on a receiver's current state: those would change the protocol or its portable certificate-verification semantics. The plan's conformance and simulator exit criteria before commit 10 are not yet satisfied.

## Implementation status

Independent work completed while checking the claim: a volatile first-vote journal; bounded mutable A/B state; distinct-responder A/B evaluation; and separate W admission/Q eligibility counters. These are pure components. The full R/A/B driver, recursive verifier, synchronizer, network service and Gate B remain incomplete. This record does not mark commit 8 or Phase 1 complete.
