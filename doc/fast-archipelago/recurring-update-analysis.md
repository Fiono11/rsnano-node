# Recurring updates, late incorporation, and convergence

4 October 2026. Investigation of the proposed periodic batching rule. No production algorithm, PDF, or implementation gate is changed by these experiments.

## Outcome

The user's objection to permanent freezing is correct: if every late request is incorporated at a later communication-round boundary, correct R maxima eventually reach the higher received value and correct A states record the conflict. The permanent-freeze explanation no longer applies.

However, this fact alone is insufficient for termination. First votes are immutable, historical certificates remain admissible, and a true B pair has priority over a larger false value. A finite batch period plus eventual incorporation does not imply that a rank ever starts with a clean request-before-response batch. The model below permits a repeating nondeciding partition despite eventual incorporation at every rank. It is **not** a counterexample to a synchronizer that eventually enforces the source's stronger clean-round condition.

## Semantics tested

- One state update per communication tick at each replica and phase/rank; late requests are buffered and processed at subsequent ticks.
- Before its first phase response, a correct replica has processed Q distinct phase requests, including its own.
- Replies are permitted repeatedly. In this model a reply is emitted only after processing its triggering request, not merely acknowledging receipt. This is stronger than the previous receipt-only experiment.
- Signatures are abstracted by identities. Each correct response is bound to a triggering request and a historical snapshot. Origin eligibility is Q processing witnesses per originating request; all required origins eventually have them. Current state does not revoke previous responses.
- R first votes are immutable; recovery permits a valid singleton different from the later R maximum. Each next rank uses preceding B evidence. No freshness filter, new retry-instance rule, or compulsory response replacement is added.
- All correct replicas remain awake. Every request is incorporated within at most three ticks per phase, with additional finite bounded delays available to deliver the selected responses first. All remaining evidence can be delivered before the next rank starts.

A tick here is a periodic batch boundary. It is not automatically a synchronized protocol round long enough to receive all awake-correct requests before the first response. A synchronizer that stretches rounds to enforce that condition excludes this schedule.

## Six-replica construction (v < w)

There are five correct replicas 1–5 and Byzantine replica 6. At each rank correct carry values are `v,v,v,w,w`. First R batches produce correct first votes `v,v,v,w,w`; Byzantine 6 can equivocate in its own first vote. All R states later update to w, but the correct first votes do not change. There is no fast certificate: even with Byzantine help v has four signatures, below F=5.

The snapshots `{1:v,2:v,3:v,4:w}` and `{1:v,4:w,5:w,6:w}` validly resolve recovery to v and w. The first remains valid after all current R maxima become w. Correct A requesters 1–3 use v; 4–5 use w. Byzantine 6 sends a justified A(v).

### A: update everyone, retain earlier signatures

At the first batch, responders 1–3 process `{1,2,3,6}` and sign singleton-v replies to requester 1. These, plus Byzantine 6's response, certify true-v for requester 1. Responders 4–5 process `{1,2,4,5}` and report a conflict.

At the next batch, **every correct replica incorporates every A request and has `{v,w}`**. Requesters 2–5 collect mixed responses to their own requests and return false-w. Requester 1's already valid true-v result remains valid. Thus B requests are:

    1: true-v
    2,3,4,5,6: false-w

The sole correct true requester is deliberately different from the previous permanently frozen construction. It permits Q false requests without violating the request-count barrier.

### B: incorporate the true request later

| Correct responders | First batch | Second batch | Third batch |
| --- | --- | --- | --- |
| 2,3,4 | Requests 2,3,4,6: only false-w | Also process request 5: still false-w | Process request 1: mixed true-v/false-w |
| 1,5 | Requests 1,2,4,5: mixed | Process remaining requests | Remain mixed |

Every reply used for a certificate follows processing of that specific request. In particular, the false-only replies to requester 5 are from the second batch, after request 5 was actually incorporated. All false origins have Q processing witnesses by that batch; true origin 1 obtains them by the third batch.

Requesters 4 and 5 can collect old false-only responses from 2,3,4,6 and adopt w. Requesters 1,2,3 collect eligible mixed responses and adopt v. Byzantine response delivery/relay can be delayed selectively for a finite time so that 2 and 3 do not first complete with the false-only quorum. The mixed responses initially lacking true-origin witnesses stay pending until that origin obtains Q witnesses.

