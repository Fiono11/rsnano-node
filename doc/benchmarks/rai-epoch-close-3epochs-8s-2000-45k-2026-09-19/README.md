# RAI epochs, step 4: timed epochs, the final vs the finalized epoch hash (2026-09-19)

Same build type, command and machine as `../rai-epoch-close-2000-45k-2026-09-19`
(`--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks, `run_fork_settle.sh`),
with the epochs ended by **time**: `--epoch-duration-ms 8000`, three epochs of 8 s over the
~23 s of publishing. The sibling record `../rai-epoch-close-3epochs-15k-2000-45k-2026-09-19`
is the same code with the epochs ended by count.

## What changed since step 3

- **Pre-epoch setup.** nanospam sets its accounts up (no epoch running, the genesis key is
  the only voter), waits for the full quorum, and only then tells every PR to start epoch 0
  at once (`epoch_start` RPC, T0). Epoch k ends at `T0 + k·D` on every PR, the boundary
  checked on every tick and every vote (`EPOCH_ENDED` within 1–3 ms of each other).
- **Sequential epochs, as before**: an ended epoch drains (no new instance is started in it),
  is left once every instance holds a certificate and the epoch before is closed
  (`EPOCH_ADVANCED`, the next epoch starts), and its close election runs from then on.
  A PR that sees more than f of the weight ahead ends its epoch, and one that sees a
  certificate's weight ahead while draining leaves at once.
- **A PR still opens an instance of epoch e after leaving it** if a vote of epoch e arrives
  for a block without one (a peer still in e); the block gets a timeout vote here, and the
  epoch's value is **updated between rounds** as such instances terminate. The **final epoch
  hash** is the value once no instance of e can be opened any more; the **finalized epoch
  hash** is whatever value a round finalized, possibly earlier.
- **The epoch state hashes every notarized or finalized block** of the epoch's instances,
  both blocks of a conflicting slot included. Conflicts are not discarded; a block in the
  finalized hash a replica does not hold arrives by the legacy missing-block path.
- **The only discard**: an instance of a closed epoch opened *after* the close certificate
  was seen here is late; whatever it notarizes is not in the finalized hash and is rolled
  back (`Ledger::roll_back_batch_unchecked`, the block taken out of the schedulers and the
  vote cache, its instances in every epoch erased: `EPOCH_DISCARDED`). A late instance that
  finalizes is discarded the same way, never cemented. Neither run discarded anything.
- Rules from the user, in force: the close of e starts only after e's duration ended and
  all e instances terminated; e−1 must be closed; e+1 starts when e starts closing; a PR
  starts a new instance of e only while in e or for a vote of e; the finalized-vs-final
  hash reconciliation is deferred.
- Support: the dependency gate on first votes (a block is proposed only once its
  dependencies are finalized), the vote cache replayed when a block arrives, the winner
  re-broadcast to every PR lacking a vote, solicitation urgencies (`Now` while draining),
  slot states outliving their elections (dropped per epoch once the next epoch is agreed),
  `WalletRepsChecker` every 500 ms under RAI.

Seventeen earlier attempts of the day are in `attempts/` (gitignored): the epoch 0 timer
started during setup; the closed-at-agreement discard raced with the next epoch's
re-decision (two PRs rolled back what four had cemented); a certificate arriving before
the PR knew its own value never agreed; cached votes for blocks not yet held were never
replayed while draining; a rollback refused because the scheduler had re-queued the block;
a full-list `EpochEntries` reconciliation, rejected for scale and removed.

## Results

Per epoch, identical on all six PRs (`SETTLED_CONSISTENT after 0s`,
`CLOSED_CONSISTENT after 0s`), every PR's own final value equal to the finalized one:

| Epoch | State hash | Finalized | Single | Conflicting | Ended → left | Values converged | Closed (round) |
|---|---|---|---|---|---|---|---|
| 0 | `3A66C92B…` | 14,801 | 0 | 284 | T0+8.00 → +8.24…8.50 s | +8.75 s | +15.50 s (1) |
| 1 | `20A500E5…` | 14,875 | 0 | 302 | +16.00 → +16.18…16.38 s | +16.53 s | +22.13 s (1) |
| 2 | `2905179B…` | 14,109 | 376 | 305 | +24.00 → +24.10…24.33 s | ~+24.5 s | round 1 |

Every close took a second round: the round-0 leader proposes the value it holds when it
enters the round (right after leaving), while the instances opened during the 250–500 ms
drain skew still terminate on the other PRs — three distinct values were seen in the first
half second of every close, all converging to one value within ~0.5 s. The followers
abstain at the 5 s round timeout, and round 1's leader proposes the converged value, which finalizes at once. The close therefore costs one round timeout per
epoch (5–7 s after the epoch is left) but never a wrong value. The step-3 run closed in
round 0 because its values agreed at once; run-to-run.

nanospam status lines with ≥ 1000 cps (23 s):

| Metric | step 3 (2 epochs by count) | **timed, 3 × 8 s** | count, 3 × 15k (sibling) |
|---|---|---|---|
| Confirmation rate | 1854 cps | **1864 cps** | 1859 cps |
| Median of the per-second averages | 96 ms | **103 ms** | 105 ms |
| Average confirmation time, cps-weighted | 104 ms | **190 ms** | 200 ms |
| Switch seconds | 146 ms | **237 ms, 165 ms** | 176 ms, 157 ms |
| Worst second | 171 ms | **1349 ms** (T0+21 s) | 1915 ms (T0+22 s) |
| Cemented on every PR | 42,855 | 43,786 | 43,307 |
| Settle phase | 6 s | 0 s | 6 s |

Node side, per PR: non-fork finalization p50 89–91 ms, p95 118–126 ms, p99 129–270 ms;
3 epochs ended, left and closed; 6–7 close rounds entered; 1,144–1,671 instances started
for a vote; 0 discarded; 1,018–1,113 fork rollbacks (the losing forks).

**Open**: both runs have one second with a ~1.3–1.9 s average about 21–22 s after T0, i.e.
around the round-0 timeout and round-1 close of epoch 1's close election (the switch
seconds themselves cost 160–240 ms). Epoch 0's close, which also went to round 1, shows
no such second. Not diagnosed.
