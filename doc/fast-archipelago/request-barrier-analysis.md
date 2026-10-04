# Request barrier and immutable responses: bounded investigation

4 October 2026. This investigates a proposed rule; it does not change the production protocol or replace a termination proof.

## Result

Waiting for Q distinct valid requests alone does not force identical snapshots. Adding one immutable response per phase and rank can exclude the previous true/false example **if** origin eligibility is tied to those same responses: a correct response counts as a processing witness for an originating request only if the snapshot reflects processing it. A separate receipt acknowledgement issued after the snapshot was frozen would not establish this property.

The original Algorithm 5 waits for 2f+1 responses (lines 18–19, 34–35, 53–54); its handlers respond upon delivery (lines 41–48, 61–71). Its response reliability check requires 2f+1 responses for originating broadcasts (line 91). Page 8 separately describes synchronized request-before-response delivery. These are distinct conditions, not an explicit Q-request immutable-snapshot barrier in that pseudocode. For the adaptation Q=2f+p+1; Q=4 for f=p=1. The original 3f+1 is its population size.

## Intersection argument

Suppose an A-true(v) certificate has Q singleton-v signers. At least Q-f are correct. Under one immutable A response, each of those correct signers cannot also provide a processing witness for an A(w) request with w != v: it would have to reflect w and could not remain singleton-v.

Consequently any such w request has at most

    n - (Q - f) = 2f + p = Q - 1

possible processing witnesses, even allowing all Byzantine identities to equivocate. It cannot obtain Q origin-eligibility witnesses. A false certificate requiring such a conflicting value is therefore ineligible. This reasoning depends on the exact meaning of a witness, not merely the number of requests awaited. It also presumes the validated nonempty snapshot accurately represents conflicting inputs; a bounded A state cannot revert from mixed to singleton.

## Why the barrier alone is insufficient

With n=6, Q=4, identity 6 Byzantine, let A requesters 1,2,3,6 send v and requesters 4,5 send w. Responders 1,2,3 can first receive {1,2,3,6}, while responders 4,5 first receive {1,2,4,5}. Identity 6 can sign singleton-v. Every correct responder processes four distinct requests and responds once. Responses 1,2,3,6 evaluate to true-v; responses 1,2,4,5 evaluate to false-w.

However, the w origins have only two correct processing witnesses, plus at most one Byzantine witness: three, below Q. This is a counterexample to *identical snapshots from a count barrier*, NOT a valid full-certificate execution under the additional snapshot-bound eligibility rule. Valid recovery for the A request values is a premise of this limited example, not independently verified here.

## Remaining work

- Specify whether the sole response acknowledges each processed request or whether receipt witnesses are separate. Do not conflate delivery with processing reflected in the snapshot.
- Specify handling of requests arriving after the sole response. Refusing all further processing witnesses may prevent origins from becoming eligible; issuing new mutable snapshots abandons the proposed immutable-response rule.
- Prove that enough eligible responses can be obtained under Byzantine omissions and rotating suspensions, including mixed first snapshots and pre-GST state. The intersection argument proves exclusion, not progress.
- Reconcile per-phase/rank immutability with the source's per-broadcast response binding and the implementation's W admission / Q eligibility distinction.
- Investigate the full R/recovery/A/B execution and cross-rank behavior. No full liveness claim follows from this bounded check.

## Reproduction

Run `python3 tools/rai/check_request_barrier.py`. It checks every pair of Q identity sets with a fixed maximal Byzantine set (identities are symmetric): 225 pairs for n=6,f=p=1 and 7,056 for n=9,f=2,p=1. It also checks the barrier-only example and that its w origins lack eligibility. The script does not model cryptographic verification, complete ancestry, scheduling, or termination.

The earlier mutable-response trace remains a finding about the existing response-on-delivery interpretation. It is not evidence against the strengthened immutable-response rule analyzed here.

## Termination check: strict immutable snapshots can deadlock

The additional check `python3 tools/rai/check_request_barrier_termination.py` constructs closed A-phase deadlocks for both configurations. Its exact model binds each sole response to the requests processed before signing. Later delivery cannot extend that signed origin set, and origin eligibility requires Q distinct such processing witnesses **per request**, not per value. There is no independent receipt-acknowledgement, retry-instance, or rank-skip rule. These qualifications are essential.

For n=6, correct replicas are 1–5; Byzantine replica 6 withholds all A responses. Valid singleton recoveries for v and w are possible from immutable first votes `1:v, 2:v, 3:w, 4:w, 5:u`: snapshots `{1,2,4,5}` and `{2,3,4,5}` respectively select v and w at support two. There is no fast certificate. Replicas 1–3 submit A(v); replicas 4–5 submit A(w), each with its corresponding recovery evidence.

| Responders | First four A request origins | Sole response |
| --- | --- | --- |
| 1, 2, 4 | 1, 2, 3, 4 | {v,w} |
| 3, 5 | 1, 2, 3, 5 | {v,w} |

Every responder counts its own request and four distinct correct requesters. Origins 1–3 each have five witnesses. Origin 4 has three; origin 5 has two. Neither w origin reaches Q=4. Every response contains maximum w (take v<w), so every response requires a w origin that cannot become eligible. Even allowing alternative provenance from either w request cannot help. No valid Q-response A certificate can form; no correct B request or next-rank predecessor can follow.

Deliver all remaining requests and relay all existing signed records to all correct replicas. The signed origin sets stay fixed, so the witness counts remain unchanged. A finite pre-GST prefix can therefore leave the phase blocked after the network stabilizes, with all correct replicas awake forever. This meets the suspension upper bound and recurrence requirement. A clean-round reset or fresh attempt could change this conclusion only with a specified mechanism compatible with vote locks and ancestry; one cannot assume such a mechanism in the single-response rule.

The script checks recovery arithmetic, distinct request counts, self-inclusion, frozen snapshots, witness eligibility, absence of a fast certificate from the supplied votes, and closure under eventual delivery and retransmission. It abstracts signatures, rank-zero application validity, and the R-prefix producing these first votes; it is a phase-level counterexample from admissible recovery inputs, not a complete implementation/network model. A rule preventing those inputs or safely restarting the phase would require separate analysis. The checked traces are in `request-barrier-deadlock.json`.

Allowing late processing acknowledgements or reusable snapshot responses bound to newly received requests invalidates this particular deadlock argument, but also requires revisiting the earlier exclusion proof and defining exactly what a witness certifies. The original mutable response-on-delivery service does not have the frozen-witness restriction tested here. No production protocol or PDF was changed by this investigation.
