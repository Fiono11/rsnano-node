# RAI proposer-free close (rules C2/C3), 2026-09-16

Three runs each of the baseline (948a3b656) and the proposer-free close
working tree on the 8-core M1, alternating base/new, on a quiet machine:

```
nanospam --prs 6 --no-prio --fork-percentage 5 --blocks 45000 --accounts 45000 --rate 2000 \
  --epoch-terminated-elections 15000 --closed-epochs 3 --close-timeout 240 --no-kill --data-dir <fresh dir>
```

Release `rsnano` and `nanospam` built with `--features rai_protocol`; no close
tracing. Node data was deleted after each run. A seventh run (new1) is not
included: three Chrome helpers and a Cursor helper held 20-55% of a core
throughout and it showed the load-confound signature (355-900 ms latency in
the first five seconds, 10-15 s closes).

## What changed

- Announcements carry the close parent as well as the membership root.
- The FIRST vote goes to the parent/root pair that certificate weight announced,
  whether or not it is the replica's own root (rule C2 of `rai_protocol.tex`):
  a replica holding more members than the announced value signs it. Readiness
  gates announcing only; every draining replica votes.
- A FINAL vote is bound to the FIRST vote (rule C3): a member learned after the
  FIRST vote no longer withdraws it. The droppable-member rule is gone; it is
  subsumed.
- The announcement phase (vote once every representative announced the same
  pair, or after 6 s with certificate weight) is unchanged.

## Results

Close after the last drain start, first / last PR; non-fork finalization mean /
p95 in ms at PR0's websocket.

| Run | Epoch 0 close | Epoch 1 close | Epoch 2 close | Discarded (e0 / e1 / e2) | Latency e0 / e1 / e2 |
|---|---:|---:|---:|---:|---:|
| base1 | 2.68 / 3.98 s | 5.23 / 5.55 s | 4.08 / 4.29 s | 73 / 256-322 / 70-130 | 243/537, 224/434, 462/699 |
| base2 | 2.66 / 3.81 s | 8.45 / 8.72 s | 0.51 / 0.68 s | 96 / 194-196 / 52-83 | 252/662, 162/304, 274/459 |
| base3 | 3.00 / 4.40 s | 8.36 / 55.86 s | 4 PRs closed | 190 / 116-126 / 12-92 | 184/407, 287/517, 338/582 |
| new2 | 8.76 / 9.06 s | 9.48 / 9.58 s | 6.20 / 6.28 s | 138 / 519-555 / 218-506 | 141/296, 247/507, 689/985 |
| new3 | 2.63 / 2.75 s | 3.18 / 3.50 s | 2.34 / 2.50 s | 131 / 129-143 / 12-69 | 166/322, 226/386, 417/877 |
| new4 | 2.63 / 3.71 s | 5.33 / 7.25 s | 2.25 / 2.50 s | 197 / 282-349 / 145-207 | 177/409, 195/329, 438/743 |

Every close was round 0 with one hash. The two outliers are drain stragglers,
not close rounds: in new2 PR5 held one election (root `C81582DC…`, block
`5BDC5AC3…`) that had two FIRST votes and two FIRST-timeouts and never
terminated, in every epoch, so the other five closed after the 6 s
announcement phase each time; in base3 PR5 took 56 s to close epoch 1 and was
still catching up when epoch 2 closed. Both shapes are the recurring straggler
described in the Sept 16 notes and occur with either build.

Excluding the straggler runs, closes after the last drain are 2.3-3.5 s (new)
against 2.7-8.7 s (base), and latency is the same within run-to-run noise.

## The recurring straggler

Runs `a12`, `a14`, `a17` and `a19` were made with the nodes' termination audit
(`NANOSPAM_TERMINATION_AUDIT`, collected over RPC by `collect_audit.py`) and
`RAI_FORK_TRACE`; the audit roughly doubles latency, so those runs compare
only with each other. `straggle.py` prints, for the elections a drain was
waiting on, every node's audit events for that root in time order; the
`*-straggler-trace.txt` files are its output for the four runs.

Every straggler had one shape: a fork whose two candidates reach the nodes
around a drain boundary, and one node that lacks the second candidate. All
FIRST-timeout and TIMEOUT votes its peers route through that hash are
indeterminate there, so the election cannot terminate until the block
arrives (`a12`: pr1 got `511E0263` 61 s after everyone else; `a14`: pr3 got
`D63B75B9` 65 s late; `a17`: pr4 got `B3956D72` 64 s late). The 60 s is the
network duplicate filter's cutoff: the first copy of the block had reached the
node and was lost before it reached its election, and every copy a peer
republished on request was byte-identical and dropped as a duplicate until the
entry aged out. The multi-epoch straggler of `new2` is the same root re-elected
in each epoch while the node keeps lacking one candidate.

Three repairs, all in the working tree of these runs from `a19` on:

- the hinted scheduler requests a block that representatives voted for but
  neither the ledger nor an election holds, with a zero-root ConfirmReq to the
  principal representatives (1 s scan, 5 s cooldown per hash, not gated on
  container vacancy); the node stats show 1.6k-4.5k such requests per node per
  run, matching the 1.7k-3.9k blocks each node drops at its block-processor
  queue under this load;
- a zero-root request makes the aggregator publish a block it holds even
  without a certificate for it;
- a duplicate `Publish` is suppressed for two filter epochs (5-10 s) instead
  of 60 s; a received block is never flooded on, so this only covers one
  flood's fan-in.

| Run | Audit | Epoch 0 close | Epoch 1 close | Epoch 2 close | Latency e0 / e1 / e2 (mean ms) |
|---|---|---:|---:|---:|---:|
| a19 | yes | 2.59 / 2.73 s | 8.5 s | 6.5 s | 531 (run mean) |
| a20 | yes | 1.66 / 1.82 s | 1.54 / 2.15 s | 0.82 / 0.84 s | 468 |
| a21 | yes | 4.90 / 5.28 s | 2.57 / 2.60 s | 2.26 / 2.36 s | 484 |
| a22 | yes | 1.94 / 2.24 s | 2.74 / 4.73 s | 2.29 / 2.41 s | 485 |
| new8 | no | 1.62 / 1.75 s | 7.36 / 7.61 s | 0.66 / 0.73 s | 220 / 208 / 343 |
| new9 | no | 4.76 / 4.90 s | 2.60 / 2.81 s | 0.37 / 0.56 s | 236 / 177 / 337 |
| new10 | no | 2.65 / 2.82 s | 2.48 / 2.69 s | 2.23 / 2.34 s | 232 / 286 / 387 |

No minute-long straggler in these seven runs (previously about one run in
three); in `a19` the one late candidate arrived 6 s after the others, and the
largest gap in that run between a node's first sight of one fork candidate and
of the other is 11 s. Un-audited latency (new8-new10: 302-383 ms run mean) is
unchanged from new3/new4.
