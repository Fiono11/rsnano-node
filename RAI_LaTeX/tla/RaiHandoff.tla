----------------------------- MODULE RaiHandoff -----------------------------
(***************************************************************************)
(* RAI handoff with early voting, two closures and a changing committee.   *)
(* One account position, two conflicting blocks, N = 6, f = p = 1.         *)
(*                                                                         *)
(* Epoch 1 is voted by K1 and closed by K1 (checkpoint S1).  Epoch 2 is    *)
(* voted by K2 and closed by K2 (checkpoint S2).  K1 and K2 are the        *)
(* correct members; the Byzantine validator sits in both committees.  A    *)
(* member of K2 may first-vote in epoch 2 before installing S1 (an early   *)
(* vote); if it is also in K1 and supported nothing else in epoch 1, that  *)
(* vote comes with a late notarization in epoch 1.                         *)
(*                                                                         *)
(* Fix B, the NC split, matching-origin discharge and late votes as        *)
(* evidence are all built in.                                              *)
(*                                                                         *)
(* RULE = FALSE: the paper's certificates.  A fast certificate counts all  *)
(*   first votes; a validator final-votes on its epoch-2 NC alone.         *)
(* RULE = TRUE : no fast path on early votes.  A fast certificate counts   *)
(*   only first votes cast after the signer installed S1, and an early     *)
(*   final vote is released only by a validator that holds the epoch-1     *)
(*   exclusion witness for the block.                                      *)
(*                                                                         *)
(* A block is final when its certificate can be formed from released       *)
(* signatures and it is valid against S1: either through the overlap       *)
(* bundle (epoch-1 exclusion witness plus epoch-2 NC) or by rechecking     *)
(* against S1 with discharge.  The Byzantine validator's signatures always *)
(* count as available for formability, but the builder of S2 only has a    *)
(* discharge witness if a selected correct reporter relied on it; otherwise*)
(* whether the manifest carries it is the adversary's choice.  That is how *)
(* a withheld signature is modelled.                                       *)
(*                                                                         *)
(* Abstractions: a validator that observes its NC final-votes in the same  *)
(* step; correct reports carry no F tag for an observed certificate (that  *)
(* path is covered by RaiClose and RaiOverlap); a claim that conflicts     *)
(* with a surviving record is ignored rather than making the selection     *)
(* invalid, which only adds checkpoints; epoch 3 is not modelled, so       *)
(* safety after S2 is checked as Theorem 6.4 for S2.                       *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS K1, K2, byz, RULE

C       == K1 \cup K2
Blocks  == {"X", "Y"}
Other(b) == IF b = "X" THEN "Y" ELSE "X"
Q       == 4
R       == 3
FAST    == 5
NREP    == 5
Tags    == {"-", "R", "N", "F"}
NoRep   == [T |-> [b \in Blocks |-> "-"], G |-> {}]

VARIABLES fv1,    \* epoch-1 first vote
          fn1,    \* epoch-1 notarization recorded and final vote released
          ph,     \* "e1" in epoch 1; "e2" left epoch 1, S1 not installed;
                  \* "in" S1 installed; "out" left epoch 2 (report frozen)
          fv2,    \* epoch-2 first vote
          early,  \* that vote was cast before installing S1
          fn2,    \* "no"; "plain" final vote on the NC alone; "wit" final
                  \* vote released while holding the witness / after recheck
          S1, S2
vars == <<fv1, fn1, ph, fv2, early, fn2, S1, S2>>

Local(v) == <<v \in K1, v \in K2, fv1[v], fn1[v], ph[v], fv2[v], early[v], fn2[v]>>
View == <<[t \in {Local(v) : v \in C} |->
             Cardinality({v \in C : Local(v) = t})], S1, S2>>

Num(P(_), D) == Cardinality({v \in D : P(v)})

\* Epoch 1.
FV1(b)  == Cardinality({v \in K1 : fv1[v] = b})
LT1(b)  == Cardinality({v \in K1 \cap K2 :
                          early[v] /\ fv1[v] = "-" /\ fv2[v] = b})
NC1(b)  == FV1(b) + 1 >= Q
XW1(b)  == FV1(b) + LT1(b) + 1 >= Q
Cert1(b) == \/ FV1(b) + 1 >= FAST
            \/ Cardinality({v \in K1 : fv1[v] = b /\ fn1[v]}) + 1 >= Q

\* Epoch 2.
FV2(b)   == Cardinality({v \in K2 : fv2[v] = b})
FV2p(b)  == Cardinality({v \in K2 : fv2[v] = b /\ ~early[v]})
NC2(b)   == FV2(b) + 1 >= Q
FF2(b)   == (IF RULE THEN FV2p(b) ELSE FV2(b)) + 1 >= FAST
FC2(b)   == Cardinality({v \in K2 : fv2[v] = b /\ fn2[v] # "no"}) + 1 >= Q

\* Rechecking b against the installed S1 (records in S1 have origin 1).
PostValid(b) == /\ S1.done /\ S1.fin \subseteq {b}
                /\ \A l \in S1.locks : l.b = Other(b) => XW1(b)
OverlapOK(b) == XW1(b) /\ NC2(b)
Final2(b)    == (FF2(b) \/ FC2(b)) /\ (OverlapOK(b) \/ PostValid(b))

NoCk == [done |-> FALSE, fin |-> {}, locks |-> {}]

Init ==
  /\ fv1 = [v \in C |-> "-"] /\ fn1 = [v \in C |-> FALSE]
  /\ ph = [v \in C |-> IF v \in K1 THEN "e1" ELSE "e2"]
  /\ fv2 = [v \in C |-> "-"] /\ early = [v \in C |-> FALSE]
  /\ fn2 = [v \in C |-> "no"]
  /\ S1 = NoCk /\ S2 = NoCk

Vote1(v, b) ==
  /\ v \in K1 /\ ph[v] = "e1" /\ fv1[v] = "-"
  /\ fv1' = [fv1 EXCEPT ![v] = b]
  /\ UNCHANGED <<fn1, ph, fv2, early, fn2, S1, S2>>

Final1(v) ==
  /\ v \in K1 /\ ph[v] = "e1" /\ fv1[v] # "-" /\ ~fn1[v] /\ NC1(fv1[v])
  /\ fn1' = [fn1 EXCEPT ![v] = TRUE]
  /\ UNCHANGED <<fv1, ph, fv2, early, fn2, S1, S2>>

Leave1(v) ==
  /\ v \in K1 /\ ph[v] = "e1"
  /\ ph' = [ph EXCEPT ![v] = "e2"]
  /\ UNCHANGED <<fv1, fn1, fv2, early, fn2, S1, S2>>

Install(v) ==
  /\ v \in K2 /\ S1.done /\ ph[v] \in {"e1", "e2"}
  /\ ph' = [ph EXCEPT ![v] = "in"]
  /\ UNCHANGED <<fv1, fn1, fv2, early, fn2, S1, S2>>

Vote2(v, b) ==
  /\ v \in K2 /\ fv2[v] = "-"
  /\ \/ /\ ph[v] = "e2"                       \* early: cross-epoch guard
        /\ fv1[v] \in {"-", b}
        /\ early' = [early EXCEPT ![v] = TRUE]
     \/ /\ ph[v] = "in"                       \* after installing S1
        /\ S1.fin = {} /\ PostValid(b)
        /\ UNCHANGED early
  /\ fv2' = [fv2 EXCEPT ![v] = b]
  /\ UNCHANGED <<fv1, fn1, ph, fn2, S1, S2>>

FinalVote2(v) ==
  /\ v \in K2 /\ fv2[v] # "-" /\ fn2[v] = "no" /\ NC2(fv2[v])
  /\ \/ /\ ph[v] = "e2"
        /\ IF RULE THEN /\ XW1(fv2[v])
                        /\ fn2' = [fn2 EXCEPT ![v] = "wit"]
                   ELSE fn2' = [fn2 EXCEPT ![v] = "plain"]
     \/ /\ ph[v] = "in" /\ PostValid(fv2[v])
        /\ fn2' = [fn2 EXCEPT ![v] = "wit"]
  /\ UNCHANGED <<fv1, fn1, ph, fv2, early, S1, S2>>

Leave2(v) ==
  /\ v \in K2 /\ ph[v] = "in" /\ ~S2.done
  /\ ph' = [ph EXCEPT ![v] = "out"]
  /\ UNCHANGED <<fv1, fn1, fv2, early, fn2, S1, S2>>

-----------------------------------------------------------------------------
\* Closure 1 (no inherited locks).
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
                new   == IF Cardinality(Thr) = 1
                         THEN {[b |-> CHOOSE b \in Thr : TRUE, o |-> 1]} ELSE {}
            IN S1' = IF Fs # {} THEN [done |-> TRUE, fin |-> Fs, locks |-> {}]
                     ELSE IF Ns # {} THEN [done |-> TRUE, fin |-> Ns, locks |-> {}]
                     ELSE [done |-> TRUE, fin |-> {}, locks |-> new]
  /\ UNCHANGED <<fv1, fn1, ph, fv2, early, fn2, S2>>

\* Closure 2, with Fix B and per-record discharge.
Rep2(i) ==
  LET T == [b \in Blocks |->
              IF b \in S1.fin                        THEN "F"
              ELSE IF fv2[i] = b /\ fn2[i] # "no"    THEN "N"
              ELSE IF \E l \in S1.locks : l.b = b    THEN "R"
              ELSE "-"]
  IN [T |-> T, G |-> {b \in Blocks : fv2[i] = b /\ T[b] \notin {"N", "F"}}]

ByzReps2 ==
  {r \in [T : [Blocks -> Tags], G : SUBSET Blocks] :
     /\ \A b \in Blocks : /\ r.T[b] = "F" => b \in S1.fin
                          /\ r.T[b] = "N" => NC2(b)
                          /\ r.T[b] = "R" => \E l \in S1.locks : l.b = b
     /\ \A b \in r.G : r.T[b] \notin {"N", "F"}}

\* A correct reporter's vote for b brings the discharge witness with it if
\* the vote was cast after installing S1, or its final vote was released
\* while holding the witness.
Relied(i, b) == fv2[i] = b /\ (~early[i] \/ fn2[i] = "wit")

Close2 ==
  /\ S1.done /\ ~S2.done
  /\ \E Qs \in SUBSET (K2 \cup {byz}) :
       /\ Cardinality(Qs) = NREP
       /\ \A v \in Qs \ {byz} : ph[v] = "out"
       /\ \E brep \in (IF byz \in Qs THEN ByzReps2 ELSE {NoRep}) :
            LET Rp(i)  == IF i = byz THEN brep ELSE Rep2(i)
                forced == {l \in S1.locks :
                             \E i \in Qs \ {byz} : Relied(i, Other(l.b))}
                able   == {l \in S1.locks : XW1(Other(l.b))}
            IN \E D \in SUBSET able :
                 /\ forced \subseteq D
                 /\ LET surv    == S1.locks \ D
                        Elig(b) == \A l \in surv : l.b # Other(b)
                        Ns   == {b \in Blocks :
                                   Elig(b) /\ \E i \in Qs : Rp(i).T[b] = "N"}
                        U(b) == IF Elig(b) THEN {i \in Qs : b \in Rp(i).G} ELSE {}
                        Thr  == {b \in Blocks : Cardinality(U(b)) >= R}
                        new  == IF Cardinality(Thr) = 1
                                THEN {[b |-> CHOOSE b \in Thr : TRUE, o |-> 2]}
                                ELSE {}
                    IN S2' = IF S1.fin # {}
                             THEN [done |-> TRUE, fin |-> S1.fin, locks |-> {}]
                             ELSE IF Ns # {}
                             THEN [done |-> TRUE, fin |-> Ns, locks |-> {}]
                             ELSE [done |-> TRUE, fin |-> {}, locks |-> surv \cup new]
  /\ UNCHANGED <<fv1, fn1, ph, fv2, early, fn2, S1>>

Next ==
  \/ \E v \in C, b \in Blocks : Vote1(v, b) \/ Vote2(v, b)
  \/ \E v \in C : Final1(v) \/ Leave1(v) \/ Install(v) \/ FinalVote2(v) \/ Leave2(v)
  \/ Close1 \/ Close2

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
Finals == {b \in Blocks : Cert1(b) \/ Final2(b)} \cup S1.fin \cup S2.fin

\* No two conflicting blocks are final.
Agreement == Cardinality(Finals) <= 1

\* A record of origin o is discharged by an exclusion witness of epoch o
\* for the rival.  (Epoch-2 support is first votes; epoch 3 is not modelled.)
Disch(l) == IF l.o = 1 THEN XW1(Other(l.b)) ELSE NC2(Other(l.b))

Protected(S, b) == \/ b \in S.fin
                   \/ \E l \in S.locks : l.b = b /\ ~Disch(l)

\* Theorem 6.4 for each closure, with no extra hypothesis.
Thm64a == S1.done => \A b \in Blocks : Cert1(b) => Protected(S1, b)
Thm64b == S2.done => \A b \in Blocks :
            (Cert1(b) \/ Final2(b)) => Protected(S2, b)

\* Reachability probes (expected to be VIOLATED).
ProbeEarlyFinal == ~(\E b \in Blocks : Final2(b) /\ ~S1.done)
ProbeDischarged == ~(S2.done /\ S1.locks # {} /\ \E b \in S2.fin :
                       \E l \in S1.locks : l.b = Other(b))
ProbeTwoRecords == ~(\E l1, l2 \in S2.locks : l1.o # l2.o)
=============================================================================
