----------------------------- MODULE RaiResolve -----------------------------
(***************************************************************************)
(* Termination at a forked position: does the position become final?      *)
(*                                                                         *)
(* One account position with two owner-signed conflicting blocks X and Y,  *)
(* both published from the start.  Three voting epochs and two closures    *)
(* with Rules 1-3, origin-stamped recovery records and matching-origin     *)
(* discharge.  One committee: the correct validators C and the Byzantine   *)
(* validator, which signs first and final votes for any block in any epoch *)
(* or withholds them, may first-vote both blocks, and reports adversarially *)
(* within the usability checks.  The manifest discharges any subset of the *)
(* dischargeable records (the adversary picks).                            *)
(*                                                                         *)
(* Voting follows the protocol: one first vote per epoch, the cross-epoch  *)
(* rule while the predecessor is not installed, admission against the      *)
(* latest installed checkpoint with matching-origin discharge, early final *)
(* votes only with the closing-epoch witness, settled first votes only for *)
(* the fast path.  A validator leaves epoch e only with S_{e-1} installed.  *)
(*                                                                         *)
(* RULES = TRUE adds the three termination rules:                          *)
(*  (i)   after installing a checkpoint that keeps a block K here, a       *)
(*        validator first- or final-votes only K here, witnesses or not;   *)
(*  (ii)  at a contested position (it saw both blocks, or its installed    *)
(*        checkpoint keeps a block here) it first-votes in epoch e only    *)
(*        after installing S_{e-1}; with EARLYKEPT = TRUE only where its   *)
(*        latest installed checkpoint keeps nothing here (where it keeps   *)
(*        K, an early vote for K is allowed, rule (i) forbids the rest);   *)
(*  (iii) Rule 2 keeps the eligible block with the most reporter first     *)
(*        votes, ties broken by hash, instead of keeping nothing.          *)
(*                                                                         *)
(* An epoch flagged good is a window of synchrony: no correct validator    *)
(* leaves it before every correct validator has installed S_{e-1}, seen    *)
(* both blocks, and cast every first and final vote the rules allow it.    *)
(* The invariant TwoGood says that two consecutive good epochs make some   *)
(* block final.                                                            *)
(*                                                                         *)
(* The Byzantine validator signs no final votes: they can only complete   *)
(* a final certificate, which ends the run in the adversary's disfavour.   *)
(* Abstractions: no late notarizations and no committee change; a formable *)
(* FC or FF counts as finality (the safety models check validity); a      *)
(* correct report tags a block N only after its final vote.                *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS C, byz, N, f, p, RULES, EARLYKEPT

E        == 3
Epochs   == 1..E
Blocks   == {"X", "Y"}
Other(b) == IF b = "X" THEN "Y" ELSE "X"
Q        == N - f - p
R        == f + p + 1
FAST     == N - p
NREP     == N - f

VARIABLES good, \* [Epochs -> BOOLEAN], fixed at the start
          fv,   \* [Epochs -> [C -> {"-"} \cup Blocks]] first votes
          sv,   \* ... cast with S_{e-1} installed (settled)
          fn,   \* [Epochs -> [C -> BOOLEAN]] final vote released
          bf,   \* [Epochs -> SUBSET Blocks] Byzantine first votes
          bn,   \* [Epochs -> SUBSET Blocks] Byzantine final votes
          cur,  \* [C -> 1..E+1] current epoch
          inst, \* [C -> 0..E-1] latest installed checkpoint
          seen, \* [C -> SUBSET Blocks]
          S     \* [0..E-1 -> checkpoint]
vars == <<good, fv, sv, fn, bf, bn, cur, inst, seen, S>>

Local(v) == <<cur[v], inst[v], seen[v],
              [e \in Epochs |-> <<fv[e][v], sv[e][v], fn[e][v]>>]>>
View == <<[t \in {Local(v) : v \in C} |->
             Cardinality({v \in C : Local(v) = t})], good, bf, bn, S>>

B1(b, X) == IF b \in X THEN 1 ELSE 0
FV(e, b) == Cardinality({v \in C : fv[e][v] = b}) + B1(b, bf[e])
NC(e, b) == FV(e, b) >= Q
XW(e, b) == NC(e, b)                    \* no late notarizations here
FF(e, b) == Cardinality({v \in C : fv[e][v] = b /\ sv[e][v]}) + B1(b, bf[e]) >= FAST
FC(e, b) == Cardinality({v \in C : fv[e][v] = b /\ fn[e][v]}) + B1(b, bn[e]) >= Q

Resolved == \/ \E e \in Epochs, b \in Blocks : FF(e, b) \/ FC(e, b)
            \/ \E k \in 0..E-1 : S[k].fin # {}

Kept(k)          == {l.b : l \in S[k].locks}
Admissible(k, b) == /\ S[k].fin \subseteq {b}
                    /\ \A l \in S[k].locks : l.b = Other(b) => XW(l.o, b)
Contested(v)     == seen[v] = Blocks \/ S[inst[v]].locks # {}

VoteOK(v, b) ==
  LET e == cur[v]  k == inst[v]  settled == k = e - 1 IN
  /\ e <= E /\ fv[e][v] = "-" /\ b \in seen[v]
  /\ Admissible(k, b)
  /\ (~settled /\ e > 1) => fv[e-1][v] \in {"-", b}        \* cross-epoch rule
  /\ RULES => /\ Contested(v) /\ ~(EARLYKEPT /\ Kept(k) # {}) => settled  \* (ii)
              /\ Kept(k) # {} => b \in Kept(k)             \* (i)

FinalOK(v) ==
  LET e == cur[v]  k == inst[v]  b == fv[e][v] IN
  /\ e <= E /\ b # "-" /\ ~fn[e][v] /\ NC(e, b)
  /\ Admissible(k, b)
  /\ IF k = e - 1
     THEN RULES /\ Kept(k) # {} => b \in Kept(k)           \* (i)
     ELSE XW(e - 1, b)                                     \* early-vote rule

NoCk == [done |-> FALSE, fin |-> {}, locks |-> {}]

Init ==
  /\ good \in [Epochs -> BOOLEAN]
  /\ fv = [e \in Epochs |-> [v \in C |-> "-"]]
  /\ sv = [e \in Epochs |-> [v \in C |-> FALSE]]
  /\ fn = [e \in Epochs |-> [v \in C |-> FALSE]]
  /\ bf = [e \in Epochs |-> {}] /\ bn = [e \in Epochs |-> {}]
  /\ cur = [v \in C |-> 1] /\ inst = [v \in C |-> 0]
  /\ seen = [v \in C |-> {}]
  /\ S = [k \in 0..E-1 |-> IF k = 0 THEN [NoCk EXCEPT !.done = TRUE] ELSE NoCk]

Vote(v, b) ==
  /\ VoteOK(v, b)
  /\ fv' = [fv EXCEPT ![cur[v]][v] = b]
  /\ sv' = [sv EXCEPT ![cur[v]][v] = (inst[v] = cur[v] - 1)]
  /\ UNCHANGED <<good, fn, bf, bn, cur, inst, seen, S>>

FinalVote(v) ==
  /\ FinalOK(v)
  /\ fn' = [fn EXCEPT ![cur[v]][v] = TRUE]
  /\ UNCHANGED <<good, fv, sv, bf, bn, cur, inst, seen, S>>

Show(v, b) ==
  /\ b \notin seen[v]
  /\ seen' = [seen EXCEPT ![v] = @ \cup {b}]
  /\ UNCHANGED <<good, fv, sv, fn, bf, bn, cur, inst, S>>

Install(v) ==
  /\ inst[v] < cur[v] - 1 /\ inst[v] < E - 1 /\ S[inst[v] + 1].done
  /\ inst' = [inst EXCEPT ![v] = @ + 1]
  /\ UNCHANGED <<good, fv, sv, fn, bf, bn, cur, seen, S>>

\* In a good epoch nobody leaves before every correct validator is done.
Done(w, e) == \/ cur[w] > e
              \/ /\ cur[w] = e /\ seen[w] = Blocks /\ inst[w] = e - 1
                 /\ ~\E b \in Blocks : VoteOK(w, b)
                 /\ ~FinalOK(w)

Leave(v) ==
  LET e == cur[v] IN
  /\ e <= E /\ inst[v] >= e - 1
  /\ good[e] => \A w \in C : Done(w, e)
  /\ cur' = [cur EXCEPT ![v] = e + 1]
  /\ UNCHANGED <<good, fv, sv, fn, bf, bn, inst, seen, S>>

ByzFirst(e, b) ==
  /\ b \notin bf[e]
  /\ \E v \in C : cur[v] >= e      \* earlier signatures have no effect yet
  /\ bf' = [bf EXCEPT ![e] = @ \cup {b}]
  /\ UNCHANGED <<good, fv, sv, fn, bn, cur, inst, seen, S>>

ByzFinal(e, b) ==
  /\ b \notin bn[e]
  /\ bn' = [bn EXCEPT ![e] = @ \cup {b}]
  /\ UNCHANGED <<good, fv, sv, fn, bf, cur, inst, seen, S>>

-----------------------------------------------------------------------------
\* The checkpoints closure e can build: every selection of N - f reports of
\* validators that left epoch e, every usable Byzantine report, and every
\* manifest discharge set.
Builds(e, prev) ==
  UNION {
    UNION {
      UNION {
        LET Nt(i) == IF i = byz THEN bt
                     ELSE {b \in Blocks : fv[e][i] = b /\ fn[e][i]}
            G(i)  == IF i = byz THEN bg \ bt
                     ELSE {b \in Blocks : fv[e][i] = b} \ Nt(i)
            able  == {l \in prev.locks : XW(l.o, Other(l.b))}
        IN { LET surv    == prev.locks \ D
                 Elig(b) == \A l \in surv : l.b # Other(b)
                 Ns      == {b \in Blocks : Elig(b) /\ \E i \in Qs : b \in Nt(i)}
                 U(b)    == IF Elig(b) THEN {i \in Qs : b \in G(i)} ELSE {}
                 Thr     == {b \in Blocks : Cardinality(U(b)) >= R}
                 Most    == {b \in Blocks : Cardinality(U(b)) >= 1 /\
                              \A c \in Blocks : Cardinality(U(c)) <= Cardinality(U(b))}
                 Pick    == IF "X" \in Most THEN "X" ELSE "Y"   \* ties by hash
                 new     == IF RULES
                            THEN IF Most = {} THEN {} ELSE {[b |-> Pick, o |-> e]}
                            ELSE IF Cardinality(Thr) = 1
                                 THEN {[b |-> CHOOSE b \in Thr : TRUE, o |-> e]}
                                 ELSE {}
             IN IF prev.fin # {} THEN [done |-> TRUE, fin |-> prev.fin, locks |-> {}]
                ELSE IF Ns # {} THEN [done |-> TRUE, fin |-> Ns, locks |-> {}]
                ELSE [done |-> TRUE, fin |-> {}, locks |-> surv \cup new]
             : D \in SUBSET able }
      : bt \in IF byz \in Qs THEN SUBSET {b \in Blocks : NC(e, b)} ELSE {{}} }
    : bg \in IF byz \in Qs THEN SUBSET bf[e] ELSE {{}} }
  : Qs \in {Q2 \in SUBSET (C \cup {byz}) :
             Cardinality(Q2) = NREP /\ \A i \in Q2 \ {byz} : cur[i] > e} }

\* Closure e.  A correct report tags its block N once it final-voted it and
\* lists an untagged first vote in G; the Byzantine report lists any of its
\* own first votes and tags N only a block with a formable NC.
Close(e) ==
  /\ e \in 1..E-1 /\ ~S[e].done /\ S[e-1].done
  /\ \E ck \in Builds(e, S[e-1]) : S' = [S EXCEPT ![e] = ck]
  /\ UNCHANGED <<good, fv, sv, fn, bf, bn, cur, inst, seen>>

\* The run is over once every validator has left the last epoch.  Run TLC
\* with deadlock checking: a deadlock is a state in which a good epoch can
\* never be left, which would make TwoGood vacuous.
Finished ==
  /\ \A v \in C : cur[v] = E + 1
  /\ UNCHANGED vars

Next ==
  \/ \E v \in C, b \in Blocks : Vote(v, b) \/ Show(v, b)
  \/ \E v \in C : FinalVote(v) \/ Install(v) \/ Leave(v)
  \/ \E e \in Epochs, b \in Blocks : ByzFirst(e, b)
  \/ \E e \in 1..E-1 : Close(e)
  \/ Finished

Spec == Init /\ [][Next]_vars

\* The case TwoGood is about, checked exhaustively: epoch 1, the fork epoch,
\* is over and arbitrary (any first votes, final votes on a formable NC, any
\* Byzantine first votes, anything seen), closure 1 has decided any of the
\* checkpoints it can build, which nobody has installed yet, and epochs 2
\* and 3 are good.
Init23 ==
  /\ good = <<FALSE, TRUE, TRUE>>
  /\ fv \in {x \in [Epochs -> [C -> {"-"} \cup Blocks]] :
               \A e \in 2..E : x[e] = [v \in C |-> "-"]}
  /\ bf \in {x \in [Epochs -> SUBSET Blocks] : \A e \in 2..E : x[e] = {}}
  /\ fn \in {x \in [Epochs -> [C -> BOOLEAN]] :
               /\ \A e \in 2..E : x[e] = [v \in C |-> FALSE]
               /\ \A v \in C : x[1][v] => fv[1][v] # "-" /\ NC(1, fv[1][v])}
  /\ sv = [e \in Epochs |-> [v \in C |-> e = 1 /\ fv[1][v] # "-"]]
  /\ bn = [e \in Epochs |-> {}]
  /\ cur = [v \in C |-> 2] /\ inst = [v \in C |-> 0]
  /\ seen \in [C -> SUBSET Blocks]
  /\ \A v \in C : fv[1][v] # "-" => fv[1][v] \in seen[v]
  /\ S \in {[k \in 0..E-1 |-> CASE k = 0 -> [NoCk EXCEPT !.done = TRUE]
                                 [] k = 1 -> ck
                                 [] OTHER -> NoCk]
            : ck \in Builds(1, [NoCk EXCEPT !.done = TRUE])}

Spec23 == Init23 /\ [][Next]_vars

-----------------------------------------------------------------------------
AllLeft(e) == \A v \in C : cur[v] > e

\* Two consecutive good epochs make some block at the position final.
TwoGood == \A e \in 1..E-1 : (good[e] /\ good[e+1] /\ AllLeft(e+1)) => Resolved

\* Reachability probes (expected to be VIOLATED).
ProbeGood12 == ~(good[1] /\ good[2] /\ AllLeft(2))
ProbeGood23 == ~(good[2] /\ good[3] /\ AllLeft(3))
\* One good epoch is not enough.
ProbeOneGood == ~(\E e \in Epochs : good[e] /\ AllLeft(e) /\ ~Resolved)
=============================================================================