After the third batch **all correct B states contain true-v and false-w**. No correct B snapshot at any batch is singleton true, so no B commit certificate can form in this execution. Current responses adopt v, while the older response certificate for w remains valid.

The correct carry partition is again `v,v,v,w,w`. Every next-rank introduction has actual preceding-rank B evidence for its value; both values remain legally available. A higher rank takes priority over lower-rank R state, so learning w at rank i does not alone forbid a certified v at rank i+1. The same schedule can therefore be reconstructed at the next fresh rank. The argument repeats inductively, while every rank's late requests are eventually incorporated.

This construction fails the *clean first-update* premise in every rank. Bounded delays and recurring updates alone allow it; an eventual clean-round synchronizer must rule it out. The construction does not establish nontermination when that extra premise is enforced.

## What clean batching can establish

A useful historical-evidence-safe sufficient condition is stronger than simply updating current state:

**First-response lower bound.** Fix rank i and value w. Suppose a set H of at least Q correct A responders has maximum at least w in its **first-ever signed A response at that rank**, and thereafter never decreases its maximum. Then every valid A certificate at rank i carries a value at least w, including certificates revealed later using historical snapshots. Any Q certificate intersects H; the intersecting correct response has maximum at least w. For a true certificate its singleton must therefore be at least w; otherwise the union maximum is at least w. By validated A-to-B ancestry, all valid B pairs and B results at that rank also carry values at least w.

This does not hold merely because those responders have maximum w *now*: earlier lower snapshots could already exist. It also does not prove that all future R introductions are at least w unless the fast predecessor branch is handled. Under this condition a lower fast certificate cannot coexist with the valid A input w: hidden-fast preservation would force all resolved recovery/A inputs to the lower value. A full application must establish these premises and nonempty justified response states, not assume them from a round counter.

If H incorporates all awake correct A requests before its first response, the largest such input supplies w. The remaining challenge is to establish sufficiently many such **fresh** clean ranks despite pre-GST first responses, rank advancement and historic certificates. Increasing a local batch period without coordinating ranks/steps is not itself that proof.

**Correction from the convergence investigation:** Q correct matching first votes already suffice; unanimity among all correct first voters is unnecessary. If H contains Q correct first voters for v, a snapshot S of m identities intersects H in at least m+Q-n=m-f-p identities, exactly the candidate threshold. Thus v is always a candidate, an empty recovery is impossible, and every resolved singleton is v. The earlier statement warning that Q matching correct votes were insufficient was incorrect. At P=n-f records candidate uniqueness ensures resolution to v. This applies to every valid snapshot, including earlier ones, because correct first votes are immutable. See `clean-convergence-analysis.md` for the proof and limits.

## Evidence and limits

Run:

    python3 tools/rai/check_recurring_updates.py

The script checks three successive constructed ranks for n=6,f=p=1 and n=9,f=2,p=1, reply/request binding and processing, Q request batches including self, actual recurring monotone updates, final incorporation of every request, origin witness availability, singleton recovery, absence of a possible fast certificate, B adoption results, absence of correct singleton-true B snapshots, and preservation of the carry partition. The document's repeatability argument is needed in addition to finite execution.

It also enumerates binary Byzantine vote assignments and all eligible signer subsets for the unanimous-correct-first-vote lemma. The counting proof above covers arbitrary Byzantine value names. Checked output is stored in `recurring-update-traces.json`.

This is a certificate-level model. It abstracts cryptography, application validity, the complete production recursive verifier, and the detailed network/synchronizer implementation. It is not an exhaustive model checker or a completed proof of safety or termination for a revised protocol. The source's stronger eventual-clean-round assumption has not been disproved. No code should reject old certificates or override immutable first votes based on this experiment alone.

## Recommendation

Continue with recurring monotone updates, but specify the eventual clean request-collection boundary and its interaction with first responses before treating it as a proof repair. Investigate fresh-rank progress using the first-response lower-bound lemma, rather than claiming that eventual learning of a higher value revokes older evidence. The open question is the synchronizer/ancestry progress argument, not whether late requests can be incorporated at all.
