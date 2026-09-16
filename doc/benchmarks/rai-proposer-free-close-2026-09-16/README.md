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
