# A clean-round nontermination execution for the fast adaptation

4 October 2026. Constructive counterexample for the stated first-vote recovery, mutable/recurringly batched response, historical-certificate, and per-origin Q-witness rules. This supersedes the earlier investigation status that no clean-round counterexample had been established. It is not a counterexample to the original Algorithm 5 without the recovery gate.

## Theorem and precise scope

For n=6, f=p=1, the fast adaptation admits an infinite nondeciding execution with two application-valid values v<w, one Byzantine replica, every correct replica continuously active in the suffix, and clean request-before-response ordering for all correct R/A/B requests. The execution also works for n=9,f=2,p=1 and general f>=1,p>=0 with the stated thresholds.

The rules used are:

1. Each correct replica emits one immutable first vote per rank. Singleton recovery selects its candidate even when it is below the current certified R maximum; this is the stated Section 3.3 rule.
2. Correct A/B state is monotone and incorporates later requests. Correct replicas may send updated responses in later communication rounds, with at most one state update per round. They complete a phase from Q eligible responses; a response needs Q complementing responses for its originating request(s).
3. A snapshot is usable after its origin witnesses arrive; later snapshots do not revoke its signature. Certificates may be assembled privately and revealed later.
4. Byzantine replicas may send different valid requests to different recipients, withhold their own responses and first votes, and reveal them inside a valid certificate later. No correct identity equivocates and no signature is forged.
5. All awake correct requests in a phase are processed before the correct responses of that round. A clean round does not require Byzantine requests to be delivered uniformly. Responses missing origin witnesses stay pending, so a phase can span additional communication rounds.
6. First responses are not globally sealed forever. A signed request/response round number alone does not expire historical certificates or require every piece of origin evidence to have been created in the same round. Such restrictions would be additional protocol rules.

These conditions match the documented adaptation and the latest recurring-update proposal. A stronger synchronizer or verifier that forbids one of them would need an explicit specification and separate analysis. The counterexample does not rely on responses sent before the clean correct-request boundary, permanent suspension, or a lack of fair relay.

## Six-replica execution

Correct replicas are 1,2,3,4,5; Byzantine replica is 6. Let L={1,2,3} and H={4,5}. Every correct proposer carries v into the rank. Byzantine replica 6 has a valid introduction for w: an authenticated proposal at rank zero, and the private B(w) certificate constructed below at each subsequent rank. Its v introduction is also valid.

### R: everyone returns w, but recovery selects v

All five correct replicas broadcast R(v). Before any correct response, every correct replica receives all five correct requests. Byzantine 6 supplies its valid R(w) request to H before their first response and to L only after the first response batch (through relay). Thus correct immutable first votes are:

    1:v, 2:v, 3:v, 4:w, 5:w.

Initially the w request lacks Q=4 processing witnesses: H contributes two, and even a Byzantine witness would bring the count only to three. Three public low-state responses are insufficient for a Q-response R completion. After the next update/relay batch everyone has processed w, its origin has five correct witnesses, and everyone can R-return w.

Byzantine 6 withholds its own first vote w. Correct recovery uses the available correct records:

    S_public = {1:v,2:v,3:v,4:w,5:w}.

Here m=5 and support m-f-p=3, so the unique candidate is v. If a requester resolves earlier at m=4, deliver three v records and one w record first; that also resolves to v. Every correct requester therefore sends A(v), even though its R maximum is w.

Privately, replica 6 uses:

    S_private = {1:v,4:w,5:w,6:w}.

Here m=4 and support is two, so the unique candidate is w. Both recoveries are valid; they use different identity sets, and no correct first vote changes. No fast certificate exists: v has only three correct first voters, w has only two; Byzantine support cannot bring either to F=5.

### A: public false-w, private true-v

All correct replicas broadcast A(v) with valid recovery evidence. All correct A(v) requests arrive everywhere before responses. Byzantine 6 also sends a valid A(v) request to L and a valid A(w) request to H; the latter carries S_private.

The first batch of correct responses is:

| Correct responders | State after all correct requests plus selective Byzantine input |
| --- | --- |
| 1,2,3 | {v} |
| 4,5 | {v,w} |

The mixed responses are initially pending because the Byzantine A(w) origin has only two correct processing witnesses (at most three with Byzantine help). The three singleton responses cannot form a public Q certificate. Replica 6 withholds its own A responses from correct requesters.

Replica 6 retains the three singleton responses to its A(v) request and adds its own signed singleton-v response. These form a private valid A certificate returning true-v, once its complementing evidence is available. Its v origin can have four witnesses from L plus replica 6, or all five correct witnesses after relay.

The A(w) request and its proof are relayed. In the next batch every correct replica incorporates w and responds {v,w}; the A(w) origin now has five correct witnesses. Each correct requester can collect Q eligible mixed responses to its own A(v) request and returns false-w. The first singleton responses have not been revoked, so the private true-v certificate remains valid.

### B: public adopt-v, private adopt-w

All correct replicas now broadcast B(false,w), with their valid mixed A certificates. Everyone receives all five correct B requests before responding. Byzantine 6 sends B(false,w) to L, using a valid false-w A certificate, and B(true,v) to H, using its privately retained true-v A certificate.

The first B responses are:

| Correct responders | B state |
| --- | --- |
| 1,2,3 | {(false,w)} |
| 4,5 | {(false,w),(true,v)} |

