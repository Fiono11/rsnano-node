# RAI close rules from the audited proof (v3), 2026-09-16

Runs of the baseline (`1af95fc53`, built in a detached worktree) and the working
tree implementing rules from `RAI_Semantic_Epoch_Closure_Proof_Audited_Revised_v3`
on the 8-core M1, quiet machine (`run_one.sh` waits for three quiet `top` samples,
deletes the data directory after every run, and dumps the node counters over RPC
before killing the nodes):

```
nanospam --prs 6 --no-prio --fork-percentage 5 --blocks 45000 --accounts 45000 --rate 2000 \
  --epoch-terminated-elections 15000 --closed-epochs 3 --close-timeout 240 --no-kill --data-dir <fresh dir>
```

Release `rsnano` and `nanospam` with `--features rai_protocol`, no close tracing.
`analyze_all.sh` rebuilds `analysis.txt`; the per-block timelines were trimmed
from the archived results as in the earlier folders.

## Builds

| Label | Contents |
|---|---|
| `base*` | `1af95fc53` |
| `c1`, `c2` | **kept**: X1 round exit on a complete candidate, ParentOK (K6), earliest-ancestor decision (#1); uncapped round budget (#2); one C1 commitment per round, kind-10 message (#4) |
| `new1`-`new8`, `noretry1` | the above plus the Remark-1 identical-block retry (#3), in successive variants; **reverted** |

## Results, kept build vs baseline

Close after the last drain, first / last PR; non-fork finalization mean / p95 in
ms at PR0's websocket.

| Run | Epoch 0 close | Epoch 1 close | Epoch 2 close | Latency e0 / e1 / e2 |
|---|---:|---:|---:|---:|
| base1 | 4.70 / 4.83 s | 2.57 / 2.94 s | 3.48 / 3.58 s | 169/336, 175/332, 519/886 |
| base2 | 6.09 / 7.18 s | 2.97 / 3.17 s | 2.64 / 2.83 s | 222/585, 160/281, 420/645 |
| base3 | 4.48 / 5.26 s | 7.69 / 8.04 s | 5.98 / 6.22 s | 283/595, 234/395, 344/561 |
| base4 | 1.82 / 1.97 s | 2.57 / 2.78 s | 2.39 / 2.50 s | 244/461, 324/639, 420/803 |
| c1 | 2.70 / 2.90 s | 4.77 / 4.92 s | 8.81 s, 4 PRs | 255/581, 313/523, 494/918 |
| c2 | 2.53 / 3.95 s | 2.74 / 3.15 s | 8.23 / 8.57 s | 250/601, 201/424, 458/700 |

Every close was round 0 with one hash (the new `local_round` field in
`EPOCH_CLOSED` shows a PR that had already exited round 0 on notarization
before it learned finality). Latency, election counts, vote processing and
request traffic of `c1`/`c2` are inside the baseline spread (`c2`: 202k vote
batches processed against 206-235k for the bases). The two slow epoch-2 closes
are drain and recovery stragglers, not close rounds: in `c2` one PR became
close-ready 6 s after the others (`EPOCH_DRAIN_WAIT pending: 2`), so the other
five committed at the 6 s fallback; in `c1` two PRs lacked one member of the
finalized close (`only_theirs: 1`) and were still soliciting it 270 s later,
the recovery shape described in `rai-proposer-free-close-2026-09-16`.

The round-exit rule (#1) never fired in these runs beyond the cosmetic
`local_round`; it matters when a notarized value misses its FINAL quorum with
too few timeouts to trigger timeout shares, which the unit test
`a_stalled_notarized_round_closes_through_its_descendant` reproduces.

## Why the identical-block retry (#3) was reverted

The proof's Remark 1 lets a closed epoch's single retained member be
FIRST-voted again in a later epoch (R1 rejects only slot-conflicting
candidates). Implemented as (a) the vote-state lock relaxation, (b) `decided_before`
admitting the identical member, (c) an election opened for each stuck member at
the close, and (d) peers joining such an election on a vote for it, the runs
`new1`-`new8` regressed epochs 1 and 2 to 300-1400 ms mean, with the same
signature in every variant: 2.7x the vote batches processed, +21k elections
started, 1.7x elections dropped unconfirmed, +50-60% request hashes. `noretry1`
(c disabled) shows the cost is in (a)/(b)/(d), not the close-time insertion.

Traced diagnostics (`RAI_CLOSE_TRACE_DIR`, 1000/s) showed the shape: with a
count-based drain the six PRs pass the epoch count 0.1-0.4 s apart, so every
block published in that window gets FIRST from the three slower PRs in epoch e
and FIRST-timeout plus second-look notarization from the three faster ones, is
notarized but unfinalizable in e (3 < 4 FINAL voters), and becomes a retained
member. There are 100-400 such members per close. Under the relaxed rule they
are elected again in e+1, where the faster PRs FIRST-timeout them again
(their e+1 election already ended or the root now carries a fork candidate), so
they are retried at every close and never finalize; 208 of 564 retried hashes
in the diagnostic were retried at both closes. Under the baseline rule those
boundary blocks stay unconfirmed, cheaply. Making them finalize needs the
epoch boundary itself handled (an agreed cut instead of six local counts, or a
catch-up FIRST for the faster PRs), not the R1 relaxation alone.
