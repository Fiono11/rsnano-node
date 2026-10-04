# Fixed temporary suspensions

> **Later finding (4 October 2026):** [A clean-round nontermination construction](clean-nontermination-proof.md) now covers the documented recovery and historical-certificate rules with every correct replica active. It uses the recovery gate to return below the common R maximum and private Byzantine certificates. This supersedes the earlier “no assumption-respecting counterexample established” status below; see the construction's explicit rule scope and executable checker.

4 October 2026. The suspended correct replicas return; permanent absence is not assumed.

## Models distinguished

**Final return:** a fixed set S of at most p correct replicas can be suspended before a finite time T_return, and afterwards every correct replica remains active. After max(GST,T_return,alignment time), the remaining convergence problem has no correct suspensions.

**Repeated temporary suspension of a fixed cohort:** only identities in S may be suspended, possibly repeatedly, but each returns and services pending requests/evidence. There is a stable core of at least n-f-p=Q correct replicas outside S that is never suspended. A final all-active suffix need not exist.

Both models retain the configured n,f,p and existing Q,F,P thresholds. Returning replicas do not justify resetting immutable votes, silently changing p, or accepting uncertified rank skips.

## Recovery availability can be separated from convergence

For an activated rank, assume all correct replicas eventually emit (or retransmit an existing) valid first vote for that rank and its ancestry, and fair evidence service delivers the records. There are P=n-f correct identities. Each record arrives in finite time; since there are finitely many correct identities, eventually all P are available. No Byzantine cooperation is required.

At m=P, candidate support is P-f-p=f+p+1. Two distinct candidates would require 2(f+p+1)=P+1 records, impossible in a P-record snapshot. Recovery therefore ceases to be ambiguous. It may yield:

- a singleton, which supplies the A input;
- no candidate, which authorizes using the independently validated ordinary R maximum after its evidence is available.

“No candidate” is not a deadlock or an arbitrary-choice rule. The ordinary R certificate and its witnesses must still be obtained. It is not necessary to identify which peers are correct: the existence of P correct suppliers ensures enough distinct identities can eventually be collected despite Byzantine withholding, and the cardinality lemma applies to any P-identity snapshot.

A return alone does not create an unissued first vote. The service must process pending historical rank requests consistently with rank/ancestry rules; simply relaying already-created records does not prove this. The implementation plan's selected-R-rank obligation still matters.

### Six-replica example

While one correct replica is temporarily suspended and one Byzantine replica withholds messages, the four active correct votes can be v,v,w,w. At Q=4, support is two, so the process waits.

When the correct replica returns:

- If its first vote is v, the snapshot v,v,v,w,w resolves to v (support three).
- If its first vote is a third value u, the snapshot v,v,w,w,u has no candidate. Recovery then proceeds using valid R evidence.

Thus the previously described permanent-absence deadlock does not apply to the user's model.

## What stability simplifies

The never-suspended core has at least Q correct replicas, enough to supply ordinary R/A/B response quorums without Byzantine help. Its membership stays unchanged across rounds, avoiding the need to argue that moving awake sets hand off each intermediate step successfully. Returned first votes can be relayed through this core.

If there is a final return, historical state before the final synchronized suffix is finite. It can be handled as an arbitrary finite starting configuration for a suspension-free convergence proof. This observation does not itself prove that fresh legal ranks will be reached or that old evidence is harmless.

If the fixed cohort returns repeatedly, the recovery argument still applies once each required historical vote is produced and transferred. There need not be a simultaneous all-correct return. This establishes evidence availability, not a uniform latency bound or full election termination.

## What stability does not establish

The checked selective-Byzantine R example in `alignment-obligation-analysis.md` already has every correct replica online throughout. All correct requesters can return the same R maximum while immutable correct first votes remain split. Therefore final return does not automatically produce Q matching correct first votes.

Similarly, response-state updates do not invalidate existing signed certificates. Removing rotating suspensions changes neither certificate validity nor the priority of true B evidence over larger false values.

The fixed-suspension model therefore removes a complication from the liveness proof, not the requirement for a convergence argument. We still need either:

- eventual Q matching correct first votes, which force every resolved recovery to one value; or
- the fresh clean A/B progress branches (or another proved progress measure), accounting for historical first responses and selective Byzantine evidence.

The existing counterexamples without clean first-response ordering do not disprove termination with an eventual clean synchronizer. No nonterminating execution meeting all the intended static-temporary/clean-round assumptions has been established here. Conversely, the missing general synchronizer/progress proof has not been completed merely by fixing the suspended identities.

## Fast and slow completion after return

All correct replicas together number P=n-f; a fast certificate requires F=n-p. Byzantine-independent fast completion from unanimous correct votes is possible only when P>=F, equivalently p>=f.

- n=6,f=p=1: P=F=5, so five matching correct first votes can fast-decide.
- n=9,f=2,p=1: P=7<F=8. Even after every correct replica returns, Byzantine withholding can prevent a fast certificate. The slow path is therefore essential.

These are availability statements; the correct first votes must also match for a fast certificate. Q matching **correct** first votes remain sufficient to force slow-path recovery regardless of fast availability.

## Checked evidence

Run `python3 tools/rai/check_static_temporary_suspensions.py`. Output is saved in `static-temporary-suspension-checks.json`.

With a fixed suspended identity and three value labels, the script exhausts 243 correct-vote assignments for n=6 and 2,187 for n=9. Every complete P-record snapshot has at most one candidate. Of the initially ambiguous assignments, 54 for n=6 and 180 for n=9, all lose ambiguity after return; some become empty and therefore require R evidence. The script also reruns the all-correct-online R/first-vote split example and checks fast-threshold availability.

The algebraic recovery proof is general; the enumeration is supporting evidence. These are static evidence checks, not an exhaustive network model or a completed consensus termination proof. No production code, thresholds, PDF, or implementation gate was changed.

## Recommendation

Use the fixed-temporary-cohort assumption explicitly if it matches the target environment. Prove recovery availability as above, then state the remaining convergence theorem under the stable core (or all-active suffix after final return). Keep first-vote timing, fresh A/B response ordering, and historical certificate handling explicit. Do not infer full termination merely from all replicas eventually being online, and do not introduce a quorum or fast-path change without a separate specification.
