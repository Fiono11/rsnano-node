---------------------- MODULE RaiRecoverManyScenarios -----------------------
(***************************************************************************)
(* Scripted scenarios for RaiRecoverMany: action constraints that confine  *)
(* the validators to the roles of an expected trace.  A restriction only   *)
(* removes behaviours, so a violation found under one is a run of the full *)
(* model.  The constraints name validators, so run these without the VIEW. *)
(***************************************************************************)
EXTENDS RaiRecoverMany

NewVote(w, e, s) == fv'[e][w][s] # fv[e][w][s]

\* Early finality under the overlap rule: before S_1 is decided, the only
\* epoch-2 first votes at v are early votes for A_1 by validators 1, 4 and 5
\* (4 and 5 late-notarize it in epoch 1; 1 voted it in epoch 1); validator 6
\* adds its votes.  Closure 1 is decided only once A_1 has an epoch-2
\* certificate.  Three correct votes are needed: 4, 5 and 6 are one short
\* of an NC.
EarlyOverlap ==
  /\ ~S[1].done => \A w \in C : NewVote(w, 2, 1) =>
                     w \in {1, 4, 5} /\ fv'[2][w][1] = A(1) /\ Early(w)
  /\ (S'[1].done /\ ~S[1].done) => Cert(2, A(1))

\* The ClosureThm1 counterexample of the paper's rules (BLOCKS = 2), run on
\* into epoch 3: in epoch 2, 2 and 4 vote A2 early and 3 and 5 vote A1
\* early, 1 votes A2 after installing S1, and 1, 2, 4, 5 vote C2 settled;
\* in epoch 3 only A1 is voted at v and nothing at v+1.
PaperHole ==
  /\ \A w \in C : NewVote(w, 2, 1) =>
       CASE w \in {2, 4} -> fv'[2][w][1] = A(2) /\ Early(w)
         [] w \in {3, 5} -> fv'[2][w][1] = A(1) /\ Early(w)
         [] OTHER       -> fv'[2][w][1] = A(2) /\ ~Early(w)
  /\ \A w \in C : NewVote(w, 2, 2) => w \in {1, 2, 4, 5} /\ ~Early(w)
  /\ \A w \in C : NewVote(w, 3, 1) => fv'[3][w][1] = A(1)
  /\ \A w \in C : ~NewVote(w, 3, 2)

\* The same with the two checkpoints of the counterexample: S1 keeps A2 and
\* S2 keeps A1.
PaperHoleTight ==
  /\ PaperHole
  /\ (S'[1].done /\ ~S[1].done) => A(2) \in KV(1)'
  /\ (S'[2].done /\ ~S[2].done) => A(1) \in KV(2)'

\* TLC checks invariants also in the state a constraint-violating step
\* reaches, so both name the early certificate instead of relying on the
\* constraint.
\* Does S_1 keep a rival of the early-final A_1?  (Probe.)
ProbeS1Rival == ~(S[1].done /\ A(1) \notin KV(1) /\ Cert(2, A(1)))
\* The checkpoint of the epoch of the early certificate keeps A_1.
YieldNext == (S[2].done /\ Cert(2, A(1))) => A(1) \in KV(2)
==============================================================================
