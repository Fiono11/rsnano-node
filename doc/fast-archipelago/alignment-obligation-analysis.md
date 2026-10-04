# Does eventual synchronization overcome early first votes?

> **Later finding (4 October 2026):** [A clean-round nontermination construction](clean-nontermination-proof.md) now covers the documented recovery and historical-certificate rules with every correct replica active. It uses the recovery gate to return below the common R maximum and private Byzantine certificates. This supersedes the earlier “no assumption-respecting counterexample established” status below; see the construction's explicit rule scope and executable checker.

4 October 2026. Investigation against the supplied original report, corrected Fast Archipelago document, and current pure implementation.

## Findings

1. First votes issued before a finite stabilization point affect only finitely many ranks in a standard non-Zeno execution. They do not, by themselves, establish a permanent obstruction.
2. A clean ordinary R exchange need not produce Q matching correct **first votes**, even at a fresh rank. Agreement of R return values and agreement of first responder snapshots are different properties.
3. Matching first votes are sufficient, not necessary. A fresh clean mixed A rank can eliminate a value and force a subsequent decision without a matching first-vote quorum in the first rank.
4. The missing general argument is to establish repeated fresh, correctly ordered response opportunities and a valid progress measure. The supplied source's synchronization assumptions are not a first-vote-aware synchronizer specification, and the production driver/synchronizer is not implemented. This investigation neither completes a general termination proof nor supplies a nontermination execution satisfying all intended assumptions.

## 1. Finite pre-alignment history

Let T be the finite point after which the assumed round discipline holds. A finite execution prefix contains finitely many correct signing events, so the set of ranks for which a correct replica emitted a first vote before T is finite. It has a maximum M if nonempty.

A Byzantine process cannot make valid high-rank ancestry out of its signatures alone: every predecessor B or fast certificate needs more than f distinct identities. A purported later certificate assembled after T from earlier correct signatures remains bounded by the finite ranks actually signed in that prefix.

Thus, **if** the nondeciding execution reaches arbitrarily high fresh ranks, it eventually reaches ranks above this pre-T contamination frontier. This lemma does not authorize timeout-only rank skipping, reset first-vote slots, or prove that ranks keep advancing. Recovery completion, evidence availability and legal predecessor production must justify advancement. It also does not prevent new premature first responses after T unless the synchronizer actually governs the responder handlers.

## 2. Common R returns do not imply common first votes

The original report's Lemma A.24 (p. 26) concerns the values returned by R invokers after gathering response evidence. The new adaptation records immutable first votes at responders when they first answer. The original algorithm has no such immutable first-vote record.

Here is a fresh clean six-replica example with Q=4, v<w:

- Five correct replicas send R(v). All their requests are delivered before first responses, satisfying the correct-request ordering condition.
- Byzantine replica 6 also has a valid R(w) introduction. It delivers that request to correct replicas 1,2,3 before their first response, and to 4,5 later.
- Correct first votes are `w,w,w,v,v`. No value has four correct first votes.
- The w origin has four processing witnesses: replicas 1,2,3 and Byzantine 6. Every quorum of four R-response identities intersects the three correct w responders, so every requester can return the same certified maximum w.
- Nonetheless, `{1:w,2:w,3:w,4:v}` resolves recovery to w, while `{1:w,4:v,5:v,6:v}` resolves to v. Byzantine 6's own first-vote report can differ; correct votes remain immutable.

This example is not a termination counterexample. It demonstrates that the shortcut “synchronized R returns agree, therefore immutable first votes agree” is invalid. Correct R maxima may all become w while a valid earlier recovery for v remains available.

The independent-budgets n=9,f=2,p=1 example has four correct w first votes, three correct v first votes, and two Byzantine identities. Its Q=6 R response quorums all return w, while both recovery values remain possible.

## 3. A checked slow-path route to convergence

Continue the example with both correct A(v) and A(w) requests. Suppose a set H of Q correct responders incorporates both before its **first-ever A response** at this rank. Every such response contains {v,w}; later updates preserve the conflict and maximum.

