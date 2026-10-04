# Recovery repair investigation: one fast attempt, certified slow fallback

4 October 2026. Design investigation only. No production code, thresholds, or PDF have been changed.

## Recommended direction

Use **one fast attempt per checkpoint instance**, followed by a separately namespaced slow consensus instance whose initial proposals are justified by that fast attempt's recovery certificates. The slow instance then runs its ordinary R -> A -> B algorithm: A takes the value returned by its own slow R step, and recovery is not rerun at each slow rank.

This removes the specific feedback loop in `clean-nontermination-proof.md`: the fast recovery result becomes an initial proposal to slow R; it can no longer replace the value that slow R has already selected at every rank. The optimistic fast path can retain its two message delays. The slow path pays for its own initial R exchange, and later slow ranks have no new fast attempts.

The composition has a direct safety proof below. Its general termination theorem is conditional on a separately proved slow engine under the chosen membership/fault model. It is not valid to treat the currently unfinished adapted R/A/B implementation as an already proved black box.

## Investigated shortcuts

### Require P first-vote records for every recovery: does not fix it

For n=6, P=5, correct votes v,v,v,w,w justify recovery v. A Byzantine w vote together with two correct w votes and two correct v votes gives another P-record snapshot v,v,w,w,w, justifying w. Both certificates remain valid.

Generally the counterexample's correct distribution is f+p+1 votes for v and f+p votes for w. Its public P-record snapshot selects v. A private P-record snapshot using all f Byzantine w votes, all f+p correct w votes, and p+1 correct v votes selects w. Waiting for P ensures uniqueness *within each snapshot*, not agreement between snapshots.

### Always choose max(recovery value, R maximum): unsafe

With n=6, all five correct first votes can form FC(v). A later valid R(w) request with w>v may increase R state without changing those first votes. Ignoring recovery and sending A(w) can permit a slow decision for w despite the existing fast decision for v. The hidden-fast constraint cannot be removed by a maximum rule.

### Require all n first votes: may block

Byzantine identities may withhold their first votes forever even after every temporarily suspended correct replica returns. Correct service can guarantee only P=n-f identities. Requiring n is not a general liveness repair.

### Two conflicting recovery certificates as a no-fast proof: useful but insufficient alone

If valid singleton recoveries for different values exist at the same fast rank, no fast certificate can exist there: hidden-fast preservation would force both recoveries to the fast value. This is a portable logical no-fast witness.

It does not revoke an already possible slow commit, erase signed A/B snapshots, or stop unaware replicas from attempting fast decisions in later ranks. A mid-instance mode switch needs additional decision-preservation and mode-transition rules. It is therefore not a drop-in justification for replacing a recovery value or resetting A/B state.

## One-shot composition

Use distinct domains for the initial fast attempt and for slow fallback. The fast attempt has one immutable first-vote slot per replica for the whole instance, not one new slot per slow rank.

1. Run the initial fast R exchange. F=n-p matching first votes form a decision certificate, as before.
2. If proceeding on the slow path, construct a valid resolved recovery certificate RC_0(x). Singleton recovery supplies x; empty recovery supplies x from its verified fast-attempt R evidence.
3. Submit (x,RC_0(x)) as an initial proposal to a fresh slow consensus instance. Every initial slow proposal, including a Byzantine proposal, must pass this external-validity predicate. A raw application-valid value is insufficient.
4. Slow R selects among those justified proposals and later certified slow predecessors. Slow A uses slow R's result. There is no per-slow-rank first-vote fast listener and no recovery override between slow R and A.
5. A slow decision is a decision for the same checkpoint instance. A valid initial fast certificate remains acceptable even if learned later. Correct replicas continue proof/response service after deciding so peers can finish.

Fast FIRST signatures must be domain-separated from slow messages and bind the same checkpoint instance. Do not reinterpret existing per-rank signatures across modes. This is a prospective protocol change, not a safe hot migration of an already-running instance.

## Safety theorem for the interface

Assume the slow engine provides agreement and external validity: it decides only a value with a valid RC_0 certificate (and its later ancestry preserves that admission constraint). Assume the original first-vote uniqueness and hidden-fast recovery checks are enforced.

