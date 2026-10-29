------------------------- MODULE RaiRecoverScenarios -------------------------
(***************************************************************************)
(* Scripted scenarios for RaiRecover: action constraints that confine the  *)
(* validators to the roles of the expected traces.  A restriction only     *)
(* removes behaviours, so a violation found under one is a run of the full *)
(* model.  The constraints name validators, so run these without the VIEW. *)
(***************************************************************************)
EXTENDS RaiRecover

\* w casts the epoch-e first vote x at position s in this step.
Casts(w, e, s, x) == fv[e][w][s] = "-" /\ fv'[e][w][s] = x
NewVote(w, e, s)  == fv'[e][w][s] # fv[e][w][s]

\* Expected trace 1 (IMPLIED = "off"): 1, 3, 4 vote A and 2, 5 vote A' in
\* epoch 1; in epoch 2, 1, 3, 4 re-vote A early, 2 and 5 vote nothing at v,
\* and all five vote C after installing S1.  Nobody leaves epoch 2.
Trace1 ==
  /\ \A w \in C : cur'[w] <= 2
  /\ \A w \in C, s \in 1..2 : NewVote(w, 1, s) =>
       s = 1 /\ fv'[1][w][1] = IF w \in {"c1", "c3", "c4"} THEN "A" ELSE "A2"
  /\ \A w \in C : NewVote(w, 2, 1) => w \in {"c1", "c3", "c4"} /\ Early(w)
                                      /\ fv'[2][w][1] = "A"
  /\ \A w \in C : NewVote(w, 2, 2) => fv'[2][w][2] = "C" /\ ~Early(w)

\* The same with every epoch-1 vote cast before its voter leaves epoch 1.
Trace1Exact ==
  /\ Trace1
  /\ \A w \in C : (cur[w] = 1 /\ cur'[w] > 1) => fv[1][w][1] # "-"

\* Expected trace 3 (IMPLIED = "all", SETTLED = FALSE, PICKRULE = "hash"):
\* 1, 2 only ever vote A, 4, 5 only A'; 3 votes only after installing the
\* predecessor; nobody votes at v+1.
Trace3 ==
  /\ \A w \in C, e \in Epochs : NewVote(w, e, 2) => FALSE
  /\ \A w \in {"c1", "c2"}, e \in Epochs : NewVote(w, e, 1) => fv'[e][w][1] = "A"
  /\ \A w \in {"c4", "c5"}, e \in Epochs : NewVote(w, e, 1) => fv'[e][w][1] = "A2"
  /\ \A e \in Epochs : NewVote("c3", e, 1) => ~Early("c3")

\* Recovered as specified: nobody votes at all before the last epoch.
NoVotesBeforeLast == \A w \in C, e \in 1..(E - 1), s \in 1..2 : ~NewVote(w, e, s)

\* Nobody votes between the fork epoch and the last one, so no later closure
\* sees a reporter vote.  Symmetric: the VIEW may stay.
NoVotesMiddle == \A w \in C, e \in 2..(E - 1), s \in 1..2 : ~NewVote(w, e, s)
==============================================================================
