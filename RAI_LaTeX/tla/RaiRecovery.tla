---------------------------- MODULE RaiRecovery ----------------------------
(***************************************************************************)
(* Owner recovery on top of a recovery-locked block: two positions, one    *)
(* closure, installation and settled votes.  N = 3f + 2p + 1, run with     *)
(* N = 4 (f = 1, p = 0) and N = 6 (f = p = 1).                             *)
(*                                                                         *)
(* Position 1 holds the conflicting blocks X and Y.  Position 2 holds a    *)
(* child of each: CX on X and CY on Y.  Epoch 1 is voted by K1 at position *)
(* 1 and closed by K1 (checkpoint S1, Rules 1-3 with origin-1 records).    *)
(* Epoch 2 is voted by K2 at both positions.  A member of K2 may vote in   *)
(* epoch 2 before installing S1 (an early vote, with a late notarization   *)
(* in epoch 1 if it is in K1 and supported nothing else there) or after    *)
(* installing S1 (a settled vote).  The early-vote rule, Fix B, the NC     *)
(* split, matching-origin discharge, late votes as evidence and the        *)
(* prefix-wide overlap guard with a fresh child witness are built in.      *)
(*                                                                         *)
(* EXEMPT = TRUE : a block retained by S1 under a recovery record alone    *)
(*   may always be extended in epoch 2 (the retained-tip exemption).       *)
(* EXEMPT = FALSE: such a block is a complete parent in epoch 2 only once  *)
(*   it holds an epoch-2 NC of first votes (re-notarization rule).  Final  *)
(*   parents need nothing new; S1 never carries a notarization lock here,  *)
(*   since with one position Rule 3 promotes every represented NC.         *)
(*                                                                         *)
(* A block is final when its certificate can be formed from released       *)
(* signatures and it is valid: through the overlap bundle (epoch-1         *)
(* exclusion witness and epoch-2 NC for the block and its unresolved       *)
(* parent) or by rechecking against S1.  A final child finalizes its       *)
(* parent.  The Byzantine validator's signatures count for every block,    *)
(* vote kind and epoch.                                                    *)
(*                                                                         *)
(* Abstractions: children are published after the first boundary, so      *)
(* nobody votes position 2 in epoch 1; a validator that observes the       *)
(* needed certificates final-votes in the same step; finality is global;   *)
(* closure 2 is not modelled, so safety is checked within epoch 2 and      *)
(* against S1 (NoConflict).                                                *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS K1, K2, byz, N, f, p, EXEMPT, MAXKEEP

C        == K1 \cup K2
Blocks   == {"X", "Y"}
Kids     == {"CX", "CY"}
Par(c)   == IF c = "CX" THEN "X" ELSE "Y"
Other(b) == IF b = "X" THEN "Y" ELSE "X"
Q        == N - f - p
R        == f + p + 1
FAST     == N - p
NREP     == N - f
NoRep    == [T |-> [b \in Blocks |-> "-"], G |-> {}]

VARIABLES fv1,  \* epoch-1 first vote at position 1
          fn1,  \* epoch-1 final vote released
          ph,   \* "e1" in epoch 1; "e2" left epoch 1, S1 not installed;
                \* "in" S1 installed
          fa,   \* epoch-2 first vote at position 1
          ea,   \* ... cast before installing S1
          na,   \* epoch-2 final vote at position 1 released
          fc,   \* epoch-2 first vote at position 2
          ec,   \* ... cast before installing S1
          nc,   \* epoch-2 final vote at position 2 released
          S1
vars == <<fv1, fn1, ph, fa, ea, na, fc, ec, nc, S1>>

Local(v) == <<v \in K1, v \in K2, fv1[v], fn1[v], ph[v],
              fa[v], ea[v], na[v], fc[v], ec[v], nc[v]>>
View == <<[t \in {Local(v) : v \in C} |->
             Cardinality({v \in C : Local(v) = t})], S1>>

Cnt(P(_)) == Cardinality({v \in C : P(v)})

\* Epoch 1.  Late notarizations come with early epoch-2 first votes.
FV1(b)   == Cardinality({v \in K1 : fv1[v] = b})
LT1(b)   == Cardinality({v \in K1 \cap K2 : ea[v] /\ fv1[v] = "-" /\ fa[v] = b})
LT1c(c)  == Cardinality({v \in K1 \cap K2 : ec[v] /\ fc[v] = c})
NC1(b)   == FV1(b) + 1 >= Q
XW1(b)   == FV1(b) + LT1(b) + 1 >= Q
XW1c(c)  == LT1c(c) + 1 >= Q
Cert1(b) == \/ FV1(b) + 1 >= FAST
            \/ Cardinality({v \in K1 : fv1[v] = b /\ fn1[v]}) + 1 >= Q

\* Epoch 2.  Fast certificates count settled first votes only.
NC2(b)  == Cardinality({v \in K2 : fa[v] = b}) + 1 >= Q
FF2(b)  == Cardinality({v \in K2 : fa[v] = b /\ ~ea[v]}) + 1 >= FAST
FC2(b)  == Cardinality({v \in K2 : fa[v] = b /\ na[v]}) + 1 >= Q
NC2c(c) == Cardinality({v \in K2 : fc[v] = c}) + 1 >= Q
FF2c(c) == Cardinality({v \in K2 : fc[v] = c /\ ~ec[v]}) + 1 >= FAST
FC2c(c) == Cardinality({v \in K2 : fc[v] = c /\ nc[v]}) + 1 >= Q

\* Rechecking against the installed S1 (records have origin 1).
Admissible(b) == /\ S1.done /\ S1.fin \subseteq {b}
                 /\ \A l \in S1.locks : l.b = Other(b) => XW1(b)
Retained(b)   == \E l \in S1.locks : l.b = b

\* May a child be built on its parent in epoch 2 after installing S1?
ParentOK(c) == LET a == Par(c) IN
               \/ a \in S1.fin
               \/ Admissible(a) /\ NC2(a)          \* complete epoch-2 parent
               \/ EXEMPT /\ Retained(a)            \* retained-tip exemption
\* Before installing S1: a complete epoch-2 parent or a finalized epoch-1 one.
ParentEarly(c) == Cert1(Par(c)) \/ NC2(Par(c))

\* The overlap bundle, prefix-wide with a fresh child witness.
OverlapA(b) == XW1(b) /\ NC2(b)
OverlapC(c) == /\ XW1c(c) /\ NC2c(c)
               /\ Cert1(Par(c)) \/ OverlapA(Par(c))

FinalA(b) == (FF2(b) \/ FC2(b)) /\ (OverlapA(b) \/ Admissible(b))
FinalC(c) == (FF2c(c) \/ FC2c(c)) /\ (OverlapC(c) \/ ParentOK(c))

NoCk == [done |-> FALSE, fin |-> {}, locks |-> {}]

Init ==
  /\ fv1 = [v \in C |-> "-"] /\ fn1 = [v \in C |-> FALSE]
  /\ ph = [v \in C |-> IF v \in K1 THEN "e1" ELSE "e2"]
  /\ fa = [v \in C |-> "-"] /\ ea = [v \in C |-> FALSE] /\ na = [v \in C |-> FALSE]
  /\ fc = [v \in C |-> "-"] /\ ec = [v \in C |-> FALSE] /\ nc = [v \in C |-> FALSE]
  /\ S1 = NoCk

Vote1(v, b) ==
  /\ v \in K1 /\ ph[v] = "e1" /\ fv1[v] = "-"
  /\ fv1' = [fv1 EXCEPT ![v] = b]
  /\ UNCHANGED <<fn1, ph, fa, ea, na, fc, ec, nc, S1>>

Final1(v) ==
  /\ v \in K1 /\ ph[v] = "e1" /\ fv1[v] # "-" /\ ~fn1[v] /\ NC1(fv1[v])
  /\ fn1' = [fn1 EXCEPT ![v] = TRUE]
  /\ UNCHANGED <<fv1, ph, fa, ea, na, fc, ec, nc, S1>>

Leave1(v) ==
  /\ v \in K1 /\ ph[v] = "e1"
  /\ ph' = [ph EXCEPT ![v] = "e2"]
  /\ UNCHANGED <<fv1, fn1, fa, ea, na, fc, ec, nc, S1>>

Install(v) ==
  /\ v \in K2 /\ S1.done /\ ph[v] \in {"e1", "e2"}
  /\ ph' = [ph EXCEPT ![v] = "in"]
  /\ UNCHANGED <<fv1, fn1, fa, ea, na, fc, ec, nc, S1>>

\* Epoch-2 first vote at position 1.
VoteA(v, b) ==
  /\ v \in K2 /\ fa[v] = "-"
  /\ \/ /\ ph[v] = "e2"                       \* early: cross-epoch guard
        /\ fv1[v] \in {"-", b}
        /\ ea' = [ea EXCEPT ![v] = TRUE]
     \/ /\ ph[v] = "in"
        /\ S1.fin = {} /\ Admissible(b)
        /\ UNCHANGED ea
  /\ fa' = [fa EXCEPT ![v] = b]
  /\ UNCHANGED <<fv1, fn1, ph, na, fc, ec, nc, S1>>

\* Epoch-2 first vote at position 2.
VoteC(v, c) ==
  /\ v \in K2 /\ fc[v] = "-"
  /\ \/ /\ ph[v] = "e2" /\ ParentEarly(c)
        /\ ec' = [ec EXCEPT ![v] = TRUE]
     \/ /\ ph[v] = "in" /\ ParentOK(c)
        /\ UNCHANGED ec
  /\ fc' = [fc EXCEPT ![v] = c]
  /\ UNCHANGED <<fv1, fn1, ph, fa, ea, na, nc, S1>>

\* Epoch-2 final votes; an early one needs the overlap bundle (early-vote rule).
FinalVoteA(v) ==
  /\ v \in K2 /\ fa[v] # "-" /\ ~na[v] /\ NC2(fa[v])
  /\ \/ ph[v] = "e2" /\ XW1(fa[v])
     \/ ph[v] = "in" /\ Admissible(fa[v])
  /\ na' = [na EXCEPT ![v] = TRUE]
  /\ UNCHANGED <<fv1, fn1, ph, fa, ea, fc, ec, nc, S1>>

FinalVoteC(v) ==
  /\ v \in K2 /\ fc[v] # "-" /\ ~nc[v] /\ NC2c(fc[v])
  /\ \/ ph[v] = "e2" /\ OverlapC(fc[v])
     \/ ph[v] = "in" /\ ParentOK(fc[v])
  /\ nc' = [nc EXCEPT ![v] = TRUE]
  /\ UNCHANGED <<fv1, fn1, ph, fa, ea, na, fc, ec, S1>>

-----------------------------------------------------------------------------
\* Closure 1, as in RaiHandoff.
Rep1(i) ==
  LET T == [b \in Blocks |-> IF fv1[i] = b /\ fn1[i] THEN "N" ELSE "-"]
  IN [T |-> T, G |-> {b \in Blocks : fv1[i] = b /\ T[b] = "-"}]

ByzReps1 ==
  {r \in [T : [Blocks -> {"-", "N", "F"}], G : SUBSET Blocks] :
     /\ \A b \in Blocks : /\ r.T[b] = "F" => Cert1(b)
                          /\ r.T[b] = "N" => NC1(b)
     /\ \A b \in r.G : r.T[b] = "-"}

Close1 ==
  /\ ~S1.done
  /\ \E Qs \in SUBSET (K1 \cup {byz}) :
       /\ Cardinality(Qs) = NREP
       /\ \A v \in Qs \ {byz} : ph[v] # "e1"
       /\ \E brep \in (IF byz \in Qs THEN ByzReps1 ELSE {NoRep}) :
            LET Rp(i) == IF i = byz THEN brep ELSE Rep1(i)
                Fs    == {b \in Blocks : \E i \in Qs : Rp(i).T[b] = "F"}
                Ns    == {b \in Blocks : \E i \in Qs : Rp(i).T[b] = "N"}
                U(b)  == {i \in Qs : b \in Rp(i).G}
                Thr   == {b \in Blocks : Cardinality(U(b)) >= R}
                Most  == {b \in Blocks : Cardinality(U(b)) >= 1 /\
                            \A c \in Blocks : Cardinality(U(c)) <= Cardinality(U(b))}
                Pick  == IF "X" \in Most THEN "X" ELSE "Y"   \* ties by hash
                new   == IF MAXKEEP
                         THEN IF Most = {} THEN {} ELSE {[b |-> Pick, o |-> 1]}
                         ELSE IF Cardinality(Thr) = 1
                              THEN {[b |-> CHOOSE b \in Thr : TRUE, o |-> 1]} ELSE {}
            IN S1' = IF Fs # {} THEN [done |-> TRUE, fin |-> Fs, locks |-> {}]
                     ELSE IF Ns # {} THEN [done |-> TRUE, fin |-> Ns, locks |-> {}]
                     ELSE [done |-> TRUE, fin |-> {}, locks |-> new]
  /\ UNCHANGED <<fv1, fn1, ph, fa, ea, na, fc, ec, nc>>

Next ==
  \/ \E v \in C, b \in Blocks : Vote1(v, b) \/ VoteA(v, b)
  \/ \E v \in C, c \in Kids : VoteC(v, c)
  \/ \E v \in C : \/ Final1(v) \/ Leave1(v) \/ Install(v)
                  \/ FinalVoteA(v) \/ FinalVoteC(v)
  \/ Close1

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
FinalKids == {c \in Kids : FinalC(c)}
Finals    == {b \in Blocks : Cert1(b) \/ FinalA(b)} \cup S1.fin
               \cup {Par(c) : c \in FinalKids}

\* No two conflicting blocks are final, counting finality through a child.
NoConflict == Cardinality(Finals) <= 1

\* Reachability probes (expected to be VIOLATED).
\* The precondition of the bug: X final while S1 keeps Y for recovery.
ProbeFinalVsLock    == ~(\E b \in Blocks : FinalA(b) /\ Retained(Other(b)))
\* Owner recovery: a child finalizes a block S1 kept under a recovery record.
ProbeRecovery       == ~(\E c \in FinalKids : Retained(Par(c)))
\* Owner recovery when somebody had already early-voted the dropped side.
ProbeRecoveryContested ==
  ~(\E c \in FinalKids : Retained(Par(c)) /\
       \E v \in K2 : fa[v] = Other(Par(c)))
=============================================================================
