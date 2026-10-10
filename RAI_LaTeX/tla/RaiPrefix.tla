----------------------------- MODULE RaiPrefix ------------------------------
(***************************************************************************)
(* RAI prefix-wide overlap guard: one account, two positions.              *)
(*                                                                         *)
(* Slot 1 holds a parent P and a rival Pr; slot 2 holds two children C1    *)
(* and C2 of P.  Epoch 1 is closing and is voted by K1; epoch 2 is open    *)
(* and is voted by K2.  K1 and K2 are the correct members; the Byzantine   *)
(* validator sits in both and its signatures are available for every       *)
(* block, vote kind and epoch.  Members of K1 cross their local boundary   *)
(* one by one.  Nobody installs S1, so every epoch-2 first vote is early:  *)
(* it forms no fast certificate, and an epoch-2 final vote is released     *)
(* only under the overlap guard (the early-vote rule).  A member of both   *)
(* committees that first-votes x in epoch 2 also signs a late              *)
(* notarization for x in epoch 1 unless it first-voted at that slot there; *)
(* the cross-epoch rule stops it first-voting a different block at a slot  *)
(* where it supported one in epoch 1.                                      *)
(*                                                                         *)
(* The closure and BuildState are left out.  Early finality is checked     *)
(* against latent finality instead: any epoch-1 FC or FF that can be       *)
(* formed from released signatures.  That is the obligation the guard      *)
(* protects (S1 must preserve it by Theorem 1).                            *)
(*                                                                         *)
(* Switches:                                                               *)
(*  GUARD      "prefix": every unresolved block on the path to the         *)
(*                       finalized anchor needs XW1 and NC2.               *)
(*             "target": only the finalizing block is checked.             *)
(*  FRESHCHILD TRUE : a child needs its own XW1.                           *)
(*             FALSE: NC2 for the child plus a parent notarized in both    *)
(*                    epochs (NC1 and NC2) is enough.                      *)
(*                                                                         *)
(* Abstractions: votes are sets of signers per (epoch, kind, block), not   *)
(* messages; admission checks and the guard read formable certificates     *)
(* rather than a validator's received set; finality is recorded globally;  *)
(* members of K2 outside K1 may vote in epoch 2 from the start.  Each of   *)
(* these only adds behaviours.                                             *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS K1, K2, byz, N, f, p, GUARD, FRESHCHILD

ASSUME /\ Cardinality(K1) = N - 1 /\ Cardinality(K2) = N - 1
       /\ byz \notin K1 \cup K2
       /\ GUARD \in {"prefix", "target"} /\ FRESHCHILD \in BOOLEAN

C        == K1 \cup K2
Q        == N - f - p
FAST     == N - p
Slots    == {1, 2}
Blocks   == {"P", "Pr", "C1", "C2"}
Children == {"C1", "C2"}
Slot(x)  == IF x \in Children THEN 2 ELSE 1
Parent(x) == "P"                              \* only used on children

\* x is y or an ancestor of y.
AncOf(x, y) == x = y \/ (y \in Children /\ x = Parent(y))
Conflict(x, y) == ~AncOf(x, y) /\ ~AncOf(y, x)
AncClose(S) == S \cup {Parent(x) : x \in S \cap Children}
Path(x) == AncClose({x})

VARIABLES fv1,   \* epoch-1 first vote per slot
          fn1,   \* epoch-1 final vote released per slot
          left,  \* crossed the local boundary out of epoch 1
          fv2,   \* epoch-2 first vote per slot
          fn2    \* epoch-2 final vote released per slot
vars == <<fv1, fn1, left, fv2, fn2>>

Local(v) == <<v \in K1, v \in K2, left[v], fv1[v], fn1[v], fv2[v], fn2[v]>>
View == [t \in {Local(v) : v \in C} |->
           Cardinality({v \in C : Local(v) = t})]

-----------------------------------------------------------------------------
\* Epoch 1.  Late notarizations come only from members of both committees.
FV1(x) == Cardinality({v \in K1 : fv1[v][Slot(x)] = x})
LT1(x) == Cardinality({v \in K1 \cap K2 :
                         left[v] /\ fv1[v][Slot(x)] = "-" /\ fv2[v][Slot(x)] = x})
NC1(x)   == FV1(x) + 1 >= Q
XW1(x)   == FV1(x) + LT1(x) + 1 >= Q
FC1(x)   == Cardinality({v \in K1 : fv1[v][Slot(x)] = x /\ fn1[v][Slot(x)]}) + 1 >= Q
FF1(x)   == FV1(x) + 1 >= FAST
Cert1(x) == FC1(x) \/ FF1(x)
\* Latent epoch-1 finality, with ancestors.
Fin1 == AncClose({x \in Blocks : Cert1(x)})

\* Epoch 2.  All first votes are early, so there is no fast certificate.
FV2(x) == Cardinality({v \in K2 : fv2[v][Slot(x)] = x})
NC2(x) == FV2(x) + 1 >= Q
FC2(x) == Cardinality({v \in K2 : fv2[v][Slot(x)] = x /\ fn2[v][Slot(x)]}) + 1 >= Q

\* Cross-boundary exclusion at one position.
PosOK(x) == XW1(x) /\ NC2(x)
CheckOK(x) ==
  IF x \in Children /\ ~FRESHCHILD
  THEN PosOK(x) \/ (NC2(x) /\ NC1(Parent(x)) /\ NC2(Parent(x)))
  ELSE PosOK(x)

\* The overlap guard for finalizing x before S1 is known.  A block with
\* latent epoch-1 finality is a safe anchor.
Guard(x) ==
  IF GUARD = "prefix"
  THEN \A y \in Path(x) : y \in Fin1 \/ CheckOK(y)
  ELSE x \in Fin1 \/ CheckOK(x)

EarlyFinal == {x \in Blocks : FC2(x) /\ Guard(x)}

-----------------------------------------------------------------------------
Init ==
  /\ fv1 = [v \in C |-> [s \in Slots |-> "-"]]
  /\ fn1 = [v \in C |-> [s \in Slots |-> FALSE]]
  /\ left = [v \in C |-> v \notin K1]
  /\ fv2 = [v \in C |-> [s \in Slots |-> "-"]]
  /\ fn2 = [v \in C |-> [s \in Slots |-> FALSE]]

\* A child needs a complete epoch-1 parent.
Vote1(v, x) ==
  /\ v \in K1 /\ ~left[v] /\ fv1[v][Slot(x)] = "-"
  /\ x \in Children => NC1(Parent(x))
  /\ fv1' = [fv1 EXCEPT ![v][Slot(x)] = x]
  /\ UNCHANGED <<fn1, left, fv2, fn2>>

Final1(v, s) ==
  /\ v \in K1 /\ ~left[v] /\ fv1[v][s] # "-" /\ ~fn1[v][s]
  /\ NC1(fv1[v][s])
  /\ fn1' = [fn1 EXCEPT ![v][s] = TRUE]
  /\ UNCHANGED <<fv1, left, fv2, fn2>>

Leave(v) ==
  /\ v \in K1 /\ ~left[v]
  /\ left' = [left EXCEPT ![v] = TRUE]
  /\ UNCHANGED <<fv1, fn1, fv2, fn2>>

\* Cross-epoch rule; a child needs a closing-epoch finalized parent or a
\* complete current-epoch parent.  The late notarization is implicit (LT1).
Vote2(v, x) ==
  /\ v \in K2 /\ left[v] /\ fv2[v][Slot(x)] = "-"
  /\ v \in K1 => fv1[v][Slot(x)] \in {"-", x}
  /\ x \in Children => (Parent(x) \in Fin1 \/ NC2(Parent(x)))
  /\ fv2' = [fv2 EXCEPT ![v][Slot(x)] = x]
  /\ UNCHANGED <<fv1, fn1, left, fn2>>

\* Early-vote rule: an early final vote only under the overlap guard.
Final2(v, s) ==
  /\ v \in K2 /\ fv2[v][s] # "-" /\ ~fn2[v][s]
  /\ NC2(fv2[v][s]) /\ Guard(fv2[v][s])
  /\ fn2' = [fn2 EXCEPT ![v][s] = TRUE]
  /\ UNCHANGED <<fv1, fn1, left, fv2>>

Next ==
  \/ \E v \in C, x \in Blocks : Vote1(v, x) \/ Vote2(v, x)
  \/ \E v \in C, s \in Slots : Final1(v, s) \/ Final2(v, s)
  \/ \E v \in C : Leave(v)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* Everything final somewhere: latent epoch-1 finality and early epoch-2
\* finality, each with its ancestors.
Finals == Fin1 \cup AncClose(EarlyFinal)

NoConflict == \A x, y \in Finals : ~Conflict(x, y)

\* Early finality, with ancestors, is compatible with every block that has
\* a formable epoch-1 FC or FF.
EarlyCompatible ==
  \A x \in AncClose(EarlyFinal), y \in Blocks : Cert1(y) => ~Conflict(x, y)

\* Reachability probes (expected to be VIOLATED).
\* A child finalizes early.
ProbeChildEarly == ~(\E x \in Children : x \in EarlyFinal /\ x \notin Fin1)
\* The parent is finalized through its child only.
ProbeParentViaChild ==
  ~("P" \in AncClose(EarlyFinal) /\ "P" \notin EarlyFinal /\ "P" \notin Fin1)
\* The precondition of the failure traces: a latent epoch-1 certificate on
\* one branch and an epoch-2 NC on a conflicting child.
ProbeSplitBranches ==
  ~(\E x \in Blocks, y \in Children : Cert1(x) /\ NC2(y) /\ Conflict(x, y))
=============================================================================
