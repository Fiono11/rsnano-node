---- MODULE RaiResolve_TTrace_1794389554 ----
EXTENDS RaiResolve_TEConstants, Sequences, RaiResolve, TLCExt, Toolbox, Naturals, TLC

_expression ==
    LET RaiResolve_TEExpression == INSTANCE RaiResolve_TEExpression
    IN RaiResolve_TEExpression!expression
----

_trace ==
    LET RaiResolve_TETrace == INSTANCE RaiResolve_TETrace
    IN RaiResolve_TETrace!trace
----

_inv ==
    ~(
        TLCGet("level") = Len(_TETrace)
        /\
        fv = (<<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y")>>)
        /\
        cur = ((c1 :> 4 @@ c2 :> 4 @@ c3 :> 4))
        /\
        S = ((0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {[b |-> "X", o |-> 2]}, done |-> TRUE]))
        /\
        sv = (<<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>)
        /\
        bf = (<<{}, {}, {}>>)
        /\
        inst = ((c1 :> 2 @@ c2 :> 2 @@ c3 :> 2))
        /\
        fn = (<<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>)
        /\
        bn = (<<{}, {}, {}>>)
        /\
        good = (<<FALSE, TRUE, TRUE>>)
        /\
        seen = ((c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"}))
    )
----

_init ==
    /\ cur = _TETrace[1].cur
    /\ bf = _TETrace[1].bf
    /\ S = _TETrace[1].S
    /\ bn = _TETrace[1].bn
    /\ good = _TETrace[1].good
    /\ fn = _TETrace[1].fn
    /\ fv = _TETrace[1].fv
    /\ sv = _TETrace[1].sv
    /\ inst = _TETrace[1].inst
    /\ seen = _TETrace[1].seen
----

_next ==
    /\ \E i,j \in DOMAIN _TETrace:
        /\ \/ /\ j = i + 1
              /\ i = TLCGet("level")
        /\ cur  = _TETrace[i].cur
        /\ cur' = _TETrace[j].cur
        /\ bf  = _TETrace[i].bf
        /\ bf' = _TETrace[j].bf
        /\ S  = _TETrace[i].S
        /\ S' = _TETrace[j].S
        /\ bn  = _TETrace[i].bn
        /\ bn' = _TETrace[j].bn
        /\ good  = _TETrace[i].good
        /\ good' = _TETrace[j].good
        /\ fn  = _TETrace[i].fn
        /\ fn' = _TETrace[j].fn
        /\ fv  = _TETrace[i].fv
        /\ fv' = _TETrace[j].fv
        /\ sv  = _TETrace[i].sv
        /\ sv' = _TETrace[j].sv
        /\ inst  = _TETrace[i].inst
        /\ inst' = _TETrace[j].inst
        /\ seen  = _TETrace[i].seen
        /\ seen' = _TETrace[j].seen

\* Uncomment the ASSUME below to write the states of the error trace
\* to the given file in Json format. Note that you can pass any tuple
\* to `JsonSerialize`. For example, a sub-sequence of _TETrace.
    \* ASSUME
    \*     LET J == INSTANCE Json
    \*         IN J!JsonSerialize("RaiResolve_TTrace_1794389554.json", _TETrace)

=============================================================================

 Note that you can extract this module `RaiResolve_TEExpression`
  to a dedicated file to reuse `expression` (the module in the 
  dedicated `RaiResolve_TEExpression.tla` file takes precedence 
  over the module `RaiResolve_TEExpression` below).

---- MODULE RaiResolve_TEExpression ----
EXTENDS RaiResolve_TEConstants, Sequences, RaiResolve, TLCExt, Toolbox, Naturals, TLC

expression == 
    [
        \* To hide variables of the `RaiResolve` spec from the error trace,
        \* remove the variables below.  The trace will be written in the order
        \* of the fields of this record.
        cur |-> cur
        ,bf |-> bf
        ,S |-> S
        ,bn |-> bn
        ,good |-> good
        ,fn |-> fn
        ,fv |-> fv
        ,sv |-> sv
        ,inst |-> inst
        ,seen |-> seen
        
        \* Put additional constant-, state-, and action-level expressions here:
        \* ,_stateNumber |-> _TEPosition
        \* ,_curUnchanged |-> cur = cur'
        
        \* Format the `cur` variable as Json value.
        \* ,_curJson |->
        \*     LET J == INSTANCE Json
        \*     IN J!ToJson(cur)
        
        \* Lastly, you may build expressions over arbitrary sets of states by
        \* leveraging the _TETrace operator.  For example, this is how to
        \* count the number of times a spec variable changed up to the current
        \* state in the trace.
        \* ,_curModCount |->
        \*     LET F[s \in DOMAIN _TETrace] ==
        \*         IF s = 1 THEN 0
        \*         ELSE IF _TETrace[s].cur # _TETrace[s-1].cur
        \*             THEN 1 + F[s-1] ELSE F[s-1]
        \*     IN F[_TEPosition - 1]
    ]

=============================================================================



Parsing and semantic processing can take forever if the trace below is long.
 In this case, it is advised to uncomment the module below to deserialize the
 trace from a generated binary file.

\*
\*---- MODULE RaiResolve_TETrace ----
\*EXTENDS RaiResolve_TEConstants, IOUtils, RaiResolve, TLC
\*
\*trace == IODeserialize("RaiResolve_TTrace_1794389554.bin", TRUE)
\*
\*=============================================================================
\*

---- MODULE RaiResolve_TETrace ----
EXTENDS RaiResolve_TEConstants, RaiResolve, TLC

trace == 
    <<
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "-" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 0 @@ c2 :> 0 @@ c3 :> 0),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "-" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 0 @@ c2 :> 0 @@ c3 :> 0),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "-"), (c1 :> "-" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 0 @@ c2 :> 0 @@ c3 :> 0),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "-" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 0 @@ c2 :> 0 @@ c3 :> 0),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "-" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 0 @@ c3 :> 0),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "-" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 1 @@ c3 :> 0),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "-" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 1 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "-" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 3 @@ c2 :> 2 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 1 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 3 @@ c2 :> 2 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 1 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "-" @@ c3 :> "-")>>,cur |-> (c1 :> 3 @@ c2 :> 3 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 1 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "-")>>,cur |-> (c1 :> 3 @@ c2 :> 3 @@ c3 :> 2),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 1 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "-")>>,cur |-> (c1 :> 3 @@ c2 :> 3 @@ c3 :> 3),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 1 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y")>>,cur |-> (c1 :> 3 @@ c2 :> 3 @@ c3 :> 3),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {}, done |-> FALSE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 1 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y")>>,cur |-> (c1 :> 3 @@ c2 :> 3 @@ c3 :> 3),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {[b |-> "X", o |-> 2]}, done |-> TRUE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 1 @@ c2 :> 1 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y")>>,cur |-> (c1 :> 3 @@ c2 :> 3 @@ c3 :> 3),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {[b |-> "X", o |-> 2]}, done |-> TRUE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 2 @@ c2 :> 1 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y")>>,cur |-> (c1 :> 3 @@ c2 :> 3 @@ c3 :> 3),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {[b |-> "X", o |-> 2]}, done |-> TRUE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 1),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y")>>,cur |-> (c1 :> 3 @@ c2 :> 3 @@ c3 :> 3),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {[b |-> "X", o |-> 2]}, done |-> TRUE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y")>>,cur |-> (c1 :> 4 @@ c2 :> 3 @@ c3 :> 3),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {[b |-> "X", o |-> 2]}, done |-> TRUE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y")>>,cur |-> (c1 :> 4 @@ c2 :> 4 @@ c3 :> 3),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {[b |-> "X", o |-> 2]}, done |-> TRUE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})]),
    ([fv |-> <<(c1 :> "-" @@ c2 :> "-" @@ c3 :> "-"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y"), (c1 :> "X" @@ c2 :> "X" @@ c3 :> "Y")>>,cur |-> (c1 :> 4 @@ c2 :> 4 @@ c3 :> 4),S |-> (0 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 1 :> [fin |-> {}, locks |-> {}, done |-> TRUE] @@ 2 :> [fin |-> {}, locks |-> {[b |-> "X", o |-> 2]}, done |-> TRUE]),sv |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bf |-> <<{}, {}, {}>>,inst |-> (c1 :> 2 @@ c2 :> 2 @@ c3 :> 2),fn |-> <<(c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE), (c1 :> FALSE @@ c2 :> FALSE @@ c3 :> FALSE)>>,bn |-> <<{}, {}, {}>>,good |-> <<FALSE, TRUE, TRUE>>,seen |-> (c1 :> {"X", "Y"} @@ c2 :> {"X", "Y"} @@ c3 :> {"X", "Y"})])
    >>
----


=============================================================================

---- MODULE RaiResolve_TEConstants ----
EXTENDS RaiResolve

CONSTANTS c1, c2, c3, z

=============================================================================

---- CONFIG RaiResolve_TTrace_1794389554 ----
CONSTANTS
    C = { c1 , c2 , c3 }
    byz = z
    N = 4
    f = 1
    p = 0
    RULES = TRUE
    EARLYKEPT = FALSE
    CONTESTED <- RuleOff
    c3 = c3
    c1 = c1
    c2 = c2
    z = z

INVARIANT
    _inv

CHECK_DEADLOCK
    \* CHECK_DEADLOCK off because of PROPERTY or INVARIANT above.
    FALSE

INIT
    _init

NEXT
    _next

CONSTANT
    _TETrace <- _trace

ALIAS
    _expression
=============================================================================
\* Generated on Wed Nov 11 09:40:32 WET 2026