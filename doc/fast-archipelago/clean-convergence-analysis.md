# Convergence to either value: clean rounds and immutable first votes

> **Later finding (4 October 2026):** [A clean-round nontermination construction](clean-nontermination-proof.md) now covers the documented recovery and historical-certificate rules with every correct replica active. It uses the recovery gate to return below the common R maximum and private Byzantine certificates. This supersedes the earlier “no assumption-respecting counterexample established” status below; see the construction's explicit rule scope and executable checker.

4 October 2026. This investigation corrects an overly strong requirement in the preceding recurring-update analysis and isolates a sufficient condition for decision. No protocol rule or PDF has been changed.

## Main finding

The objective is agreement and decision on **any** valid value, not adoption of the numerically largest value. A clean B round containing a correct true-v request favors v even if a larger false-w value is present. Earlier experiments that keep the split forever without ever supplying clean first-response rounds do not refute termination under the source synchronizer assumption.

A particularly useful sufficient condition is **Q correct immutable first votes for the same value at one rank**. This alone forces recovery and the slow path to that value. It is weaker than the all-correct unanimity condition previously discussed and needs no certificate-expiry rule.

## Lemma: a correct first-vote quorum fixes recovery

Let n=3f+2p+1 and Q=n-f-p. At a fixed instance/rank, suppose H is a set of Q correct identities whose immutable first votes are v. For any valid first-vote snapshot S of m>=Q distinct identities,

    |S intersect H| >= m + Q - n = m - f - p.

Every intersecting record must vote v because H contains only correct immutable first voters. Therefore v is in Cand(S). No empty recovery is valid; every resolved singleton recovery is v. A different fast certificate is impossible because it would intersect H and require a correct identity to change its vote. At P=n-f reports, candidate uniqueness ensures the snapshot resolves to v.

This argument covers records and recovery certificates assembled **before or after** the quorum is observed; it is a statement about immutable records, not a freshness assumption. Older conflicting first votes outside H cannot authorize a different recovery result. They may produce ambiguity and require more evidence, but cannot dislodge v.

The premise is about Q **correct** matching first voters. An arbitrary certificate with Q matching signatures can contain Byzantine identities and does not establish it. The protocol's publicly verifiable fast decision threshold remains F=n-p. No new decision rule is introduced.

### Six-replica example

Suppose four correct replicas first-vote v. The fifth correct replica and Byzantine replica may vote w. A four-record snapshot can contain `v,v,w,w`, requiring a wait. It cannot resolve to w. At five records the worst case is `v,v,v,w,w`, which resolves to v. All valid A inputs then carry v, regardless of which old w predecessor certificates exist at the previous rank.

## Conditional progress to a decision

Under full recovery/ancestry validation, the lemma excludes every A input other than v at that rank, including earlier inputs: a valid recovery for another value cannot coexist with H. Hence valid A response evidence contains only v and every A certificate returns true-v. Valid B inputs then contain only true-v. Q awake correct B responses can form a commit certificate for v.

This slow-path argument does not require Byzantine cooperation or F matching signatures. Progress still assumes eventual resolution and availability of first-vote/ancestry/witness evidence, recurring correct service, and scheduling of the A/B steps. If a delayed required correct first vote was never emitted, the request/relay service must eventually elicit it; relay alone cannot create a missing record.

The proof does not assume v is the maximum value proposed in the instance. It can be the lower value in the previous examples.

## What a clean B round proves

Suppose an awake correct requester sends B(true,v), and a clean B round delivers it to a set H of at least Q correct responders before their round responses. Those responses contain true-v, and retained true evidence prevents later responses from losing it. Any Q-response certificate consisting of responses at or after that boundary intersects H and therefore adopts or commits v (true-value uniqueness excludes another true result).

So the intended fresh-response behavior **does converge to v**. The old adoption trace is not evidence that fresh clean B responses remain split.

There are two distinct extensions:

1. If these are H's **first-ever** B responses at the rank, every historical/future B certificate intersects a true-v response as well. Every B predecessor carries v. Any fast certificate at this rank must also carry v, since a different fast value would forbid the valid A-true(v) certificate. All next-rank legal introductions carry v. With service progress, the next rank has only v and decides. This is a complete conditional true-branch argument covering old evidence.
2. If H signed earlier false-only responses, the earlier conflicting adoption certificate can remain valid. That fact does not itself prove nontermination. One must show how the synchronizer and future R/recovery steps eventually establish a correct first-vote quorum or another well-founded progress condition. Global invalidation of all old certificates is a sufficient strategy, not a necessary condition for termination.

Correctly counted B responses must actually meet the clean-round timing premise. Merely naming an old response as part of the current round does not make it a response generated after the boundary.

## Exact remaining question

Does the actual synchronizer, request-validation discipline and R rank-selection rule ensure that a nondeciding execution eventually reaches a rank with Q correct first votes for one value, or another condition that forces a commit?

This cannot simply be inferred from current R maxima converging: immutable first votes might have been issued earlier, before alignment. Conversely, repeatedly scheduling such early splits without supplying the promised eventual clean rounds is not a counterexample to the complete intended protocol.

The next proof work should:

- Specify when a rank's first R response/first vote can be emitted relative to synchronized collection, and how previously active ranks are handled.
- Establish what the source's R convergence argument provides with independent f,p budgets and certificate-origin witnesses, including selective Byzantine requests and rotating suspensions.
- Use the quorum lemma above immediately once its premise holds, rather than continuing to require convergence to the largest value.
- For executions not reaching that premise, give a progress measure accounting for earlier first votes and certificate ancestry, or a counterexample that satisfies the eventual clean-round assumption.

No such assumption-respecting nontermination execution has been established by the current experiments. A complete general termination proof also has not yet been established.

## Checks

Run `python3 tools/rai/check_clean_convergence.py`. The output is `clean-convergence-checks.json`.

For n=6,f=p=1 the script checks 990 recovery snapshots and 75 fresh B quorum combinations. For n=9,f=2,p=1 it checks 24,570 recovery snapshots and 588 fresh B quorum combinations. It also checks n=3,f=0,p=1. For each possible correct H it enumerates all assignments of three value labels outside H and all signer subsets of size at least Q. It verifies that v never leaves Cand, every singleton is v, and every P-or-larger snapshot resolves to v. The algebraic proof covers arbitrary value alphabets and Byzantine equivocation across snapshots; enumeration is supporting evidence only.

The B checks deliberately include a larger false value and verify adoption of the smaller true value for every quorum. They also verify commit evaluation for singleton true inputs. These are arithmetic/certificate checks, not an implemented synchronizer or exhaustive full-protocol simulation.
