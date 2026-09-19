# RAI epochs, step 2: count-based epochs, per-epoch final state (2026-09-19)

Same build flags, command and machine as `../rai-epochs-step1-2000-45k-2026-09-18`
(`--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks, `caffeinate -i`,
idle machine), plus `--epoch-terminated-elections 22500`: every PR advances to the next epoch
once 22.5k elections of its current epoch got a certificate, so the run spans epochs 0, 1 and
the start of 2. `run_fork_settle.sh` ends when `settle_check.py` reports `SETTLED_CONSISTENT`:
every PR reports every epoch with no open instance and no undecided cemented block, and the
per-epoch final-state hashes (`final_state` RPC) are identical on all PRs.

## What changed (all behind `rai_protocol`)

1. **Epoch advance.** `ActiveElectionsConfig::epoch_terminated_elections` (nanospam flag
   `--epoch-terminated-elections`): the AEC counts the elections of the current epoch which
   got their first certificate and advances when the count is reached; a node also follows
   as soon as more than *f* of the weight votes in a later epoch. Nothing else happens at
   the switch: the instances of the old epoch run on to their termination, this node keeps
   voting in them.
2. **Per-epoch final state.** `EpochStates` records every instance finalized by a
   certificate of its epoch (winner, candidates, this node's own statements). `final_state`
   reports, per epoch, the hash over the finalized blocks plus the settled single
   notarizations, and the counts of pending / undecided-cemented / empty / conflicting slots.
   `confirmation_active` lists the epochs of each root, `confirmation_info` takes the epoch.
3. **Instance convergence.** Every instance that exists on some replica reaches the same
   outcome on every replica:
   - a non-final vote for (block, epoch ≤ current) opens that instance on a node which holds
     the block and has not finalized it in that epoch, also for a cemented block (`Late`);
     in an epoch the node already left it casts the timeout first vote of Kudzu Protocol 1
     lines 22–25 (`VoteKind::Abstain` = `FirstVote(B_timeout)`, duration bits `0xC`) and
     collects the certificates the others produce;
   - a vote of an epoch the node has not reached yet is never `Late`: it waits in the vote
     cache and is replayed when the node advances (`AecFact::EpochAdvanced`);
   - a block whose instance of an earlier epoch still runs is not proposed again in the
     current epoch (scheduler gate and `insert`); one whose instance ended undecided was,
     in this run, proposed again by the backlog scan — the rule was changed right after
     this run, see `../rai-epochs-step2-no-repropose-2000-45k-2026-09-19`;
   - fork candidates join the instances of every epoch; instances survive rollbacks and the
     cementing of their dependency; the aggregator hands out certificate evidence from the
     `EpochStates` record after the election is gone, and a final vote only if this node
     cast it in that epoch; a settled instance stops soliciting 30 s after its last statement
     (`Election::EVIDENCE_WINDOW`).
4. **Vote cache under Kudzu.** Only votes which found no election are cached (an election
   is never dropped and a vote is one-shot, so an applied vote is never needed again), and a
   replayed vote is not hashed and signature-checked a second time.
5. nanospam republishes the genesis setup chain until every PR saw the full quorum.

## Results

| Metric (nanospam, publishing phase = status lines with ≥ 1000 cps) | step 1 (single epoch) | **this run** |
|---|---|---|
| Non-fork blocks confirmed during publishing | 42,579 | **43,002** |
| Confirmation rate | 1851 cps | 1870 cps |
| Median of the per-second averages | 108 ms | **101 ms** |
| Average confirmation time, cps-weighted | 111 ms | 257 ms (see below) |
| Worst second | 157 ms | 1839 ms (the epoch switch, see below) |
| Settle phase | 3 s | 5 s |
| Cemented on every PR | 42,855 | **45,019** |
| Epoch hashes identical on all PRs | – | 0: 21,489 finalized + 609 single, 1: 21,624 + 602, 2: 1,924 + 17 |
| Inconsistencies | 0 | 0 |

Per-second status around the first switch (`EPOCH_ADVANCED` at 23:44:50.14 UTC on all six
PRs within 1 ms):

| second | cps | avg conf time |
|---|---|---|
| 23:44:49 | 1788 | 102 ms |
| 23:44:50 | 1630 | 103 ms |
| **23:44:51** | **2544** | **1839 ms** |
| 23:44:52 | 1955 | 110 ms |
| 23:44:53 | 1672 | 101 ms |

Throughput does not drop at the switch, it rises: the switch second confirms ~800 blocks
more than the steady ~1750 cps. Those are blocks that epoch 0 left undecided (forks which
timed out, and their successors), now proposed again in epoch 1 and finalized there; their
age (seconds) is what lifts the average of that second, while the blocks in flight keep
their ~100 ms. The same happens at the second switch (23:45:02, in the publishing tail).
The step-1 run never confirmed those blocks at all, which is why its weighted average and
its confirmed count are both lower.

## Run history (the `attempts/` logs are not committed)

Runs 1–26 established the termination condition and fixed one convergence hole per run
(see the memory note `rai-epochs-step2-2026-09-18`): votes not opening `Late` instances,
forks joining one epoch only, timed-out/abstained accounting, `final,timeout` reps,
rollbacks erasing instances, the aggregator inventing final votes, dropped rebroadcast of
first votes, the solicitor skipping reps whose final vote is held, own votes dropped in
the loopback lane, a first-vote / vote-cache race at the switch, and two load spirals
(settled instances soliciting forever; per-statement evidence replies). From run 20 on the
runs were `SETTLED_CONSISTENT` but each switch cost a ~3.5 s dip: ~500 cps in the switch
second and 1–1.5 s averages for the next three.

Runs 27–35 traced the dip with temporary diagnostics (fact-processor timing per fact kind,
vote-processor enqueues per delivery, vote-cache replays with vote ages, AEC inserts by
source):

- the AEC fact-queue cooldown was brief (90 ms) and 2 s after the switch: a consequence,
  not the cause;
- in the 5 s around the switch the vote processor handled 15–20k *replayed* votes per PR
  (steady state: ~900), carrying 2–3 M hashes; their ages averaged 3–5 s, up to 20 s —
  the full epoch-0 vote history of blocks whose epoch-1 election was just started;
- those elections were the restart of the in-flight instances at the switch (run 32
  removed it: instances run on in their epoch) and, mainly, the backlog scan re-proposing
  every unconfirmed block whose epoch-0 instance was still in the AEC, since the scheduler
  gate only looked at the current epoch (run 33: a running earlier-epoch instance blocks
  the proposal; replayed votes skip `validate()`) — the switch second went from ~500 to
  ~1840 cps;
- run 34 closed the hole behind two `INCONSISTENT` runs (29, 31: one PR without an instance
  every other PR finalized): votes of a not-yet-reached epoch were cached but never replayed;
- run 35 removed the remaining ~1 s delay of the blocks in flight by caching only unmatched
  votes (the re-proposal of ~2k undecided blocks replayed 12 useless votes each, 50–80 µs
  per fact on the AEC event thread), and kept a future-epoch vote for a block decided in an
  earlier epoch (`Late` → `Indeterminate`).

Run 36 is this run, with the diagnostics removed. Message counts per PR: 5.6–6.1k
`confirm_ack` in (step 1: 10.5k), 17–24k votes through the vote processor (step 1: 18k),
7–14k vote-cache insertions (before run 35: 294k), ~2.9k fork candidate replies, 2.4–3.3k
certificate evidence replies, 1.6–2.9k instances started for a vote, ≤ 3 stale instances,
no AEC cooldown.
