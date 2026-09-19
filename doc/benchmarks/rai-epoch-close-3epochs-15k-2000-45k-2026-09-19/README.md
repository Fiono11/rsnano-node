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
every PR's own final value equal to the finalized one, every close in round 0:

| Epoch | State hash | Finalized | Single | Conflicting | Ended → left | Closed (round) |
|---|---|---|---|---|---|---|
| 0 | `FE8A8EB7…` | 14,880 | 0 | 219 | T0+8.05 → +8.17…8.32 s | +9.02 s (0) |
| 1 | `63A86DA4…` | 14,380 | 342 | 426 | +15.99 → +16.19…16.37 s | +17.10 s (0) |
| 2 | `6F0A17EB…` | 14,110 | 311 | 384 | `epoch_advance` (+40.07) → +0.01 s | +0.15 s (0) |

Epoch 2 never reached 15,000 decided elections (14,805 instances) and was ended by
`settle_check.py` once the states were settled and identical.

nanospam status lines with ≥ 1000 cps (23 s): 1861 cps, median of the per-second averages
102 ms, cps-weighted average 188 ms, switch seconds 148 ms and 1569 ms (the second
switch, see the sibling's README), 43,371 cemented on every PR.

Node side, per PR: non-fork finalization p50 85–87 ms, p95 123–138 ms, p99 177–204 ms;
fork termination p50 125–127 ms, max 346–493 ms; 3–4 close rounds entered; 1,925–3,234
instances started for a vote; 0 discarded; 875–1,349 fork rollbacks (the losing forks).
