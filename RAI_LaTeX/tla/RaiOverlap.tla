----------------------------- MODULE RaiOverlap -----------------------------
(***************************************************************************)
(* RAI overlap, one account position, N = 6, f = p = 1.                    *)
(*                                                                         *)
(* Epoch 1 is closing, epoch 2 is open.  Validators cross their local      *)
(* boundary one by one.  After it, and before installing S1, a validator   *)
(* may first-vote in epoch 2 (cross-epoch guard) and then also signs a     *)
(* late notarization in epoch 1.  A block may finalize before S1 is known  *)
(* under the core overlap exception.  One closure builds S1 from N-f       *)
(* reports (no inherited locks: the predecessor is genesis).               *)
(*                                                                         *)
(* Switches:                                                               *)
(*  NCSPLIT   TRUE : a validator records N / final-votes only on an NC of  *)
(*                   q first votes.  FALSE: late notarizations count too.  *)
(*  DISCHARGE TRUE : a recovery lock is discharged by a conflicting        *)
(*                   epoch-1 NC (first votes and late notarizations).      *)
(*                   FALSE: never (the prototype, Section 9).              *)
(*  LATEEV    TRUE : late notarizations are admissible manifest evidence   *)
(*                   for discharge.  FALSE: "never construction input".    *)
(*                                                                         *)
(* Abstractions: the Byzantine validator's signatures are always           *)
(* available; a validator that observes its NC final-votes in the same     *)
(* step; finality decisions are recorded globally (ovf: by the overlap     *)
(* exception at a validator that has not installed S1; pof: by ordinary    *)
(* rechecking at one that has) instead of per validator; closure 2 is not  *)
(* built (its acceptance condition for the case of interest is the         *)
(* invariant WitnessAdmissible).                                           *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS C, byz, NCSPLIT, DISCHARGE, LATEEV

Blocks  == {"X", "Y"}
Other(b) == IF b = "X" THEN "Y" ELSE "X"
Q       == 4
R       == 3
FAST    == 5
NREP    == 5
Tags    == {"-", "N", "F"}
NoRep   == [T |-> [b \in Blocks |-> "none"], G |-> {}]

VARIABLES fv1,    \* epoch-1 first vote
          fn1,    \* epoch-1 notarization recorded and final vote released
          left,   \* crossed the local boundary (epoch-1 report frozen)
          rep,    \* frozen epoch-1 report
          fv2,    \* epoch-2 first vote
          late,   \* late notarization signed in epoch 1
          fn2,    \* epoch-2 final vote released
          S,      \* decided checkpoint S1
          inst,   \* installed S1
          ovf,    \* blocks finalized somewhere under the overlap exception
          pof     \* blocks finalized somewhere by rechecking against S1
vars == <<fv1, fn1, left, rep, fv2, late, fn2, S, inst, ovf, pof>>

Local(v) == <<fv1[v], fn1[v], left[v], rep[v], fv2[v], late[v], fn2[v], inst[v]>>
View == <<[t \in {Local(v) : v \in C} |->
             Cardinality({v \in C : Local(v) = t})], S, ovf, pof>>

Cnt(f, b) == Cardinality({v \in C : f[v] = b})
FirstNC1(b) == Cnt(fv1, b) + 1 >= Q
MixedNC1(b) == Cnt(fv1, b) + Cnt(late, b) + 1 >= Q
Cert1(b) == \/ Cnt(fv1, b) + 1 >= FAST
            \/ Cardinality({v \in C : fv1[v] = b /\ fn1[v]}) + 1 >= Q
NC2(b)   == Cnt(fv2, b) + 1 >= Q
Cert2(b) == \/ Cnt(fv2, b) + 1 >= FAST
            \/ Cardinality({v \in C : fv2[v] = b /\ fn2[v]}) + 1 >= Q

\* All locks in S1 have origin 1.
Discharged(l) == DISCHARGE /\ MixedNC1(Other(l.b))
\* Ordinary rechecking of b against the installed checkpoint.
PostValid(b) == /\ S.fin \subseteq {b}
                /\ \A l \in S.locks : l.b = Other(b) => Discharged(l)

Init ==
  /\ fv1 = [v \in C |-> "-"] /\ fn1 = [v \in C |-> FALSE]
  /\ left = [v \in C |-> FALSE] /\ rep = [v \in C |-> NoRep]
  /\ fv2 = [v \in C |-> "-"] /\ late = [v \in C |-> "-"]
  /\ fn2 = [v \in C |-> FALSE]
  /\ S = [done |-> FALSE, fin |-> {}, locks |-> {}]
  /\ inst = [v \in C |-> FALSE] /\ ovf = {} /\ pof = {}

Vote1(v, b) ==
  /\ ~left[v] /\ fv1[v] = "-"
  /\ fv1' = [fv1 EXCEPT ![v] = b]
  /\ UNCHANGED <<fn1, left, rep, fv2, late, fn2, S, inst, ovf, pof>>

Final1(v) ==
  /\ ~left[v] /\ fv1[v] # "-" /\ ~fn1[v]
  /\ IF NCSPLIT THEN FirstNC1(fv1[v]) ELSE MixedNC1(fv1[v])
  /\ fn1' = [fn1 EXCEPT ![v] = TRUE]
  /\ UNCHANGED <<fv1, left, rep, fv2, late, fn2, S, inst, ovf, pof>>

Leave(v) ==
  /\ ~left[v] /\ ~S.done
  /\ \E seen \in SUBSET {b \in Blocks : Cert1(b)} :
       LET T == [b \in Blocks |->
                   IF b \in seen THEN "F"
                   ELSE IF fv1[v] = b /\ fn1[v] THEN "N" ELSE "-"]
           G == {b \in Blocks : fv1[v] = b /\ T[b] = "-"}
       IN rep' = [rep EXCEPT ![v] = [T |-> T, G |-> G]]
  /\ left' = [left EXCEPT ![v] = TRUE]
  /\ UNCHANGED <<fv1, fn1, fv2, late, fn2, S, inst, ovf, pof>>

\* Before installation: cross-epoch guard, plus a late notarization unless
\* the validator already supported this block by its epoch-1 first vote.
\* After installation: ordinary rules against S1.
Vote2(v, b) ==
  /\ left[v] /\ fv2[v] = "-"
  /\ IF inst[v]
     THEN /\ S.fin = {} /\ PostValid(b)
          /\ UNCHANGED late
     ELSE /\ fv1[v] \in {"-", b}
          /\ late' = [late EXCEPT ![v] = IF fv1[v] = "-" THEN b ELSE "-"]
  /\ fv2' = [fv2 EXCEPT ![v] = b]
  /\ UNCHANGED <<fv1, fn1, left, rep, fn2, S, inst, ovf, pof>>

Final2(v) ==
  /\ fv2[v] # "-" /\ ~fn2[v] /\ NC2(fv2[v])
  /\ inst[v] => PostValid(fv2[v])
  /\ fn2' = [fn2 EXCEPT ![v] = TRUE]
  /\ UNCHANGED <<fv1, fn1, left, rep, fv2, late, S, inst, ovf, pof>>

\* Some validator that has not installed S1 applies the overlap exception.
FinalizeOverlap(b) ==
  /\ b \notin ovf /\ \E v \in C : ~inst[v]
  /\ Cert2(b) /\ MixedNC1(b) /\ NC2(b)
  /\ ovf' = ovf \cup {b}
  /\ UNCHANGED <<fv1, fn1, left, rep, fv2, late, fn2, S, inst, pof>>

\* Some validator that has installed S1 accepts an epoch-2 certificate.
FinalizePost(b) ==
  /\ b \notin pof /\ \E v \in C : inst[v]
  /\ Cert2(b) /\ PostValid(b)
  /\ pof' = pof \cup {b}
  /\ UNCHANGED <<fv1, fn1, left, rep, fv2, late, fn2, S, inst, ovf>>

Install(v) ==
  /\ S.done /\ ~inst[v]
  /\ inst' = [inst EXCEPT ![v] = TRUE]
  /\ left' = [left EXCEPT ![v] = TRUE]
  /\ UNCHANGED <<fv1, fn1, rep, fv2, late, fn2, S, ovf, pof>>

\* An N entry is checked against first votes only: late notarizations are
\* in no report and are not construction input for Rule 1 or Rule 3.
Usable(r) == \A b \in Blocks : r.T[b] = "N" => FirstNC1(b)

ByzReports ==
  {r \in [T : [Blocks -> Tags], G : SUBSET Blocks] :
     /\ \A b \in Blocks : /\ r.T[b] = "F" => Cert1(b)
                          /\ r.T[b] = "N" => FirstNC1(b)
     /\ \A b \in r.G : r.T[b] = "-"}

Build(Qs, brep) ==
  LET Rp(i) == IF i = byz THEN brep ELSE rep[i]
      Fs    == {b \in Blocks : \E i \in Qs : Rp(i).T[b] = "F"}
      Ns    == {b \in Blocks : \E i \in Qs : Rp(i).T[b] = "N"}
      U(b)  == {i \in Qs : b \in Rp(i).G}
      Thr   == {b \in Blocks : Cardinality(U(b)) >= R}
      new   == IF Cardinality(Thr) = 1
               THEN {[b |-> CHOOSE b \in Thr : TRUE, o |-> 1]} ELSE {}
  IN IF Fs # {} THEN [done |-> TRUE, fin |-> Fs, locks |-> {}]
     ELSE IF Ns # {} THEN [done |-> TRUE, fin |-> Ns, locks |-> {}]
     ELSE [done |-> TRUE, fin |-> {}, locks |-> new]

Close ==
  /\ ~S.done
  /\ \E Qs \in SUBSET (C \cup {byz}) :
       /\ Cardinality(Qs) = NREP
       /\ \A v \in Qs \ {byz} : rep[v] # NoRep /\ Usable(rep[v])
       /\ \E brep \in (IF byz \in Qs THEN ByzReports ELSE {NoRep}) :
            S' = Build(Qs, brep)
  /\ UNCHANGED <<fv1, fn1, left, rep, fv2, late, fn2, inst, ovf, pof>>

Next ==
  \/ \E v \in C, b \in Blocks : Vote1(v, b) \/ Vote2(v, b)
  \/ \E b \in Blocks : FinalizeOverlap(b) \/ FinalizePost(b)
  \/ \E v \in C : Final1(v) \/ Leave(v) \/ Final2(v) \/ Install(v)
  \/ Close

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* Every block some correct validator treats as final: by an epoch-1 account
\* certificate, by the overlap exception, by rechecking, or by S1.
Finals == {b \in Blocks : Cert1(b)} \cup ovf \cup pof \cup S.fin

\* Finalized histories at correct validators and the checkpoint are
\* compatible (Theorem 6.8 at this position).
Agreement == Cardinality(Finals) <= 1

\* Theorem 6.4 for closure 1, with discharge by either kind of NC.
Thm64 ==
  S.done => \A b \in Blocks : Cert1(b) =>
    \/ b \in S.fin
    \/ \E l \in S.locks : l.b = b /\ ~MixedNC1(Other(b))

\* Order independence: whatever one validator finalized before installing,
\* a validator that installs S1 first accepts on the same evidence.
SameVerdict == S.done => \A b \in Finals : PostValid(b)

\* Lemma 6.9: once every validator has left epoch 1, every correct report
\* is usable.
UsableReports ==
  (\A v \in C : left[v]) => \A v \in C : rep[v] # NoRep => Usable(rep[v])

\* Lemma 6.7 at the next closure: a correct F claim for b against an
\* inherited lock on its rival needs a discharge witness the manifest may
\* carry.  If late notarizations are inadmissible it must be a first-vote NC.
WitnessAdmissible ==
  LATEEV \/ \A b \in Finals : \A l \in S.locks : l.b = Other(b) => FirstNC1(b)

\* Reachability probes (expected to be VIOLATED).
ProbeOverlapFinal == ~(\E b \in ovf : ~S.done /\ ~Cert1(b))
ProbeLockVsFinal  == ~(\E b \in ovf, l \in S.locks : l.b = Other(b))
=============================================================================
