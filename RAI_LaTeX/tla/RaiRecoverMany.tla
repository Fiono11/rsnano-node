--------------------------- MODULE RaiRecoverMany ---------------------------
(***************************************************************************)
(* Many rivals, one vote each, replayed.  RaiRecover with BLOCKS rivals at *)
(* position v.                                                             *)
(*                                                                         *)
(* One account.  Position v (1) holds the conflicting blocks A_1 .. A_B    *)
(* (B = BLOCKS, <<1, i>>), all signed by the owner in epoch 1 and          *)
(* published just before the epoch-1 boundary; position v+1 (2) holds the *)
(* child C_i (<<2, i>>) of each A_i.  The parent of v is final at genesis. *)
(* N = 6, f = p = 1: correct validators 1 .. 5 and the Byzantine validator *)
(* 6.  Four voting epochs and three closures, one committee.  In epoch 1   *)
(* validator i <= B first-votes A_i before leaving the epoch and the       *)
(* others cast no vote.  The owner signs C_i only once a decided           *)
(* checkpoint keeps A_i or A_i is final: after the fork it only extends    *)
(* the kept or final tip.                                                  *)
(*                                                                         *)
(* Validator 6 holds every A_i and C_i from the moment it is signed and    *)
(* may deliver any of them to any correct validator at any time.  A        *)
(* correct validator votes the first admissible block it receives.  The    *)
(* model lets a correct validator vote any admissible block whenever it    *)
(* may vote: since validator 6 decides which block reaches whom first and  *)
(* when, this is the same set of votes (abstraction 2).                    *)
(*                                                                         *)
(* Rules, switches and abstractions 1 and 3 to 7 as in RaiRecover; with    *)
(* many rivals a record of origin o is discharged by an epoch-o exclusion  *)
(* witness for any rival of its block at v, and "hash" ties go to the      *)
(* lowest index.                                                           *)
(*                                                                         *)
(* Abstraction 2 here: every published block reaches every validator, and  *)
(* a correct validator may vote any admissible block it may vote, at any   *)
(* time it may vote, not only the first admissible one it received.        *)
(* Every block is held by a correct validator (A_i by validator i, C_i by  *)
(* the owner), so in a ready epoch delivery is complete, which the paper's *)
(* good epoch assumes.  Outside ready epochs validator 6 can deliver any   *)
(* block first; the model adds at most runs in which a correct validator   *)
(* skips the first block it received.  Adds behaviours.                    *)
(*                                                                         *)
(* NOEARLY: the paper's "no early votes at a contested position": an early *)
(* first vote at a position needs the voter's installed checkpoint to keep *)
(* nothing there.  The rule's other half (nor where the voter has seen two *)
(* blocks) is not modelled: every published block reaches everyone, so it  *)
(* would forbid every early vote at v.  Leaving it out adds behaviours.    *)
(*                                                                         *)
(* OWNER: "policy" as above.  "any": the owner signs every A_i and C_i at   *)
(* the start and epoch-1 votes are unrestricted (any admissible block),    *)
(* for safety against a forking owner.                                     *)
(*                                                                         *)
(* R2ORDER: the order of Rule 2 across positions, which a draft had left   *)
(* open.  "up": from v to v+1, each against the records already chosen.   *)
(* "down": from v+1 to v; a block Rule 2 retains keeps its ancestry, and   *)
(* Rule 2 does not run at a position already held.  The picks run bottom-  *)
(* up after Rule 2 in both.                                                *)
(*                                                                         *)
(* READY: epochs in which every correct validator installs the             *)
(* predecessor before voting and leaves only when no vote is still         *)
(* allowed.  The second half is the paper's good epoch; without it a       *)
(* correct validator could leave without voting and no epoch would         *)
(* finalize anything.  Validator 6 may still withhold votes.               *)
(*                                                                         *)
(* Invariants: NoConflict, ClosureThm1, OneKept, FlipBound, KeptOrFinal,   *)
(* RecoveredMany and RecoveredEarly (with READY), Progress, probes.        *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS BLOCKS, E, IMPLIED, SETTLED, PICKRULE, ATTACH, RENOTARIZE, NOEARLY, BYZ,
          READY, OWNER, R2ORDER

ASSUME /\ BLOCKS \in {2, 3} /\ E \in {2, 3, 4}
       /\ IMPLIED \in {"off", "all", "committed"}
       /\ SETTLED \in BOOLEAN /\ RENOTARIZE \in BOOLEAN /\ NOEARLY \in BOOLEAN
       /\ PICKRULE \in {"none", "hash", "prev"}
       /\ ATTACH \in {"strict", "permissive"}
       /\ BYZ \in {"all", "release"} /\ READY \subseteq 1..E
       /\ OWNER \in {"policy", "any"} /\ R2ORDER \in {"up", "down"}

C      == 1..5
byz    == 6
Epochs == 1..E
Closes == 1..(E - 1)
Q      == 4      \* N - f - p
R      == 3      \* f + p + 1
FAST   == 5      \* N - p
NREP   == 5      \* N - f

Idx       == 1..BLOCKS
A(i)      == <<1, i>>
Blocks    == {<<s, i>> : s \in 1..2, i \in Idx}
NONE      == <<0, 0>>
Pos(x)    == x[1]
At(s)     == {x \in Blocks : Pos(x) = s}
Par(x)    == <<1, x[2]>>                            \* children only
Base(x)   == <<1, x[2]>>                            \* x's block at v
Path(x)   == IF Pos(x) = 1 THEN {x} ELSE {x, Par(x)}
AncClose(X) == UNION {Path(x) : x \in X}
\* Every fork here diverges at v.
Conflict(x, y) == x[2] # y[2]
\* Hash order for ties: the lowest index.
MinHash(X) == CHOOSE x \in X : \A y \in X : x[2] <= y[2]
\* The epoch-1 role of a correct validator: the block it votes, if any.
Role(w) == IF w <= BLOCKS THEN w ELSE 0

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
Implies(e, w) == /\ IMPLIED # "off" /\ fv[e][w][2] # NONE
                 /\ IMPLIED = "all" \/ sv[e][w][2] \/ fn[e][w][2]
Sup(e, w, s) ==
  IF s = 2 THEN {fv[e][w][2]} \ {NONE}
  ELSE ({fv[e][w][1]} \ {NONE}) \cup (IF Implies(e, w) THEN {Par(fv[e][w][2])} ELSE {})
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

\* Symmetry and dead history, as in RaiRecover.  Validators are
\* interchangeable once they have left epoch 1; before, the view keeps the
\* block each must vote there.  The blocks are not interchangeable (the
\* tie-break orders them).
DeadW(e, w) == e < E /\ S[e].done /\ cur[w] > e /\ inst[w] >= e
Agg(e) == LET dw == {w \in C : DeadW(e, w)} IN
  [x \in Blocks |->
     <<Cardinality({w \in dw : x \in Sup(e, w, Pos(x))}),
       Cardinality({w \in dw : Late(e, w, x)}),
       Cardinality({w \in dw : fv[e][w][Pos(x)] = x /\ sv[e][w][Pos(x)]}),
       Cardinality({w \in dw : fv[e][w][Pos(x)] = x /\ fn[e][w][Pos(x)]})>>]
Local(w) == <<cur[w], inst[w], IF cur[w] = 1 THEN Role(w) ELSE 0,
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
Dischargeable(l) == /\ l.k # "N"
                    /\ \E y \in At(1) : Conflict(y, l.b) /\ XW(l.o, y)

Admit(x, ck) ==
  /\ FinOK(x, ck) /\ x \notin AncClose(ck.fin)
  /\ \A l \in ck.locks : Conflict(x, l.b) => ATTACH = "permissive" /\ Witness(l, x)

\* The owner signs C_i once a decided checkpoint keeps A_i or A_i is final.
Pub(x) == \/ Pos(x) = 1 \/ OWNER = "any"
          \/ Par(x) \in Final
          \/ \E k \in Closes : S[k].done /\ Par(x) \in Held(S[k])

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
  IN /\ fv[e][w][s] = NONE
     \* epoch 1: validator i votes A_i, the others nothing
     /\ (e = 1 /\ OWNER = "policy") => x = A(w)
     /\ e \in READY => ~early
     \* no early votes at a position the installed checkpoint keeps
     /\ (NOEARLY /\ early) => Held(ck) \cap At(s) = {}
     /\ Pub(x)
     /\ Admit(x, ck)
     /\ s = 2 => ParentOK(x, e, ck, ~early)
     \* one support per instance
     /\ s = 1 => Sup(e, w, 1) \subseteq {x}
     /\ imp => fv[e][w][1] \in {NONE, Par(x)}
     \* no switching across a boundary while S_{e-1} is unknown
     /\ early => /\ Sup(e - 1, w, s) \subseteq {x}
                 /\ imp => Sup(e - 1, w, 1) \subseteq {Par(x)}

FinalOK(w, s) ==
  LET e == cur[w]  x == fv[e][w][s] IN
  /\ x # NONE /\ ~fn[e][w][s]
  /\ NC(e, x)
  /\ (s = 2 /\ IMPLIED # "off") => fv[e][w][1] \in {NONE, Par(x)}
  /\ IF Early(w)
     THEN EarlyFinalOK(e, x, S[inst[w]])
     ELSE /\ Admit(x, S[e - 1])
          /\ s = 2 => ParentOK(x, e, S[e - 1], TRUE)

\* Validator i casts its epoch-1 vote before it leaves epoch 1.
Voted1(w) == (OWNER = "policy" /\ cur[w] = 1 /\ w <= BLOCKS) => fv[1][w][1] # NONE
\* In a ready epoch a validator leaves only once no vote is still allowed.
Done(w) == /\ ~\E x \in Blocks : VoteOK(w, x)
           /\ ~\E s \in 1..2 : FinalOK(w, s)
LeaveOK(w) == /\ cur[w] < E /\ inst[w] >= cur[w] - 1 /\ Voted1(w)
              /\ cur[w] \in READY => Done(w)
CatchOK(w) == inst[w] = cur[w] - 2 /\ S[cur[w] - 1].done
AheadOK(w) == /\ inst[w] = cur[w] - 1 /\ cur[w] < E /\ S[cur[w]].done /\ Voted1(w)
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
          \/ /\ Pos(x) = 1 /\ IMPLIED # "off" /\ fv[e][i][2] # NONE
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
      R1u == Rule2(1, N1 \cup N2)
      R2u == Rule2(2, N1 \cup N2 \cup R1u)
      R2d == Rule2(2, N1 \cup N2)
      R1d == IF At(1) \cap AncClose({l.b : l \in N2 \cup R2d}) # {} THEN {}
             ELSE Rule2(1, N1 \cup N2 \cup R2d)
      L1 == N1 \cup (IF R2ORDER = "up" THEN R1u ELSE R1d)
      L2 == N2 \cup (IF R2ORDER = "up" THEN R2u ELSE R2d)
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

\* What closure e can decide, as in RaiRecover.
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
                             /\ x # NONE /\ Conflict(x, l.b)
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
  /\ fv   = [e \in Epochs |-> [w \in C |-> [s \in 1..2 |-> NONE]]]
  /\ sv   = [e \in Epochs |-> [w \in C |-> [s \in 1..2 |-> FALSE]]]
  /\ fn   = [e \in Epochs |-> [w \in C |-> [s \in 1..2 |-> FALSE]]]
  /\ cur  = [w \in C |-> 1]
  /\ inst = [w \in C |-> 0]
  /\ S    = [k \in 0..(E - 1) |-> IF k = 0 THEN [NoCk EXCEPT !.done = TRUE] ELSE NoCk]
  /\ bf   = [e \in Epochs |-> {}]

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* Safety.

NoConflict == \A x, y \in Final : ~Conflict(x, y)

Protected(ck, x) == \/ x \in AncClose(ck.fin)
                    \/ \E l \in ck.locks : x \in Path(l.b) /\ ~Dischargeable(l)
ClosureThm1 ==
  \A k \in Closes : S[k].done =>
    \A e \in 1..k, x \in Blocks : Cert(e, x) => \A y \in Path(x) : Protected(S[k], y)

OneKept == \A k \in Closes : \A s \in 1..2 : Cardinality(Held(S[k]) \cap At(s)) <= 1

\* The block a checkpoint keeps at v changes at most once.
KV(k)  == Held(S[k]) \cap At(1)
KSeq   == SelectSeq([k \in Closes |-> KV(k)], LAMBDA X : X # {})
Flips  == Cardinality({i \in 1..(Len(KSeq) - 1) : KSeq[i] # KSeq[i+1]})
FlipBound == Flips <= 1

\* With a pick, every decided checkpoint keeps or finalizes one block at v.
KeptOrFinal == PICKRULE # "none" =>
                 \A k \in Closes : S[k].done => Cardinality(KV(k)) = 1

-----------------------------------------------------------------------------
\* Recovery (BYZ = "release").

VFinal == Final \cap At(1) # {}
Quiet == \A w \in C : /\ ~\E x \in Blocks : VoteOK(w, x)
                      /\ ~\E s \in 1..2 : FinalOK(w, s)
                      /\ ~LeaveOK(w) /\ ~CatchOK(w) /\ ~AheadOK(w)
CloseIdle == \A e \in Closes :
               S[e].done \/ ~S[e-1].done \/ Cardinality(Reporters(e)) < NREP - 1
Terminal == Quiet /\ CloseIdle /\ \A w \in C : cur[w] = E

\* READY = {3, 4}: some A_i is final when the run ends.
RecoveredMany == Terminal => VFinal
\* READY = {2, 3}: some A_i is final once every correct validator has left
\* epoch 3, i.e. by S_3, two checkpoints after S_1 (which keeps a block
\* under KeptOrFinal).
RecoveredEarly == (\A w \in C : cur[w] > E - 1) => VFinal
Progress == (Quiet /\ CloseIdle) => \A w \in C : cur[w] = E

-----------------------------------------------------------------------------
\* Action constraints, as in RaiRecover.
StopRecovered == ~VFinal
StopFlip == ~S[E-1].done /\ fv'[E] = fv[E]

-----------------------------------------------------------------------------
\* Reachability probes (expected to be VIOLATED).
\* A decided checkpoint keeps nothing at v.
ProbeNothing     == ~\E k \in Closes : S[k].done /\ KV(k) = {}
\* Some A_i is final by an epoch-e certificate before S_{e-1} is decided.
ProbeEarlyFinalV == ~\E e \in 2..E, x \in At(1) : Cert(e, x) /\ ~S[e-1].done
\* A checkpoint keeps A_j while a rival A_i has an epoch-1 exclusion witness.
ProbeWitnessKeep == ~\E k \in Closes, i, j \in Idx :
                       i # j /\ S[k].done /\ A(j) \in Held(S[k]) /\ XW(1, A(i))
\* A settled first vote supports a block at v other than the one the
\* voter's installed checkpoint keeps there (a replayed rival regains a
\* settled vote).
ProbeRegain == ~\E e \in 2..E, w \in C, s \in 1..2 :
                  LET x == fv[e][w][s] IN
                  /\ x # NONE /\ sv[e][w][s]
                  /\ \E y \in KV(e - 1) : y # Base(x)
ProbeFlip == Flips = 0
=============================================================================
