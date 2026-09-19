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
- **The fork stall, found and fixed** (diagnostic runs 22–33 in `attempts/`, 6 s epochs):
  nanospam sends a fork to every second node, so each half of the PRs proposes a
  different block for the root and needs the other candidate to take its second look.
  Under load that candidate took 5–10 s to arrive on some PR: (1) the fork-candidate
  reply went on the 64-slot `BlockBroadcastInitial` queue the 2000 bps flood fills, and the
  reply cache counted it as sent even when dropped, so the next request got nothing for
  2 s; (2) an instance not yet terminated was re-solicited only every 5 × base latency
  (5 s); (3) a re-delivered block is byte-identical to the flooded copy and fell to the 5 s
  publish duplicate cutoff whenever the first sighting was wasted — the fork cache was
  filled *after* the fork inserter ran, so an election started in between missed it;
  (4) a representative's epoch-e+1 first vote replaced its cached epoch-e first vote for
  the same block, so a replica that got the block late never opened the epoch-e instance.
  Now: evidence blocks go on the reply queue, flagged `is_evidence` in the Publish header
  and exempt from the duplicate filter, recorded only when queued; an unterminated
  instance is re-solicited every base latency; the fork cache is filled before the
  plugins run; cached statements are keyed by (kind, epoch). Fork termination max fell
  from 7.2 s to 0.36–0.49 s, and every instance of an epoch terminates within its drain.
- Support: the dependency gate on first votes (a block is proposed only once its
  dependencies are finalized), the vote cache replayed when a block arrives, the winner
  re-broadcast to every PR lacking a vote, solicitation urgencies (`Now` while draining),
  slot states outliving their elections (dropped per epoch once the next epoch is agreed),
  `WalletRepsChecker` every 500 ms under RAI.

Earlier attempts of the day are in `attempts/` (gitignored, 33 of them): the epoch 0 timer
started during setup; the closed-at-agreement discard raced with the next epoch's
re-decision (two PRs rolled back what four had cemented); a certificate arriving before
the PR knew its own value never agreed; cached votes for blocks not yet held were never
replayed while draining; a rollback refused because the scheduler had re-queued the block;
a full-list `EpochEntries` reconciliation, rejected for scale and removed.

## Results

`kudzu-run.log` is the run with everything above. Per epoch, identical on all six PRs
(`SETTLED_CONSISTENT after 0s`, `CLOSED_CONSISTENT after 0s`), every PR's own final value
equal to the finalized one, every close in round 0 within ~1.2 s of the epoch's end (the
fourth epoch holds the 58 blocks confirmed after publishing ended):

| Epoch | State hash | Finalized | Conflicting | Ended → left (all PRs) | Closed |
|---|---|---|---|---|---|
| 0 | `2C211AC8…` | 14,565 | 450 | T0+8.01 → +8.21…8.32 s | +9.01 s (round 0) |
| 1 | `C6C4CD67…` | 14,625 | 511 | +16.00 → +16.18…16.40 s | +17.15 s (0) |
| 2 | `C983B3E2…` | 14,499 | 342 | +24.00 → +24.17…24.36 s | +25.47…25.54 s (0) |
| 3 | `ED60BBDF…` | 58 | 2 | +32.06 → +32.15 s | +32.21…32.27 s (0) |

nanospam status lines with ≥ 1000 cps (23 s):

| Metric | step 3 (2 epochs by count) | proposal at once | proposal rule only | **this run** | count, 3 × 15k (sibling) |
|---|---|---|---|---|---|
| Confirmation rate | 1854 cps | 1864 cps | 1858 cps | **1853 cps** | 1861 cps |
| Median of the per-second averages | 96 ms | 103 ms | 98 ms | **98 ms** | 102 ms |
| Average confirmation time, cps-weighted | 104 ms | 190 ms | 159 ms | **148 ms** | 188 ms |
| Switch seconds (0→1, 1→2) | 146 ms | 237, 165 ms | 127, 1201 ms | **132, 944 ms** | 148, 1569 ms |
| Closes (round) | 0, 0 | 1, 1, 1 | 0, 0, 0 | **0, 0, 0, 0** | 0, 0, 0 |
| Close after the epoch ended | 4.5 s, 0.2 s | 7.5, 6.1 s | 0.9, 1.1, 1.5 s | **1.0, 1.2, 1.5, 0.2 s** | 1.0, 1.1, 0.1 s |
| Cemented on every PR | 42,855 | 43,786 | 43,517 | 43,748 | 43,371 |

Node side, per PR: non-fork finalization p50 84–86 ms, p95 119–125 ms, p99 141–244 ms;
fork termination p50 128–130 ms, p99 299–408 ms, max 355–489 ms; 4–5 close rounds
entered; 1,343–2,714 instances started for a vote; 0 discarded; 981–1,276 fork rollbacks.

With 6 s epochs (attempt 33, the same code) the four closes land 0.6–1.2 s after each
epoch ends, the drains take 120–250 ms and the worst second is 767 ms; before the fork
fix, the same configuration stalled the pipeline for 4 s (attempt 23).

## Open: the second switch

The 0→1 switch costs ~150 ms of average latency; every later switch costs 0.6–1.6 s in
one second (blocks published around the boundary). Not the AEC lock (attempt 22), not
the drain (180–400 ms at both switches), not stuck instances any more. Undiagnosed.