- **Fast/fast:** two F certificates for different values cannot coexist by correct first-vote immutability and fast-quorum intersection.
- **Slow/slow:** follows from slow-engine agreement.
- **Fast/slow:** suppose FC_0(v) exists, whether known now or revealed later. Hidden-fast preservation implies v belongs to every valid recovery snapshot's candidate set. Empty recovery is impossible and every resolved singleton is v. Thus the slow engine admits only v as an initial proposal and, by external validity, cannot decide any other value.

The argument is independent of certificate arrival order. A hidden initial fast certificate that appears after a slow decision must agree with it. With no fast certificate, different recovery values are simply competing initial proposals for the slow engine to resolve normally.

This proof does not require a universally verifiable no-fast certificate, unanimous recovery snapshots, or an expiry rule for historical evidence.

## Conditional termination theorem

Assume:

- the fixed temporarily suspended correct cohort returns and services pending evidence (or the specified recurrence/service condition suffices);
- at least P correct first votes and the needed R/application/ancestry evidence eventually become available, or a resolved recovery certificate is learned earlier;
- the slow engine terminates under this eventual participation/network model with valid correct proposals;
- decided replicas continue the required relay/response services.

Then either an initial fast certificate decides and is relayed, or recovery eventually resolves because P records have at most one candidate. In the empty case, the independently verified initial R evidence must also become available. Correct replicas therefore have admissible slow proposals. The slow engine eventually decides; the interface safety theorem makes that decision compatible with every possible initial fast certificate.

There is no claim that the slow engine decides in a fixed number of rounds, or that the existing independent-f,p Archipelago adaptation already meets the slow-engine premise.

## Choosing the slow engine is a real remaining decision

The supplied original report itself discusses a fast-path/backup composition in Section V (p. 10). Its original Algorithm 5 and fault model must be respected when using its termination theorem; simply replacing 2f+1 with Q in an unproved adaptation is insufficient.

For the latest **final-return** model, a concrete option to audit is a deterministic slow subcommittee of 3f+1 identities from the n-member fast committee, running original slow thresholds 2f+1. It contains at most f Byzantine replicas and eventually at least 2f+1 active correct replicas after return. This changes slow membership/quorums and may wait for returning subcommittee members; it should not be silently adopted. The source's validity/reliability rules and proposal-universe premise still need checking with the RC admission predicate. If leaderless progress is required while the same members continue being suspended repeatedly, the corresponding source suspension/Byzantine budget must be checked separately.

Alternatively retain all n replicas but finish a standalone proof of the slow engine at Q=2f+p+1. The one-shot interface eliminates the new per-rank recovery feedback problem, but does not substitute for that proof. A different independently verified consensus engine with external validity can also satisfy the composition theorem; choosing one may change the leaderless requirement.

## Effect on the existing counterexample

The same initial split may produce RC_0(v) publicly and RC_0(w) privately. Both are legitimate initial slow proposals. In the counterexample's next slow R exchange, the certified maximum is w. Correct A inputs are now w, not v. Even if Byzantine replicas have valid lower R evidence and introduce A(v), every fresh clean correct A snapshot already contains w. No Q singleton-v certificate can be formed. Consequently the attack's private true-v B input cannot be recreated in that slow rank.

This explicitly breaks the demonstrated loop. It is not, by itself, an exhaustive proof of all possible executions of a selected slow engine.

## Tradeoffs

- Preserve the initial optimistic two-message-delay fast path.
- Remove repeated fast attempts at later slow ranks.
- Add the slow engine's initial R exchange after recovery; do not merely delete recovery calls while reusing incompatible initial A/B state.
- Keep late initial fast certificates valid and safe.
- Require domain separation, external-validity checks on every slow entry path, and a separately justified slow-engine membership/fault model.
- No disk persistence work is introduced by this investigation; the prior process-lifetime limitation remains.

## Checks

Run `python3 tools/rai/check_recovery_repair.py`; saved output is `recovery-repair-checks.json`. The checker demonstrates conflicting P-sized recoveries, the hidden-fast danger of blindly taking the larger R value, exhaustive bounded hidden-fast interface checks for n=6 and n=9, and exclusion of the old lower-true A certificate in the same attack after the fresh slow R exchange.

Identity sets abstract signature verification; these are arithmetic/interface checks, not a production implementation or full slow-consensus model checker. The interface theorem is proved above; the slow engine remains an explicit dependency.
