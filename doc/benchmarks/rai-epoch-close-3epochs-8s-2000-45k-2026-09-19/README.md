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
- **The leader proposes once its value can be trusted**: at once if every instance of
  the epoch has settled here (the value is final but for a late instance), otherwise once
  the value has stood for 1 s (`EpochClose::PROPOSAL_DELAY`). A value proposed the instant
  the round is entered lost round 0 in every close of the first run of this record: the
  instances of the drain skew were still terminating on the other PRs, whose values moved
  on and abstained at the 5 s timeout. Waiting for settlement alone is worse (attempt 20):
  settlement can take 10 s, and the next epoch can not be left before the close.
- Support: the dependency gate on first votes (a block is proposed only once its
  dependencies are finalized), the vote cache replayed when a block arrives, the winner
  re-broadcast to every PR lacking a vote, solicitation urgencies (`Now` while draining),
  slot states outliving their elections (dropped per epoch once the next epoch is agreed),
  `WalletRepsChecker` every 500 ms under RAI.

Earlier attempts of the day are in `attempts/` (gitignored, 23 of them): the epoch 0 timer
started during setup; the closed-at-agreement discard raced with the next epoch's
re-decision (two PRs rolled back what four had cemented); a certificate arriving before
the PR knew its own value never agreed; cached votes for blocks not yet held were never
replayed while draining; a rollback refused because the scheduler had re-queued the block;
a full-list `EpochEntries` reconciliation, rejected for scale and removed.

## Results

`kudzu-run.log` is the run with the proposal rule above. Per epoch, identical on all six
PRs (`SETTLED_CONSISTENT after 0s`, `CLOSED_CONSISTENT after 0s`), every PR's own final
value equal to the finalized one, every close in round 0:

| Epoch | State hash | Finalized | Single | Conflicting | Ended → left | Closed (round) |
|---|---|---|---|---|---|---|
| 0 | `4198E227…` | 14,783 | 0 | 359 | T0+8.00 → +8.19…8.30 s | +8.90 s (0) |
| 1 | `2031AD44…` | 14,759 | 0 | 443 | +16.00 → +16.13…16.30 s | +17.09 s (0) |
| 2 | `3FCB5FC1…` | 13,974 | 327 | 407 | +24.00 → +24.02…24.30 s | +25.51 s (0) |

nanospam status lines with ≥ 1000 cps (23 s):

| Metric | step 3 (2 epochs by count) | first run (proposal at once) | **this run** | count, 3 × 15k (sibling) |
|---|---|---|---|---|
| Confirmation rate | 1854 cps | 1864 cps | **1858 cps** | 1859 cps |
| Median of the per-second averages | 96 ms | 103 ms | **98 ms** | 105 ms |
| Average confirmation time, cps-weighted | 104 ms | 190 ms | **159 ms** | 200 ms |
| Switch seconds (0→1, 1→2) | 146 ms | 237 ms, 165 ms | **127 ms, 1201 ms** | 176 ms, 157 ms |
| Worst second | 171 ms | 1349 ms (T0+21 s) | **1201 ms** (T0+17 s) | 1915 ms (T0+22 s) |
| Closes (round) | 0, 0 | 1, 1, 1 | **0, 0, 0** | 0, 1, 0 |
| Cemented on every PR | 42,855 | 43,786 | 43,517 | 43,307 |

Node side, per PR: non-fork finalization p50 87–89 ms, p95 112–115 ms, p99 123–153 ms;
3 epochs ended, left and closed; 3–4 close rounds entered; 670–2,111 instances started
for a vote; 4 PRs left one epoch on the quorum-ahead rule; 0 discarded; 1,032–1,238 fork
rollbacks (the losing forks).

## The second with a ~1.2–1.9 s average

Every run of this record has one such second, covering the blocks published in the last
second of epoch 1, confirmed ~1.5 s late; the 0→1 switch costs 110–240 ms. Findings from
the diagnostic runs in `attempts/` (22: AEC lock timings, 23: 6 s epochs):

- Not the AEC lock: no tick held it over 30 ms, the vote-cache replay on advance is
  instant (255–722 hashes), dropping an epoch's slot states takes 4–8 ms.
- The PRs that leave an epoch on the quorum-ahead rule (a certificate's weight already
  left) do so with instances still unterminated: blocks in flight at the boundary land in
  epoch e on some PRs and in e+1 on the others, so the e-instances of the former get no
  votes from the latter and terminate only on the timeout path. Their epoch value moves
  until then, and their close round is entered late. With 8 s epochs it is a second of
  latency; with 6 s epochs (attempt 23) epoch 0's close took 9.6 s — the followers' abstains
  came 4 s late, round 1 closed at once — and epoch 1, ended at +12 s, could not be left
  before that: 4 s at 0 cps, then catch-up at 3–5 s latency.
- The close of epoch e must finish well within the duration of e+1, or the sequential
  rules stall the pipeline. Proposing only when settled (attempt 20) made it worse for
  the same reason.

Not fixed: the boundary handling of in-flight blocks is the next thing to look at.