Again, the mixed responses initially lack Q origin witnesses: only two correct replicas have processed the Byzantine true-v request. The three eligible false-only responders cannot form a public Q certificate without Byzantine cooperation.

Replica 6 keeps the three false-only responses to its B(false,w) request and adds its own signed false-w response. This creates a private B certificate that **adopts w**. The origin of false-w is justified by the correct false-w A certificates and has adequate complementing evidence by the end of the phase.

The true-v B request is relayed, and every correct replica incorporates it in the next update batch. Now every correct B response contains both false-w and true-v, with Q origin witnesses available. Every correct requester collects Q such responses and **adopts v**. None commits: no correct B response in the entire rank is singleton true. The private adopt-w certificate is still valid.

Every late request has now been processed, and every correct replica has the same current A/B state. State convergence has happened. It does not erase the private historical B(w) certificate.

## Infinite continuation and absence of decisions

At the end of the rank:

- every correct proposer carries v;
- Byzantine 6 holds a valid B certificate for w;
- every correct first vote is unchanged;
- no fast or slow decision certificate exists.

This is exactly the introduction condition needed for the next rank: correct replicas justify R(v) with their preceding B(v) certificates, while replica 6 justifies R(w) with its preceding private B(w) certificate. A new higher rank takes priority over the previous rank's R maximum; retaining w at rank i does not make (i,w) greater than a valid (i+1,v).

Repeat the schedule at each new rank. Each rank has finite ancestry into the previous one, ending at the two valid rank-zero proposals. No incorrect predecessor rank or fabricated high rank is used. Old snapshots are used only at their own rank to build that rank's certificate; the resulting certificate is then used at exactly the next rank.

The absence of decisions is inductive:

- Correct first votes are always split 3:2, so even Byzantine cooperation could not reach F=5 matching votes.
- No correct B response is ever singleton true. Q=4 cannot be assembled from the sole Byzantine identity, so no slow commit certificate exists.
- All public completions are adopt-v. A new rank is reached each time, so this is an infinite progressing execution without a decision, not merely a finite wait.

There is no need to delay a correct message forever. Each phase has an initial response batch, relay/witness dissemination, and an updated response batch. Additional rounds for reliable evidence dissemination can be inserted finitely. All correct requests precede each applicable response batch, all messages sent by correct replicas are eventually delivered, and all late requests are processed. Byzantine private signatures are allowed to remain private until used in the following phase or rank. A fixed finite number of communication rounds per rank realizes the pattern indefinitely.

The suspended set may be empty in the suffix, which satisfies “at most p temporarily suspended correct replicas.” If a nonempty temporary suspension is desired, prepend a finite suspension-and-return before the execution begins. The counterexample does not depend on rotating or permanent suspensions.

## General counts

Let C=P=n-f=2f+2p+1, Q=2f+p+1, F=3f+p+1, and split correct replicas into:

    |L| = Q-f = f+p+1,
    |H| = f+p.

All correct first votes in L are v; those in H are w. The public P-record snapshot has singleton v at support f+p+1. A private Q-record snapshot consists of all H votes w, all f Byzantine votes w, and one correct v vote. It has Q-1 votes for w and one for v, so it resolves to w for f>=1.

At each first A/B batch, L's public low responses number Q-f<Q. The H-only high origin has at most |H|+f=Q-1 witnesses, so its responses cannot yet be eligible. Byzantine replicas can privately add their f low responses to L's Q-f responses, obtaining Q. After relay, all C correct updated responses qualify for the public opposite result. Neither first-vote value can reach F: even with all Byzantine votes, v has Q<F and w has Q-1<F for f>=1.

Only the two values v,w are needed. The counterexample does not use an unbounded proposal universe.

## Why this breaks the fast adaptation rather than the original slow protocol

The pivotal step is **R-return w followed by correct A(v)**. It is permitted by the fast adaptation's valid singleton recovery. In the original Algorithm 5 without this recovery gate, the correct A request uses the returned R value w. Then these same clean A batches cannot supply the privately retained singleton-v certificate. The construction therefore does not contradict the original source theorem.

A repair must address this interaction, not just make replicas learn w: every replica already learns w in each rank. Simply substituting w for a resolved recovery v is unsafe without a new hidden-fast preservation argument. Similarly, discarding older valid certificates, requiring same-round witnesses, or revising completed phase requests would change the stated protocol and requires a separate specification/proof.

## Executable check and assurance limits

Run:

    python3 tools/rai/check_clean_nontermination.py

The saved output is `clean-nontermination-traces.json`. The checker constructs three consecutive ranks for n=6,f=p=1 and n=9,f=2,p=1. It checks distinct response signers, response/trigger binding, processing of each triggering request before reply, delivery of all correct requests before responses, Q origin witnesses, both recovery evaluations, public/private A and B evaluations, eventual processing of all late requests, and the repeating carry/ancestry condition. It checks all correct Q subsets for each public phase result. The mathematical induction above, rather than three finite iterations alone, establishes the infinite extension.

Cryptography is abstracted by authenticated identities; only Byzantine identities create their own private signatures. The repository still lacks the full production verifier and synchronizer, so this is not an end-to-end node reproduction. The result is a counterexample to the explicit rules above. An unstated stricter rule may exclude it, but then that rule must be specified and its safety/liveness consequences examined rather than assumed.
