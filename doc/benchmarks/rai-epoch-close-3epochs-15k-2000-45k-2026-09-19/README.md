# RAI epochs, step 4: three epochs ended by count (2026-09-19)

The count-triggered sibling of `../rai-epoch-close-3epochs-8s-2000-45k-2026-09-19`, same
code, build type, command and machine: 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks,
`--epoch-terminated-elections 15000` — an epoch ends once 15,000 of its elections are
decided (or once more than f of the weight is seen ahead). See the sibling's README for
the model (pre-epoch setup, sequential epochs, final vs finalized epoch hash, the late
discard). An earlier run of the day with a previous code state is in `attempts/`
(gitignored); its counts differ (four epochs, conflicts left undecided).

## Results

Identical on all six PRs (`SETTLED_CONSISTENT after 0s`, `CLOSED_CONSISTENT after 5s`),
every PR's own final value equal to the finalized one:

| Epoch | State hash | Finalized | Single | Conflicting | Ended → left | Closed (round) |
|---|---|---|---|---|---|---|
| 0 | `A74F80C4…` | 14,936 | 0 | 273 | T0+8.09 → +8.22…8.47 s | +8.83 s (0) |
| 1 | `BCEBF9D4…` | 14,391 | 413 | 313 | +16.06 → +16.21…16.39 s | +22.31 s (1) |
| 2 | `A4B16402…` | 13,979 | 342 | 405 | `epoch_advance` (+40.15) → +0.00 s | +5 s (0) |

Epoch 2 never reached 15,000 decided elections (14,726 instances) and was ended by
`settle_check.py` once the states were settled and identical; its close then closed in
round 0 within the check's 5 s poll. Epoch 0 closed in round 0 (the values agreed at
once), epoch 1 needed round 1 (three distinct values in the first 200 ms after leaving,
the round-0 leader's among them; converged 0.6 s later, finalized after the 5 s timeout).

nanospam status lines with ≥ 1000 cps (23 s): 1859 cps, median of the per-second averages
105 ms, cps-weighted average 200 ms, switch seconds 176 ms and 157 ms, worst second
1915 ms at T0+22 s (the same unexplained second as in the timed run, around the round-0
timeout of epoch 1's close), 43,307 cemented on every PR.

Node side, per PR: non-fork finalization p50 88–89 ms, p95 120–129 ms, p99 137–224 ms;
3 epochs ended, left and closed; 3–5 close rounds entered; 979–1,815 instances started
for a vote; 0 discarded; 967–1,281 fork rollbacks (the losing forks).