Every Q-identity A certificate intersects H. In this two-value example it therefore returns false-w. This includes any historical certificate: H never signed a prior singleton at that rank. Consequently no valid true-v or true-w A certificate exists, all valid B inputs are false-w, and every valid B certificate adopts w. The actual first-vote distribution also excludes any fast certificate in the example.

All next-rank legal introductions therefore carry w. Valid first votes and recovery evidence at that rank carry only w, A returns true-w, and Q correct B responses can commit w. The initial rank did not require Q matching correct first votes.

Likewise, if a clean B round incorporates true-v before Q correct responders' first-ever B response at that rank, every historical/future B certificate carries v. Together with validated fast/slow compatibility this restricts the next rank to v and yields the analogous decision route. The value can be v or w; numerical maximality is not the decision criterion.

These are conditional complete local branches, not a proof that every nondeciding execution eventually enters one of them. The first-ever-response condition must not be silently replaced with “their latest responses now agree.”

## 4. What must be established for the general theorem

The original supplied report defines eventual synchronous rounds on pp. 3–4, distinguishes request and response ordering on p. 8, uses mutable per-request handlers in Algorithm 5 (p. 9), and invokes round synchronization/value elimination on pp. 26–27. The adaptation's Section 2.1 assumes clean aligned R/A/B ranks but explicitly specifies the request-before-response ordering for clean A/B rounds. None of this is an implementation of a new immutable-first-vote-aware synchronizer.

The implementation currently provides pure first-vote/recovery and A/B helpers. It has no production recursive evidence verifier, complete driver, or synchronizer from which the stronger ordering properties can be derived.

A useful proof/implementation contract needs to establish:

- **Responder scheduling:** eventual alignment must govern first responses to valid relayed/Byzantine requests, not merely when correct proposers start a phase. If valid requests are answered before alignment, first votes and historical A/B snapshots can already exist.
- **Fresh opportunities:** in a nondeciding execution, recovery and valid predecessor evidence eventually allow progress beyond the finite pre-alignment ranks. The driver cannot silently reset or skip a rank to obtain this property.
- **Progress despite split first votes:** common R returns alone are insufficient. Use the fresh mixed-A and true-B branches or prove another progress measure accounting for all admissible historical evidence and rotating awake sets.
- **Finite/well-founded measure:** if using value elimination, establish which value set or ordered measure is finite/well-founded under the actual proposal policy. Authentication alone does not bound it by n.
- **Recovery service:** recurring correct replicas must eventually emit any still-needed first vote and disseminate its ancestry. Fair relay of records already created is only part of that argument.

These are constraints on a candidate completion, not protocol changes already adopted. Delaying all first votes until an additional agreement exchange would change the fast-path behavior and must not be introduced as a silent proof repair.

## Recommendation

Do not make Q matching first votes the sole target of the termination proof. Keep it as a decisive sufficient lemma, but prove the source-style slow-path progress with the recovery gate explicitly included. First establish an eventual fresh-response ordering contract for the actual synchronizer, then apply the two local convergence branches and examine the remaining singleton/late-Byzantine cases. Preserve existing vote locks and certificate verification while this is unresolved.

The pre-alignment concern is finite and potentially manageable. The more significant adaptation gap is that source R-return convergence does not directly transfer to immutable first-vote convergence. The clean mixed-A example demonstrates a viable route around that gap without changing the first-vote rule, but does not finish the general proof.

## Reproduction and scope

Run `python3 tools/rai/check_alignment_obligation.py`; output is in `alignment-obligation-checks.json`. It checks all 15 R-response quorums for n=6 and all 84 for n=9, two valid singleton recoveries in each configuration, absence of Q matching correct first votes and possible fast certificates, all fresh mixed-A quorum combinations with three adversarial outsider snapshot choices (45 and 252 respectively), and the consequent B adoption/next-rank commit evaluations.

The script abstracts signature verification, application validity and the network, and uses explicit two-value ancestry premises. It is a local certificate/arithmetic experiment, not a full execution model or proof of arbitrary-schedule termination. No original-protocol nontermination claim is made. The production implementation and PDF are unchanged.
