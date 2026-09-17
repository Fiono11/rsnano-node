# RAI rotating close assembler, 2026-09-17

Runs of the baseline (`d45bb315d`, built in a detached worktree into
`target-baseline/`) and the working tree implementing the rotating close
assembler of `RAI.tex` (rules A0 and C0-C4) on the 8-core M1, quiet machine,
with the same workload as `rai-spec-v3-close-2026-09-16`:

```
nanospam --prs 6 --no-prio --fork-percentage 5 --blocks 45000 --accounts 45000 --rate 2000 \
  --epoch-terminated-elections 15000 --closed-epochs 3 --close-timeout 240 --no-kill --data-dir <fresh dir>
```

Release `rsnano` and `nanospam` with `--features rai_protocol`, no close tracing.
`run_one.sh` waits for three quiet `top` samples (`top -l 2`: the `-l 1` gate of
the earlier script always reads 0% and never blocked), deletes the data
directory after every run and dumps the node counters over RPC. `analyze_all.sh`
rebuilds `analysis.txt`; the per-block timelines were trimmed from the archived
results as before.

## What changed

The Sept 15 close was leaderless: each C1 commitment named a parent and a root,
and every replica FIRST-voted the pair that reached certificate weight. Now:

- C1 announcements (kind 10) carry only the previous close and the membership
  root, no parent and no member count, so a proposal can carry them as bare
  signatures.
- The assembler of round `r` is the committee member at position
  `(r + epoch) mod n` in public-key order (A0). When announcement weight
  certifies a root, it pairs that root with the parent the K9 selector picks
  from its certified candidates (newest notarized round with a timeout
  certificate for every later round, smallest id first, else genesis), signs
  one `CloseProp` envelope (kind 11) per round and floods it with the
  announcement signatures that certify the target.
- A replica FIRST-votes, notarizes on second look, and completes a value only
  after C2(a)-(e): the envelope is signed by the round's assembler, the target
  is certified by announcement weight in that round, the previous close is its
  own, ParentOK holds, and it holds the target's members. Past the first-vote
  deadline it votes timeout; a silent assembler costs its round and the next
  assembler closes (unit test `a_silent_assembler_costs_one_round_and_the_next_assembler_closes`).
- Proposals a replica holds are retransmitted with the close history;
  equivocation is bounded to 8 stored proposals per round.

## Results

Close after the last drain, first / last PR; non-fork finalization mean / p95
in ms at PR0's websocket. Every close was round 0 with one hash on all six PRs.

| Run | Epoch 0 close | Epoch 1 close | Epoch 2 close | Latency e0 / e1 / e2 | Avg confirmation |
|---|---:|---:|---:|---:|---:|
| base1 | 1.27 / 1.44 s | 6.46 / 6.71 s | 0.69 / 0.91 s | 308/644, 174/319, 412/706 | 372 ms |
| base2 | 10.06 / 10.41 s | 2.92 / 3.18 s | 2.37 / 2.53 s | 301/606, 432/852, 586/959 | 550 ms |
| base3 | 2.44 / 2.60 s | 7.93 / 8.05 s | 0.42 / 0.52 s | 251/585, 238/391, 415/638 | 377 ms |
| new1 | 2.94 / 3.08 s | 2.73 / 3.20 s | 2.34 / 2.47 s | 222/447, 200/424, 818/1402 | 542 ms |
| new2 | 1.54 / 1.67 s | 2.94 / 3.13 s | 2.49 / 2.61 s | 172/338, 203/410, 423/720 | 333 ms |
| new3 | 2.49 / 2.68 s | 2.83 / 4.71 s | 1.27 / 1.35 s | 239/607, 260/559, 373/604 | 360 ms |

The assembler is performance-neutral: latency, election counts, vote traffic
and `epoch_close` packet counts (589-643 per PR on both builds) are inside the
baseline spread. The `EPOCH_CLOSE_PROGRESS` reports show the rotation working
(epoch 0 round 0 proposed by the first committee key, epoch 1 by the second,
epoch 2 by the third; one proposal and one candidate per round).

Three of the nine baseline closes waited out the 6 s announcement fallback for
a straggler (base1 epoch 1: one PR with `EPOCH_DRAIN_WAIT pending: 2`; base3
epoch 1: one PR three members behind, `only_theirs: 3`; base2 epoch 0: one PR
one member behind), none of the nine assembler closes did; that fallback path
is unchanged, so this is drain variation, not the assembler. new1's epoch-2
latency comes from the last publication window (the spam stops at ~25 s) with
a 2.3 s close, the same tail shape as base2.

`loaded-*` are the first two runs, made while a Chrome renderer spun at 100%
of one core: both builds came out ~6x slower (2.3-2.8 s average confirmation),
the confound recorded in `rai-2000-5runs-2026-09-16`.
