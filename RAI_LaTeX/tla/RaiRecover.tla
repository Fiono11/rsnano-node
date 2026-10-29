----------------------------- MODULE RaiRecover -----------------------------
(***************************************************************************)
(* A fresh child of a kept block, voted while a rival at the parent's      *)
(* position still has live votes; and recovery of a forked account under   *)
(* a proposed patch.                                                       *)
(*                                                                         *)
(* One account.  Position v (1) holds the conflicting blocks A and A'      *)
(* ("A2"); position v+1 (2) holds C on A' and D on A.  The parent of v is  *)
(* final at genesis.  N = 6, f = p = 1: five correct validators C and the  *)
(* Byzantine validator byz.  Four voting epochs and three closures, one    *)
(* committee.  Each validator has its own boundary (cur) and its own       *)
(* installed checkpoint (inst); it may install S_e before its own boundary *)
(* out of e, and then casts no early votes in e+1.  An epoch-e first vote  *)
(* is settled if its signer had installed S_{e-1}, early otherwise.        *)
(*                                                                         *)
(* Built in from the paper: one support per instance, the cross-epoch rule *)
(* (no switching while S_{e-1} is unknown), late notarization, the early-  *)
(* vote rule (FF counts settled first votes only; an early final vote      *)
(* needs the prefix-wide overlap guard with a fresh child witness), Fix B, *)
(* Rules 1-3 with origin-stamped records, matching-origin discharge and    *)
(* promotion.  The consensus service picks any eligible candidate built    *)
(* from any N - f usable reports.                                          *)
(*                                                                         *)
(* The owner signed A and A' in epoch 1 and nothing else, except that once *)
(* a decided checkpoint keeps A' (A) it publishes C (D).                   *)
(*                                                                         *)
(* Switches:                                                               *)
(*  IMPLIED  "off"       a vote for C says nothing about position v.       *)
(*           "all"       a first or final vote for a child also counts as  *)
(*                       its signer's support for the parent at v in that  *)
(*                       epoch, early votes included: in NC, XW, Rule 2,   *)
(*                       single support and the cross-epoch rule.          *)
(*           "committed" the same, but early first votes are exempt (the   *)
(*                       signer's later final vote for the child is not).  *)
(*  SETTLED  Rule 2 counts only settled first votes (else all).            *)
(*  PICKRULE "none": Rule 2 keeps a block only if it alone reaches r       *)
(*           (PICK in the task; PICK is a TLA+ proof keyword).             *)
(*           "hash": where nothing else is kept, keep the block with most  *)
(*                   reporter votes, then smallest hash.                   *)
(*           "prev": there, keep the previous checkpoint's block if still  *)
(*                   eligible, else as "hash".                             *)
(*           A pick runs bottom-up, among blocks compatible with final     *)
(*           blocks and all other records, stamps a record of origin e     *)
(*           (kind "P") and is never promoted.                             *)
(*  ATTACH   "strict": after installing a checkpoint a validator votes     *)
(*           nothing that conflicts with a record it carries.              *)
(*           "permissive": it may, holding a matching-origin witness for   *)
(*           every record bypassed.  (Notarization locks never.)           *)
(*  RENOTARIZE  the paper's re-notarization rule: a parent held by         *)
(*           recovery records alone needs an epoch-e NC before a child is  *)
(*           voted.  FALSE: a child of the kept tip may be voted at once.  *)
(*  BYZ      "all": every Byzantine first and final vote counts as         *)
(*           released (safety runs).  "release": Byzantine first votes are *)
(*           released one at a time, final votes never (liveness runs).    *)
(*  E        number of voting epochs (4; 2 or 3 for exhaustive runs).       *)
(*  READY    epochs that are ready: every correct validator installs the   *)
(*           predecessor before voting and leaves only when no vote is     *)
(*           still allowed.  Used by the recovery invariants.              *)
(*                                                                         *)
(* Abstractions:                                                           *)
(*  1. Finality is global: a block is final once an FF or FC for it or a   *)
(*     descendant can be formed from released signatures, or a decided     *)
(*     checkpoint finalized it.  Certificate validity is not checked again *)
(*     (each has at least three correct signers, each of which checked     *)
(*     it).  Admission and guards read formable certificates, not a        *)
(*     validator's received set.  Adds behaviours.                         *)
(*  2. Blocks reach every validator once published.  Adds behaviours.      *)
(*  3. A correct report tags a block N iff its signer final-voted it.  The *)
(*     paper tags N every complete block at the boundary, so the model     *)
(*     also lets a validator leave holding an NC without an N tag (adds    *)
(*     behaviours; in a ready epoch the final vote is forced anyway).      *)
(*     Correct reports carry no F tag for an observed certificate, only    *)
(*     inherited finality (removes checkpoints that finalize a block which *)
(*     is already final).                                                  *)
(*  4. BYZ = "all" makes every Byzantine signature available at once.  No  *)
(*     correct action is disabled by more signatures, so for the safety    *)
(*     invariants this adds behaviours.  BYZ = "release" never releases    *)
(*     Byzantine final votes, which could only complete a certificate.     *)
(*  5. The Byzantine report lists any published blocks in G (which         *)
(*     releases those first votes) and tags N any block with an NC         *)
(*     formable from released signatures.                                  *)
(*  6. The manifest discharges any dischargeable set of records containing *)
(*     those a selected correct vote relied on, and verifies a closing-    *)
(*     epoch NC for promotion at the proposer's choice.                    *)
(*  7. A child is published as soon as a decided checkpoint keeps its      *)
(*     parent, before the owner could have seen it.  Adds behaviours.      *)
(*                                                                         *)
(* Invariants: NoConflict, ClosureThm1, OneKept (sanity), FlipBound,       *)
(* Recovered and RecoveredEarly (with READY), Progress (sanity), probes.   *)
(* The Stop* action constraints prune exploration soundly for one          *)
(* invariant each.                                                         *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS C, byz, E, IMPLIED, SETTLED, PICKRULE, ATTACH, RENOTARIZE, BYZ, READY

ASSUME /\ Cardinality(C) = 5 /\ byz \notin C /\ E \in {2, 3, 4}
       /\ IMPLIED \in {"off", "all", "committed"}
       /\ SETTLED \in BOOLEAN /\ RENOTARIZE \in BOOLEAN
       /\ PICKRULE \in {"none", "hash", "prev"}
       /\ ATTACH \in {"strict", "permissive"}
       /\ BYZ \in {"all", "release"} /\ READY \subseteq 1..E

Epochs == 1..E
Closes == 1..(E - 1)
Q      == 4      \* N - f - p
R      == 3      \* f + p + 1
FAST   == 5      \* N - p
NREP   == 5      \* N - f

Blocks    == {"A", "A2", "C", "D"}
Pos(x)    == IF x \in {"A", "A2"} THEN 1 ELSE 2
At(s)     == {x \in Blocks : Pos(x) = s}
Par(x)    == IF x = "C" THEN "A2" ELSE "A"          \* children only
Base(x)   == IF Pos(x) = 1 THEN x ELSE Par(x)       \* x's block at v
OtherV(b) == IF b = "A" THEN "A2" ELSE "A"
Path(x)   == IF Pos(x) = 1 THEN {x} ELSE {x, Par(x)}
AncClose(X) == UNION {Path(x) : x \in X}
\* Every fork here diverges at v.
Conflict(x, y) == Base(x) # Base(y)
\* Hash order for ties.
Rank(x)    == CASE x = "A" -> 1 [] x = "A2" -> 2 [] x = "C" -> 3 [] x = "D" -> 4
MinHash(X) == CHOOSE x \in X : \A y \in X : Rank(x) <= Rank(y)

VARIABLES fv,    \* fv[e][w][s]: epoch-e first vote of w at position s
          sv,    \* ... cast with S_{e-1} installed (settled)
          fn,    \* ... final vote released
          cur,   \* cur[w]: current epoch (crossing the boundary moves it on)
          inst,  \* inst[w]: latest installed checkpoint
          S,     \* S[k]: checkpoint of closure k (S[0] genesis)
          bf     \* bf[e]: released Byzantine epoch-e first votes (BYZ = "release")
vars == <<fv, sv, fn, cur, inst, S, bf>>

NoCk == [done |-> FALSE, fin |-> {}, locks |-> {}]

-----------------------------------------------------------------------------
\* Signatures and certificates.

B1(c) == IF c THEN 1 ELSE 0
BF(e) == IF BYZ = "all" THEN Blocks ELSE bf[e]
BN(e) == IF BYZ = "all" THEN Blocks ELSE {}

\* w's epoch-e vote for a child supports the child's parent at v.
Implies(e, w) == /\ IMPLIED # "off" /\ fv[e][w][2] # "-"
                 /\ IMPLIED = "all" \/ sv[e][w][2] \/ fn[e][w][2]
Sup(e, w, s) ==
  IF s = 2 THEN {fv[e][w][2]} \ {"-"}
  ELSE ({fv[e][w][1]} \ {"-"}) \cup (IF Implies(e, w) THEN {Par(fv[e][w][2])} ELSE {})
ByzSup(e, x) == \/ x \in BF(e)
                \/ /\ Pos(x) = 1 /\ IMPLIED # "off"
                   /\ \E c \in BF(e) \cup BN(e) : Pos(c) = 2 /\ Par(c) = x

FV(e, x) == Cardinality({w \in C : x \in Sup(e, w, Pos(x))})
\* Late notarizations for epoch e: early epoch-(e+1) first votes of members
\* that supported nothing at that position in e.
Late(e, w, x) == /\ e < E /\ fv[e+1][w][Pos(x)] = x /\ ~sv[e+1][w][Pos(x)]
                 /\ Sup(e, w, Pos(x)) = {}
LT(e, x) == Cardinality({w \in C : Late(e, w, x)})
NC(e, x) == FV(e, x) + B1(ByzSup(e, x)) >= Q
XW(e, x) == FV(e, x) + LT(e, x) + B1(ByzSup(e, x)) >= Q
FF(e, x) == Cardinality({w \in C : fv[e][w][Pos(x)] = x /\ sv[e][w][Pos(x)]})
              + B1(x \in BF(e)) >= FAST
FC(e, x) == Cardinality({w \in C : fv[e][w][Pos(x)] = x /\ fn[e][w][Pos(x)]})
              + B1(x \in BN(e)) >= Q
Cert(e, x) == FF(e, x) \/ FC(e, x)

Final == AncClose({x \in Blocks : \E e \in Epochs : Cert(e, x)}
                  \cup UNION {S[k].fin : k \in Closes})

\* Symmetry and dead history.  w's epoch-e votes are dead once S_e is decided
\* and w has left e and installed S_e: w can no longer vote in e, vote early
\* in e+1 (no cross-epoch check, no new late notarization), or be selected
\* for closure e.  From then on only their contribution to the counts matters,
\* so the view keeps the counts and drops the votes.  Validators are
\* interchangeable, so the view keeps the multiset of local states.
DeadW(e, w) == e < E /\ S[e].done /\ cur[w] > e /\ inst[w] >= e
Agg(e) == LET dw == {w \in C : DeadW(e, w)} IN
  [x \in Blocks |->
     <<Cardinality({w \in dw : x \in Sup(e, w, Pos(x))}),
       Cardinality({w \in dw : Late(e, w, x)}),
       Cardinality({w \in dw : fv[e][w][Pos(x)] = x /\ sv[e][w][Pos(x)]}),
       Cardinality({w \in dw : fv[e][w][Pos(x)] = x /\ fn[e][w][Pos(x)]})>>]
Local(w) == <<cur[w], inst[w],
              [e \in Epochs |-> IF DeadW(e, w) THEN <<>>
                                 ELSE <<fv[e][w], sv[e][w], fn[e][w]>>]>>
View == <<[t \in {Local(w) : w \in C} |-> Cardinality({w \in C : Local(w) = t})],
          [e \in Epochs |-> Agg(e)], S, bf>>

-----------------------------------------------------------------------------
\* Checkpoints and admission.

Held(ck) == AncClose(ck.fin) \cup AncClose({l.b : l \in ck.locks})
NLocked(ck, x) == \E l \in ck.locks : l.k = "N" /\ x \in Path(l.b)
FinOK(x, ck) == \A y \in AncClose(ck.fin) : ~Conflict(x, y)
\* A record of origin o is discharged by an epoch-o exclusion witness for
\* the conflicting branch's block at v (the first divergent position).
Witness(l, x) == l.k # "N" /\ XW(l.o, Base(x))
Dischargeable(l) == l.k # "N" /\ XW(l.o, OtherV(Base(l.b)))

Admit(x, ck) ==
  /\ FinOK(x, ck) /\ x \notin AncClose(ck.fin)
  /\ \A l \in ck.locks : Conflict(x, l.b) => ATTACH = "permissive" /\ Witness(l, x)

Pub(x) == Pos(x) = 1 \/ \E k \in Closes : S[k].done /\ Par(x) \in Held(S[k])

\* May a child x be voted in epoch e on the checkpoint ck?  A settled vote may
\* attach to a kept tip; an early one needs a final, notarization-locked or
\* complete current-epoch parent.
ParentOK(x, e, ck, settled) ==
  LET p == Par(x) IN
  \/ p \in Final
  \/ NLocked(ck, p)
  \/ settled /\ ~RENOTARIZE /\ p \in Held(ck)
  \/ NC(e, p) /\ FinOK(p, ck)

\* The overlap guard, prefix-wide with a fresh child witness, against the
\* latest closed checkpoint ck.  A block final by closing-epoch or older
\* evidence is a safe anchor.
Anchor(e, ck) == AncClose(ck.fin) \cup
                 AncClose({y \in Blocks : \E e2 \in 1..(e - 1) : Cert(e2, y)})
EarlyFinalOK(e, x, ck) ==
  /\ Admit(x, ck)
  /\ \A y \in Path(x) : y \in Anchor(e, ck) \/ (XW(e - 1, y) /\ NC(e, y))

-----------------------------------------------------------------------------
\* Correct validators.

Early(w) == inst[w] < cur[w] - 1

VoteOK(w, x) ==
  LET e     == cur[w]
      s     == Pos(x)
      ck    == S[inst[w]]
      early == Early(w)
      imp   == s = 2 /\ (IMPLIED = "all" \/ (IMPLIED = "committed" /\ ~early))
  IN /\ fv[e][w][s] = "-"
     /\ e \in READY => ~early
     /\ Pub(x)
     /\ Admit(x, ck)
     /\ s = 2 => ParentOK(x, e, ck, ~early)
     \* one support per instance
     /\ s = 1 => Sup(e, w, 1) \subseteq {x}
     /\ imp => fv[e][w][1] \in {"-", Par(x)}
     \* no switching across a boundary while S_{e-1} is unknown
     /\ early => /\ Sup(e - 1, w, s) \subseteq {x}
                 /\ imp => Sup(e - 1, w, 1) \subseteq {Par(x)}

FinalOK(w, s) ==
  LET e == cur[w]  x == fv[e][w][s] IN
  /\ x # "-" /\ ~fn[e][w][s]
  /\ NC(e, x)
  /\ (s = 2 /\ IMPLIED # "off") => fv[e][w][1] \in {"-", Par(x)}
  /\ IF Early(w)
     THEN EarlyFinalOK(e, x, S[inst[w]])
     ELSE /\ Admit(x, S[e - 1])
          /\ s = 2 => ParentOK(x, e, S[e - 1], TRUE)

\* In a ready epoch a validator leaves only once no vote is still allowed.
Done(w) == /\ ~\E x \in Blocks : VoteOK(w, x)
           /\ ~\E s \in 1..2 : FinalOK(w, s)
LeaveOK(w) == /\ cur[w] < E /\ inst[w] >= cur[w] - 1
              /\ cur[w] \in READY => Done(w)
CatchOK(w) == inst[w] = cur[w] - 2 /\ S[cur[w] - 1].done
AheadOK(w) == /\ inst[w] = cur[w] - 1 /\ cur[w] < E /\ S[cur[w]].done
              /\ cur[w] \in READY => Done(w)

Vote(w, x) ==
  /\ VoteOK(w, x)
  /\ fv' = [fv EXCEPT ![cur[w]][w][Pos(x)] = x]
  /\ sv' = [sv EXCEPT ![cur[w]][w][Pos(x)] = ~Early(w)]
  /\ UNCHANGED <<fn, cur, inst, S, bf>>

FinalVote(w, s) ==
  /\ FinalOK(w, s)
  /\ fn' = [fn EXCEPT ![cur[w]][w][s] = TRUE]
  /\ UNCHANGED <<fv, sv, cur, inst, S, bf>>

\* Crossing the local boundary freezes the epoch-e report.
Leave(w) ==
  /\ LeaveOK(w)
  /\ cur' = [cur EXCEPT ![w] = @ + 1]
  /\ UNCHANGED <<fv, sv, fn, inst, S, bf>>

Catch(w) ==
  /\ CatchOK(w)
  /\ inst' = [inst EXCEPT ![w] = @ + 1]
  /\ UNCHANGED <<fv, sv, fn, cur, S, bf>>

\* Installing S_e before the own boundary out of e.
Ahead(w) ==
  /\ AheadOK(w)
  /\ cur' = [cur EXCEPT ![w] = @ + 1]
  /\ inst' = [inst EXCEPT ![w] = cur[w]]
  /\ UNCHANGED <<fv, sv, fn, S, bf>>

ByzRelease(e, x) ==
  /\ BYZ = "release" /\ x \notin bf[e] /\ Pub(x)
  /\ \E w \in C : cur[w] >= e
  /\ bf' = [bf EXCEPT ![e] = @ \cup {x}]
  /\ UNCHANGED <<fv, sv, fn, cur, inst, S>>

-----------------------------------------------------------------------------
\* Closure e: BuildState.

Reporters(e) == {w \in C : cur[w] > e}

NT(e, i) == {x \in Blocks : fv[e][i][Pos(x)] = x /\ fn[e][i][Pos(x)]}
\* Blocks i's epoch-e first votes support (only settled ones if SETTLED).
SupG(e, i) ==
  IF ~SETTLED THEN Sup(e, i, 1) \cup Sup(e, i, 2)
  ELSE {x \in Blocks :
          \/ fv[e][i][Pos(x)] = x /\ sv[e][i][Pos(x)]
          \/ /\ Pos(x) = 1 /\ IMPLIED # "off" /\ fv[e][i][2] # "-"
             /\ Par(fv[e][i][2]) = x /\ sv[e][i][2]}

\* gc[x]: selected correct reporters listing x in G; nc: blocks they tag N.
Build(e, gc, nc, bG, bN, D, pr) ==
  LET prev == S[e-1]
      Fst  == AncClose(prev.fin)
      surv == {l \in prev.locks \ D : l.b \notin Fst /\ FinOK(l.b, prev)}
      Ok(x, L) == /\ x \notin Fst /\ FinOK(x, prev)
                  /\ \A l \in surv \cup L : ~Conflict(x, l.b)
      U(x) == gc[x] + B1(x \in bG)
      \* Rule 1 at every position first (a notarization lock retains its
      \* target's ancestry), then Rule 2 (a block alone reaching r) where no
      \* eligible NC is represented; both bottom-up.
      Ns(s, L) == {x \in At(s) : Ok(x, L) /\ x \in nc \cup bN}
      Rule1(s, L) == IF Cardinality(Ns(s, L)) = 1
                     THEN {[b |-> x, o |-> e, k |-> "N"] : x \in Ns(s, L)} ELSE {}
      Rule2(s, L) ==
        LET Thr == {x \in At(s) : Ok(x, L) /\ U(x) >= R}
        IN IF Ns(s, {}) = {} /\ Cardinality(Thr) = 1
           THEN {[b |-> x, o |-> e, k |-> "R"] : x \in Thr} ELSE {}
      N1 == Rule1(1, {})
      N2 == Rule1(2, N1)
      R1 == Rule2(1, N1 \cup N2)
      L1 == N1 \cup R1
      L2 == N2 \cup Rule2(2, N1 \cup N2 \cup R1)
      \* The pick, only where nothing is kept.
      Pick(s, L) ==
        LET kept   == Fst \cup AncClose({l.b : l \in surv \cup L})
            cands  == {x \in At(s) : Ok(x, L)}
            most   == {x \in cands : U(x) >= 1 /\ \A y \in cands : U(y) <= U(x)}
            byHash == IF most = {} THEN {} ELSE {MinHash(most)}
            prevK  == Held(prev) \cap cands
            pick   == CASE PICKRULE = "none" -> {}
                        [] PICKRULE = "hash" -> byHash
                        [] PICKRULE = "prev" -> IF prevK # {} THEN prevK ELSE byHash
        IN IF At(s) \cap kept # {} THEN {}
           ELSE {[b |-> x, o |-> e, k |-> "P"] : x \in pick}
      P1 == Pick(1, L1 \cup L2)
      P2 == Pick(2, L1 \cup L2 \cup P1)
      RQ == surv \cup L1 \cup L2 \cup P1 \cup P2
      HQ == AncClose({l.b : l \in RQ})
      \* Rule 3, from the final tip: a unique retained block with a verified
      \* closing-epoch NC, held by some record that is not a pick.
      Prom(x) == /\ x \in HQ /\ Cardinality(HQ \cap At(Pos(x))) = 1 /\ NC(e, x)
                 /\ \E l \in RQ : l.k # "P" /\ x \in Path(l.b)
                 /\ x \in pr \/ \E l \in L1 \cup L2 : l.k = "N" /\ l.b = x
      X1  == {x \in At(1) : Fst \cap At(1) = {} /\ Prom(x)}
      F1  == Fst \cup X1
      X2  == {x \in At(2) : Fst \cap At(2) = {} /\ Par(x) \in F1 /\ Prom(x)}
      Fin == AncClose(F1 \cup X2)
  IN [done  |-> TRUE,
      fin   |-> Fin,
      locks |-> {l \in RQ : l.b \notin Fin /\ \A y \in Fin : ~Conflict(l.b, y)}]

\* What closure e can decide: pairs of a checkpoint and the Byzantine first
\* votes its report releases.  Every selection of N - f reports of members
\* that left e, every usable Byzantine report, every manifest discharge set
\* containing the records a selected correct vote relied on (a settled first
\* vote or a final vote for a block bypassing them), and every choice of
\* verified NCs for promotion.
Outcomes(e) ==
  LET prev  == S[e-1]
      Fst   == AncClose(prev.fin)
      able  == {l \in prev.locks : Dischargeable(l)}
      Sels  == {Qs \in SUBSET (Reporters(e) \cup {byz}) : Cardinality(Qs) = NREP}
      Proms == SUBSET {x \in Blocks : NC(e, x)}
  IN UNION {
       LET Cq     == Qs \ {byz}
           gc     == [x \in Blocks |->
                        Cardinality({i \in Cq : x \in SupG(e, i) \ (NT(e, i) \cup Fst)})]
           nc     == UNION {NT(e, i) : i \in Cq}
           forced == {l \in prev.locks :
                        /\ l.k # "N"
                        /\ \E i \in Cq, s \in 1..2 :
                             LET x == fv[e][i][s] IN
                             /\ x # "-" /\ Conflict(x, l.b)
                             /\ sv[e][i][s] \/ fn[e][i][s]}
           Ds     == {D \in SUBSET able : forced \subseteq D}
           GOpts  == IF byz \in Qs THEN SUBSET {x \in Blocks : Pub(x)} ELSE {{}}
           NOpts  == IF byz \in Qs
                     THEN SUBSET {x \in Blocks : Pub(x) /\ NC(e, x)}
                     ELSE {{}}
       IN UNION {
            {<<Build(e, gc, nc, bG, bN, D, pr), bG \cup bN>> :
               bN \in {X \in NOpts : X \cap bG = {}}, D \in Ds, pr \in Proms}
            : bG \in GOpts }
       : Qs \in Sels }

Close(e) ==
  /\ e \in Closes /\ S[e-1].done /\ ~S[e].done
  /\ \E o \in Outcomes(e) :
       /\ S' = [S EXCEPT ![e] = o[1]]
       \* listing a vote in a report releases it
       /\ bf' = IF BYZ = "release" THEN [bf EXCEPT ![e] = @ \cup o[2]] ELSE bf
  /\ UNCHANGED <<fv, sv, fn, cur, inst>>

-----------------------------------------------------------------------------
Next ==
  \/ \E w \in C : \/ \E x \in Blocks : Vote(w, x)
                  \/ \E s \in 1..2 : FinalVote(w, s)
                  \/ Leave(w) \/ Catch(w) \/ Ahead(w)
  \/ \E e \in Closes : Close(e)
  \/ \E e \in Epochs, x \in Blocks : ByzRelease(e, x)

Init ==
  /\ fv   = [e \in Epochs |-> [w \in C |-> [s \in 1..2 |-> "-"]]]
  /\ sv   = [e \in Epochs |-> [w \in C |-> [s \in 1..2 |-> FALSE]]]
  /\ fn   = [e \in Epochs |-> [w \in C |-> [s \in 1..2 |-> FALSE]]]
  /\ cur  = [w \in C |-> 1]
  /\ inst = [w \in C |-> 0]
  /\ S    = [k \in 0..(E - 1) |-> IF k = 0 THEN [NoCk EXCEPT !.done = TRUE] ELSE NoCk]
  /\ bf   = [e \in Epochs |-> {}]

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* Safety.

\* No two conflicting blocks are final, counting finality through a descendant.
NoConflict == \A x, y \in Final : ~Conflict(x, y)

\* Theorem 1: each decided checkpoint holds every block with a formable
\* certificate of its epoch or an earlier one, and that block's path, as
\* final or under a record no admissible witness can discharge.
Protected(ck, x) == \/ x \in AncClose(ck.fin)
                    \/ \E l \in ck.locks : x \in Path(l.b) /\ ~Dischargeable(l)
ClosureThm1 ==
  \A k \in Closes : S[k].done =>
    \A e \in 1..k, x \in Blocks : Cert(e, x) => \A y \in Path(x) : Protected(S[k], y)

\* Sanity: a checkpoint holds at most one block per position.
OneKept == \A k \in Closes : \A s \in 1..2 : Cardinality(Held(S[k]) \cap At(s)) <= 1

\* The block a checkpoint keeps at v changes at most once.
KV(k)  == Held(S[k]) \cap At(1)
KSeq   == SelectSeq([k \in Closes |-> KV(k)], LAMBDA X : X # {})
Flips  == Cardinality({i \in 1..(Len(KSeq) - 1) : KSeq[i] # KSeq[i+1]})
FlipBound == Flips <= 1

-----------------------------------------------------------------------------
\* Recovery (BYZ = "release").

VFinal == Final \cap At(1) # {}
Quiet == \A w \in C : /\ ~\E x \in Blocks : VoteOK(w, x)
                      /\ ~\E s \in 1..2 : FinalOK(w, s)
                      /\ ~LeaveOK(w) /\ ~CatchOK(w) /\ ~AheadOK(w)
CloseIdle == \A e \in Closes :
               S[e].done \/ ~S[e-1].done \/ Cardinality(Reporters(e)) < NREP - 1
\* No correct validator and no closure can move; validator 6 may still hold
\* back signatures.
Terminal == Quiet /\ CloseIdle /\ \A w \in C : cur[w] = E

\* Run with BYZ = "release".  The epochs in READY are ready: every correct
\* validator installs the predecessor before voting there and leaves only
\* once no vote is still allowed.  READY = {E}: some block at v is final
\* when the run ends.  READY = {1, E}: the same after a ready fork epoch.
\* READY = {E-1, E}: two ready epochs at the end.
Recovered == Terminal => VFinal
\* READY = {E-2, E-1}: final by the end of the second of two ready epochs.
RecoveredEarly == (\A w \in C : cur[w] > E - 1) => VFinal
\* Sanity: correct validators and closures never get stuck before epoch E.
Progress == (Quiet /\ CloseIdle) => \A w \in C : cur[w] = E

-----------------------------------------------------------------------------
\* Action constraints for exhaustive runs.  Each stops exploring from states
\* after which the named invariant can no longer be violated; the step into
\* such a state is still taken and checked.
\* Recovered and RecoveredEarly: Final only grows.
StopRecovered == ~VFinal
\* FlipBound reads the checkpoints only, and no closure reads an epoch-E
\* vote: skip epoch-E votes, and stop once S_{E-1} is decided.
StopFlip == ~S[E-1].done /\ fv'[E] = fv[E]

-----------------------------------------------------------------------------
\* Reachability probes (expected to be VIOLATED).
ProbeEarlyFinalV == ~\E e \in 2..E, x \in At(1) : Cert(e, x) /\ ~S[e-1].done
ProbeFlip        == Flips = 0
ProbeCFinal      == "C" \notin Final
=============================================================================
