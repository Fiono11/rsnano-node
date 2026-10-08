------------------------------ MODULE RaiClose ------------------------------
(***************************************************************************)
(* RAI closure, one account position, N = 6, f = p = 1.                    *)
(*                                                                         *)
(* Two conflicting blocks X and Y compete for one position.  Epochs        *)
(* 1..MaxE vote; closures 1..MaxE-1 each build a checkpoint from N-f       *)
(* frozen reports with BuildState (final closure, Rules 1-3, inherited     *)
(* locks and matching-origin discharge).                                   *)
(*                                                                         *)
(* FIXB = FALSE : G_i = V_i \ keys(T_i)            (paper, Section 5.2)    *)
(* FIXB = TRUE  : G_i = V_i \ {N- and F-tagged}    (Fix B), and a lock is  *)
(*                a set of (block, origin) records discharged one by one.  *)
(*                                                                         *)
(* Scope: no early (pre-installation) voting, so no late notarization and  *)
(* no overlap finality.  Those are in RaiOverlap.tla.  Committees do not   *)
(* change.  The Byzantine validator's signatures are assumed available for *)
(* every block, vote kind and epoch, and its report is chosen              *)
(* adversarially at each closure subject to the usability checks of        *)
(* Section 5.3.                                                            *)
(*                                                                         *)
(* Three abstractions, each of which only adds behaviours:                 *)
(*  1. A validator's activity in different epochs is carried by separate   *)
(*     identities: fv[1][v] and fv[2][v] are unrelated.  Nothing couples   *)
(*     one identity across epochs except "stop e before opening e+1",      *)
(*     which is dropped: a validator may open e once S[e-1] is decided     *)
(*     and may still be signing in e while epoch e+1 votes.                *)
(*  2. A validator that observes its NC releases its final vote in the     *)
(*     same step.  The tag is N either way and more final votes only make  *)
(*     more certificates formable.                                         *)
(*  3. Reports are forgotten once their closure is decided.                *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS C,     \* the five correct validators
          byz,   \* the Byzantine validator
          FIXB   \* BOOLEAN

MaxE    == 3

Blocks  == {"X", "Y"}
Other(b) == IF b = "X" THEN "Y" ELSE "X"
Q       == 4      \* notarization / normal finality: N - f - p
R       == 3      \* recovery support: f + p + 1
FAST    == 5      \* fast finality: N - p
NREP    == 5      \* selected reports: N - f
Epochs  == 1..MaxE
Closes  == 1..(MaxE - 1)
Tags    == {"-", "R", "N", "F"}
NoRep   == [T |-> [b \in Blocks |-> "none"], G |-> {}]

VARIABLES fv,      \* fv[e][v]    : first vote of v in epoch e ("-" if none)
          fn,      \* fn[e][v]    : v recorded notarization and final-voted
          frozen,  \* frozen[e][v]: v stopped epoch-e signing
          rep,     \* rep[e][v]   : frozen report (T, G)
          S        \* S[e]        : decided checkpoint of closure e
vars == <<fv, fn, frozen, rep, S>>

\* Validators are interchangeable within an epoch, so TLC explores states
\* up to the per-epoch multiset of validator-local states.
Local(e, v) == <<fv[e][v], fn[e][v], frozen[e][v],
                 IF e \in Closes THEN rep[e][v] ELSE NoRep>>
Bag(e) == LET ls == {Local(e, v) : v \in C}
          IN [t \in ls |-> Cardinality({v \in C : Local(e, v) = t})]
View == <<[e \in Epochs |-> Bag(e)], S>>

FVc(e, b) == Cardinality({v \in C : fv[e][v] = b})
FNc(e, b) == Cardinality({v \in C : fv[e][v] = b /\ fn[e][v]})
\* "+ 1" is the Byzantine signature, always available.
NC(e, b)  == FVc(e, b) + 1 >= Q
FF(e, b)  == FVc(e, b) + 1 >= FAST
FC(e, b)  == FNc(e, b) + 1 >= Q
Cert(e, b) == FF(e, b) \/ FC(e, b)

\* A recovery lock record [b, o] protects possible epoch-o finality of b.
\* It is discharged by a conflicting NC of that same epoch o.
Dischargeable(l) == NC(l.o, Other(l.b))

Init ==
  /\ fv     = [e \in Epochs |-> [v \in C |-> "-"]]
  /\ fn     = [e \in Epochs |-> [v \in C |-> FALSE]]
  /\ frozen = [e \in Epochs |-> [v \in C |-> FALSE]]
  /\ rep    = [e \in Closes |-> [v \in C |-> NoRep]]
  /\ S      = [e \in 0..(MaxE - 1) |->
                 [done |-> (e = 0), fin |-> {}, locks |-> {}]]

\* With S[e-1] installed: a finalized position is closed; a rival of a
\* retained block is refused unless every lock on that block is discharged
\* (the validator may hold the witness whenever one can be assembled).
\* Voting for a retained block is allowed (prototype behaviour, Section 9).
CanVote(e, b) ==
  /\ S[e-1].done /\ S[e-1].fin = {}
  /\ \A l \in S[e-1].locks : l.b = Other(b) => Dischargeable(l)

Vote(v, e, b) ==
  /\ ~frozen[e][v] /\ fv[e][v] = "-"
  /\ CanVote(e, b)
  /\ fv' = [fv EXCEPT ![e][v] = b]
  /\ UNCHANGED <<fn, frozen, rep, S>>

FinalVote(v, e) ==
  /\ ~frozen[e][v] /\ fv[e][v] # "-" /\ ~fn[e][v]
  /\ NC(e, fv[e][v])
  /\ fn' = [fn EXCEPT ![e][v] = TRUE]
  /\ UNCHANGED <<fv, frozen, rep, S>>

\* v reaches its local boundary and freezes its report against S[e-1].
\* It may or may not have observed a final certificate that can be formed.
Freeze(v, e) ==
  /\ S[e-1].done /\ ~S[e].done /\ ~frozen[e][v]
  /\ \E seen \in SUBSET {b \in Blocks : \E e2 \in 1..e : Cert(e2, b)} :
       LET T == [b \in Blocks |->
                   IF b \in S[e-1].fin \/ b \in seen        THEN "F"
                   ELSE IF fv[e][v] = b /\ fn[e][v]         THEN "N"
                   ELSE IF \E l \in S[e-1].locks : l.b = b  THEN "R"
                   ELSE "-"]
           mine == IF fv[e][v] = "-" THEN {} ELSE {fv[e][v]}
           G == IF FIXB THEN {b \in mine : T[b] \notin {"N", "F"}}
                        ELSE {b \in mine : T[b] = "-"}
       IN rep' = [rep EXCEPT ![e][v] = [T |-> T, G |-> G]]
  /\ frozen' = [frozen EXCEPT ![e][v] = TRUE]
  /\ UNCHANGED <<fv, fn, S>>

\* Learning the decided checkpoint stops any remaining epoch-e signing.
Stop(v, e) ==
  /\ S[e].done /\ ~frozen[e][v]
  /\ frozen' = [frozen EXCEPT ![e][v] = TRUE]
  /\ UNCHANGED <<fv, fn, rep, S>>

\* Reports the Byzantine validator can get accepted as usable.
ByzReports(e) ==
  {r \in [T : [Blocks -> Tags], G : SUBSET Blocks] :
     /\ \A b \in Blocks :
          /\ r.T[b] = "F" => (b \in S[e-1].fin \/ \E e2 \in 1..e : Cert(e2, b))
          /\ r.T[b] = "N" => NC(e, b)
          /\ r.T[b] = "R" => \E l \in S[e-1].locks : l.b = b
     /\ \A b \in r.G : IF FIXB THEN r.T[b] \notin {"N", "F"}
                               ELSE r.T[b] = "-"}

Build(e, Qs, brep, D) ==
  LET prev   == S[e-1]
      Rp(i)  == IF i = byz THEN brep ELSE rep[e][i]
      Fs     == prev.fin \cup {b \in Blocks : \E i \in Qs : Rp(i).T[b] = "F"}
      Ns     == {b \in Blocks : \E i \in Qs : Rp(i).T[b] = "N"}
      surv   == prev.locks \ D
      Elig(b) == \A l \in surv : l.b # Other(b)
      U(b)   == IF Elig(b) THEN {i \in Qs : b \in Rp(i).G} ELSE {}
      Thr    == {b \in Blocks : Cardinality(U(b)) >= R}
      new    == IF Cardinality(Thr) = 1
                THEN {[b |-> CHOOSE b \in Thr : TRUE, o |-> e]} ELSE {}
  IN IF Fs # {} THEN [done |-> TRUE, fin |-> Fs, locks |-> {}]
     ELSE IF Ns # {} THEN [done |-> TRUE, fin |-> Ns, locks |-> {}] \* Rules 1+3
     ELSE [done |-> TRUE, fin |-> {}, locks |-> surv \cup new]      \* Rule 2

\* The consensus service decides one eligible candidate.  D is the set of
\* inherited lock records the manifest discharges.  A correct vote for the
\* rival of a locked block carries the discharge witness in its admission
\* evidence, so counting that vote forces the discharge.
Close(e) ==
  /\ e \in Closes /\ ~S[e].done /\ S[e-1].done
  /\ \E Qs \in SUBSET (C \cup {byz}) :
       /\ Cardinality(Qs) = NREP
       /\ \A v \in Qs \ {byz} : frozen[e][v] /\ rep[e][v] # NoRep
       /\ \E brep \in (IF byz \in Qs THEN ByzReports(e) ELSE {NoRep}) :
            LET Rp(i)  == IF i = byz THEN brep ELSE rep[e][i]
                forced == {l \in S[e-1].locks :
                             \/ \E i \in Qs \ {byz} : fv[e][i] = Other(l.b)
                             \/ \E i \in Qs : Rp(i).T[Other(l.b)] = "N"}
                able   == {l \in S[e-1].locks : Dischargeable(l)}
            IN \E D \in SUBSET able :
                 /\ forced \subseteq D
                 /\ S' = [S EXCEPT ![e] = Build(e, Qs, brep, D)]
  /\ rep' = [rep EXCEPT ![e] = [v \in C |-> NoRep]]
  /\ UNCHANGED <<fv, fn, frozen>>

Next ==
  \/ \E e \in Epochs : \E v \in C :
        \/ \E b \in Blocks : Vote(v, e, b)
        \/ FinalVote(v, e)
  \/ \E e \in Closes : \E v \in C : Freeze(v, e) \/ Stop(v, e)
  \/ \E e \in Closes : Close(e)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* A block is final if an account certificate for it can be assembled from
\* released signatures, or a decided checkpoint finalized it.
FinalEv(b) == \/ \E e \in Epochs : Cert(e, b)
              \/ \E e \in Closes : b \in S[e].fin

\* Transfer safety at this position.
NoConflict == ~(FinalEv("X") /\ FinalEv("Y"))

\* Theorem 6.4: every decided checkpoint keeps each block whose account
\* final certificate can be formed, as final or under a lock record that
\* no admissible witness can discharge.
Thm64 ==
  \A e \in Closes : S[e].done =>
    \A b \in Blocks :
      (\E e2 \in 1..e : Cert(e2, b)) =>
        \/ b \in S[e].fin
        \/ \E l \in S[e].locks : l.b = b /\ ~Dischargeable(l)

\* Sanity: a checkpoint never finalizes two blocks at one position.
OneFinal == \A e \in Closes : Cardinality(S[e].fin) <= 1

\* Reachability probes (expected to be VIOLATED; they show the model reaches
\* the interesting states and the invariants above are not vacuous).
ProbeTwoOrigins == ~(\E l1, l2 \in S[2].locks : l1.b = l2.b /\ l1.o # l2.o)
ProbeDischarge  == ~(S[2].done /\ S[1].locks # {} /\ S[2].locks = {} /\ S[2].fin = {})
ProbeEpoch3Cert == ~(\E b \in Blocks : Cert(3, b) /\ S[2].locks # {})
=============================================================================
