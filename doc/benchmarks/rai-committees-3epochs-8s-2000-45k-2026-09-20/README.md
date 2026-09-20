# RAI epochs, step 5: per-epoch committees, joint counting across the close (2026-09-20)

Same build type, command and machine as `../rai-epoch-close-3epochs-8s-2000-45k-2026-09-19`
(`--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks, three epochs of 8 s,
`run_fork_settle.sh`), with the **voting weights changing per epoch**.

## The rules

- The committee's members are the PRs, the same throughout the run; the weight of each is the
  balance of the accounts delegating to it, as of the state one epoch finalized. Every account
  delegates to a PR (nanospam: `Representatives::of(account)`, fixed by the account's key; a
  fork names another PR; `--change` picks a random one; the initial spam amount is fanned out to
  one seed account per PR at setup).
- Epoch e counts its instances in the committee derived by epoch e−2. Epochs 0 and 1 count in
  the **genesis committee**: the ledger at the end of the setup, snapshotted by the `epoch_start`
  RPC on every PR (nanospam first waits for every PR to hold the same cemented ledger).
- Epoch e+1 is **joint** while epoch e is still closing: its instances count in C(e−1) and
  C(e−2) both, and in C(e−1) alone once e is closed. Joint: a block is notarized / finalized
  once both committees do; the instance times out once one of them does; it is settled once one
  of them can not notarize more; many first votes (second look, timeout rule) in one is enough.
  `previous_epoch_closed(e+1)`, the gate that already runs the closes one after the other, is
  the joint-vs-single predicate. The close of e counts in C(e−2) alone.
- A closed epoch's committee is derived when the close **agrees** here (this PR's value is the
  finalized one): for every account with a block in the epoch's state, the block at its greatest
  height, if there is exactly one at that height, delegates its balance to its representative;
  an account's newer frontier replaces what was counted for it before. The blocks are read from
  the instances themselves (`FinalizedInstance.delegation`, the candidates an election holds).
- When an epoch's committees change — the epoch before closed, or a committee got derived —
  its open instances are **counted again** (`recount_epoch`, `EpochClose::recount`): the
  certificates a single committee supports form on that count, on every replica alike. Votes of
  an epoch whose committee is not known yet wait in the pool.

## Runs

- **Run 1** (`attempts/run1-ledger-race.log`): committees derived in the AEC fact processor by
  reading the frontier blocks from the ledger. `EPOCH_COMMITTEE_MISSING` twice: a fork's winner
  was not inserted yet when the close agreed, that PR skipped the account and derived a
  different committee (PR2 for epoch 0, PR1 for epoch 2), PR2 never agreed on epoch 2 and had no
  C(2). `COMMITTEES_INCONSISTENT [0, 1, 2, 3]`, epoch 2 state inconsistent on PR2.
- **Run 2** (`attempts/run2.log`): derivation moved into the AEC, from the block bodies the
  elections hold. `SETTLED_CONSISTENT`, `CLOSED_CONSISTENT`, `COMMITTEES_CONSISTENT`: genesis,
  C(0), C(1), C(2) with the same digest on all six PRs, no block missing.
- **Run 3** (`attempts/run3-loser-hash-race.log`): PR2 never agreed on epoch 0 — its epoch-0
  *entries* (the 15427 finalized blocks) and the certificate sets of the 318 conflicting roots
  were identical to the other PRs', but its state **hash** differed. The hash covered every
  notarized block of a finalized instance, and a losing fork candidate's notarization
  certificate races the finalization certificate: the instance is erased the moment it
  finalizes, so one replica records `[winner, loser]` and another `[winner]`. PR2 led round 0
  of the epoch-0 close with its own value (round timed out, round 1 closed it), never agreed,
  never derived C(0), and every epoch-2 instance on it pooled votes it could not count: 14117
  pending, cemented frozen at 30384 = epochs 0 + 1. Before committees this was a tolerated
  "own value differs" (step 4); now a lost agreement is a dead replica two epochs later.
  **Fix:** the epoch hash and the derivation take the *winner* of a finalized instance only —
  the loser is not what the epoch decided, and counting it also made every finalized fork slot
  "two blocks at that height", skipped by the derivation. The safety check found nothing but
  the stall (S4 `not cemented on PR2`); S1, S2, S5 clean, S3 = 0.
- **Run 4** (`kudzu-run.log`, this record): winner-only hash. `SETTLED_CONSISTENT`,
  `CLOSED_CONSISTENT`, `COMMITTEES_CONSISTENT`, `SAFE`: 43962 finalized slots, none with two
  blocks in the state, S3 = 0, nothing discarded, 300 sampled slots cemented at their height on
  every PR; genesis, C(0), C(1), C(2) with the same digest on all six PRs. The epoch-0 close
  took two rounds on four PRs (the drain-skew values converge within the first round), the
  others closed in round 0.

Run 2's committees, as an example of the drift:

```
committee genesis: n=3.403e+38  xfwtat7m=20.2% ohsmj9th=16.0% 8g5mp8f1=16.0% 5pddzijp=16.0% 3tfyyzs3=16.0% rxk7n7ne=16.0%
committee       0: n=3.087e+38  rxk7n7ne=17.3% ohsmj9th=17.1% 8g5mp8f1=17.0% xfwtat7m=16.6% 3tfyyzs3=16.5% 5pddzijp=15.5%
committee       1: n=3.044e+38  ohsmj9th=17.6% 8g5mp8f1=16.9% rxk7n7ne=16.7% 3tfyyzs3=16.6% xfwtat7m=16.2% 5pddzijp=15.9%
committee       2: n=3.003e+38  ohsmj9th=17.6% 8g5mp8f1=16.9% rxk7n7ne=16.9% 3tfyyzs3=16.6% xfwtat7m=16.1% 5pddzijp=16.0%
```

n is not constant: funds sent and not yet received belong to no account, so 8–12 % of the
weight is in flight at 2000 blocks/s. The thresholds are shares of each committee's own n.

## Results (nanospam status lines with ≥ 1000 cps)

|                                           | step 4 (2026-09-19) | run 2       | run 4 (this record) |
|-------------------------------------------|---------------------|-------------|-------------|
| Confirmation rate                         | 1853 cps            | 1890 cps    | **1884 cps** |
| Median of the per-second averages         | 98 ms               | 102 ms      | **105 ms**  |
| Average confirmation time, cps-weighted   | 148 ms              | 213 ms      | 191 ms      |
| Switch seconds (0→1, 1→2)                 | 132, 944 ms         | 424, 629 + 1225 ms | 413, 421 + 1074 ms |

Steady-state throughput and latency are at the step-4 level. The 1→2 switch is the first with
a joint phase (epoch 2 counted jointly for ~1 s until epoch 1 closed) and its seconds are worse
than in the step-4 run in both runs; two runs against one, so this is a hint, not a measurement.

## Faulty representatives (2026-09-20, later the same day)

nanospam got two options, combinable: `--offline p` (the last *p* PRs are funded but run no
node and never vote: p of the thresholds) and `--byzantine f` (the last *f* PRs run no node
either; nanospam holds their keys and votes with them at random - any kind, any epoch 0-3,
1-3 recently published hashes, 8 votes per 50 ms: f). PR0, the genesis representative, is
always honest. Every check takes the node count from `PRS`.

`--offline 1` passed every check at once. `--byzantine 1` stalled the run outright and, once
that was fixed, kept turning up latent bugs of the Kudzu rules that six equal honest
representatives had masked - with six, every split has a ≥ 62 % side; with five voters it
does not:

1. **`maxVotes` counted the timeout block.** A 3-abstain / 2-propose split (52 % / 32 %)
   deadlocked: the proposers never timed out because `allVotes − maxVotes` was measured
   against the abstains. Kudzu §4.7 defines maxVotes over non-timeout blocks. Fixed in
   `should_timeout`.
2. **A representative's weight counted twice in the settled predicate.** A final vote without
   a first vote (the Byzantine rep's) put its weight into the loser's tally *and* into the
   "weight that may still first vote it"; a notarized fork never settled anywhere. Fixed in
   `max_notar_weight`.
3. **An exited representative counted as able to first vote.** A final vote without a first
   vote (Kudzu line 11, legitimate for an honest replica whose first vote went to another
   epoch's instance of the same block) is an exit; its weight can not first vote the loser
   any more. Fixed in `is_settled`: what may still first vote is the unknown weight plus the
   known representatives with neither a first vote nor a final one.

And the rules that changed, all four the user's choice after the diagnosis:

- **Late by content, not by time.** An instance of an *agreed* epoch that notarizes a block
  the agreed value does not hold is late. The content is the set of (account, height, block)
  the value hashed, snapshotted when this node agrees - identical on every agreeing replica,
  unlike the instant the close certificate was seen here, which made the same instance late on
  one PR and timely on another (363 late discards in one run, 43 rolling back blocks the other
  PRs held as decided; nine attested values in one close). Until a node agrees nothing is
  late: an instance it lacks may be part of the value.
- **No instance of an agreed epoch is started for a vote**; such a vote is late, not cached.
- **Slot states of an epoch go when it is agreed** (but for the slots with a live instance,
  which go with their election), not two closes later: a re-opened instance had found no
  state and a node had voted first *and* abstain in one instance.
- **A late instance never discards a block finalized in another epoch** (`finalized_kept`), nor
  a cemented one (`cemented_kept` in `EPOCH_DISCARDED`, which now lists the hashes so that S5
  can read them).
- **Committees derive in epoch order**: a replica that agreed on epoch e+1 before epoch e (its
  own value differed there for a while) derived C(e+1) without e's frontiers - a digest the
  others did not hold. Out-of-order agreements wait.

What a Byzantine representative can still do, by the rules: keep a fork instance unsettled
for good, by first voting the winner and never exiting (it may take a second look at the
loser, so no replica can call the instance settled); and get a fork's *loser* single-notarized
in a second epoch's instance with three honest first votes and its own. Neither touches the
ledger (S1 holds, S5 holds), both pollute that epoch's state. The settle check therefore
accepts pending instances that are identical and stable on every PR (`ALLOW_PENDING`), and
S2 reports rather than fails. Open: whether an epoch's state should exclude a single-notarized
block at a slot finalized differently in another epoch; and one `--offline 1` run whose
epoch-0 drain waited ~60 s on two fork instances, not reproduced in four further runs.

One more rule came out of verifying the four: **a replica agrees on either of two values** -
the one it attests (the instances opened before it saw the close certificate, which stands
still once the epoch is closed) or the state with every instance counted. Judging lateness by
content alone had removed the cutoff from the attested value, and an honest run then never
agreed on an epoch: a node's value kept moving with the instances straddling the switch. The
frozen value lets those nodes agree; the all-instances value lets a node that lacked blocks
at the close agree once they came. Whichever matches is the agreed content.

The double finalization S3 reports, 0 in every honest run, is routine under a Byzantine
representative (700-1300 blocks per run): its votes open a block's instance in every epoch.

### Verification (every check, `run_faulty.sh`, after all fixes)

| run | settle | close | committees | safety | discards | rate / median (busy s) |
|---|---|---|---|---|---|---|
| honest, 6 PRs | consistent, all epochs | consistent | consistent | SAFE, S3 = 0 | 0 | 1872 cps / 110 ms |
| `--offline 1` | consistent | consistent | consistent | SAFE, S3 = 0 | 0 | 2001 cps / 111 ms |
| `--byzantine 1` | pending stable on every PR after 132 s | consistent, 18 epochs | consistent | SAFE, S3 = 1233 | 155, none of a finalized block | 3305 cps / 518 ms, seconds of 6-27 s |

The Byzantine run degrades hard (its votes open a block's instance in every epoch and split the
honest nodes at every boundary) but neither the ledger nor the committees diverge.

### Where the Byzantine run's time goes (2026-09-20, evening)

`drain_check.py` over a fresh `--byzantine 1` run on the build above: the epochs' drains and
closes against the per-second confirmations. **41 of the 50 confirming seconds were inside a
drain** (an ended epoch some PR had not left yet): 692 cps at a 7.7 s median inside, 1356 cps
at 112 ms outside. Epoch 0, before any close is pending, ran at honest speed (2091 cps /
106 ms); the vote traffic was ~10 % above honest. The cost is not the Byzantine votes, it is
what they do to the epoch *close*, behind which the epoch pipeline is serialized: no election
starts while an ended epoch drains (`insert` → `Draining`), a node leaves an ended epoch only
once the epoch before is closed (`try_advance_epoch`), and a close only ticks once the one
before it closed (`tick_closes`). The drain-wait lines said it directly:
`unterminated=0 previous_closed=false`.

| epoch | drain | close rounds | close took | instances at end (min..max over PRs) |
|---|---|---|---|---|
| 0 | 6.2 s | 0, 1 | 14.3 s | 1043..1044 |
| 1 | 6.8 s | 0, 1, 2, 3 | 23.2 s | 1462..2437 |
| 2 | 16.5 s | 0, 1 | 26.0 s | 2570..6456 |
| 3 | 11.3 s | 0 | 12.7 s | 3485..9216 |

Three things made the closes slow, all found in the close event lines:

1. **The close froze whenever the epoch reopened.** `EpochClose::set_state` set `ready =
   state.is_terminated()` on every tick, and `tick` returns while not ready: a Byzantine vote
   of the closed epoch opens a stale instance (~1300 per node per run), the epoch has an
   unterminated instance again, and until its timeout certificate the close neither proposed
   nor advanced its round timeout. Honest-led rounds passed without a proposal (epoch 1's
   round 0: all five PRs ready with the same value, no close for 7 s). **Fix:** once ready, a
   replica stays ready - such an instance ends by a timeout and adds nothing to the value.
2. **The Byzantine representative leads rounds.** The leaders are the epoch's voters, in key
   order; a round it leads costs the full `close_round_timeout` of 5 s (7-9 s observed, with
   the freeze). Kudzu is safe under any timeout, only liveness depends on it: **the default is
   now 2 s**, above the leader's 1 s `PROPOSAL_DELAY`.
3. **The Byzantine representative extended the ended epoch.** During the drain it votes
   `First, epoch e` for fresh blocks; `insert_for_vote` started those in the ending epoch
   (`started_for_vote`, not stale) and the draining replicas *proposed* in them - their ledger
   block, dependencies finalized - so with three draining replicas plus the Byzantine share
   each one got a notarization certificate. The drain grew one instance at a time, and the
   leader's value changed every 0.3 s (DFECF273 → 3C1C8E5D → … over 5 s), so its 1 s
   stability wait never elapsed. **Fix:** no new election starts in an ended epoch - an
   instance opened for a vote while the epoch drains abstains like one of an epoch already
   left; the block is decided in the next epoch.

Two amplifiers, not causes: the **priority scheduler spun** (`election_scheduler.loop`
1.5M-4.4M per node against 34k-38k honest) whenever `check_vacancy` said yes and `refill`
inserted nothing - the container at its cap of 5000 (two PRs sat at 6398 active) or cooling
down (`active_elections.cooldown` fired 1-9 times per run); **fix:** `check_vacancy` mirrors
`refill`'s guards, the scheduler sleeps until `AecFact::Recovered` or the next block. And
`bootstrap_stale` at ~11k per node was the settle phase, not the run: elections older than
the 60 s threshold, after publishing ended - left alone.

With all fixes the Byzantine run's per-epoch medians are 112, 116, 120 ms (honest: 104, 107,
109 ms); `election_scheduler.loop` is back at 29k-36k per node in both runs, and
`started_for_vote` fell from 8k-12k to 2k-4k per node (the ended epoch no longer takes fresh
blocks). Every check passed on both: settled (with the pending instances the Byzantine
representative keeps open, stable on every PR), closed, committees consistent, SAFE, no
discards. Epoch 1's close still took 11 s over four rounds - a leader's proposal is only
first-voted by the replicas whose value it is, and the instances the Byzantine representative
opens still land differently on each; the two-value rule catches most of it, not all.

Not done: decoupling the next epoch's start from the previous close. Epoch e+2 counts in
C(e), derived from epoch e's agreed state, so its instances could only pool votes until the
close anyway; the lag of two epochs is the slack, and the fixes above are what keep a close
inside it. Nor a budget for vote-started stale instances: at ~26 per second against 2000
elections per second they cost ~1 %, once they no longer freeze the close.

| run | drain windows | closes (rounds) | busy rate / median | seconds ≥ 1 s | settle phase |
|---|---|---|---|---|---|
| byzantine, before | 41 of 50 s | 14.3, 23.2, 26.0, 12.7 s (2, 4, 2, 1) | 3329 cps / 136 ms | 5 of 12 | 132 s, 18 epochs |
| byzantine, fixes 1+2 + scheduler cap | 18 of 62 s | 9.8, 12.6, 8.7, 4.8 s (2, 3, 3, 2) | 3161 cps / 118 ms | 4 of 13 | 10 s, 6 epochs |
| byzantine, all fixes | **9 of 31 s** | **5.5, 11.4, 6.3, 1.3 s (2, 4, 2, 2)** | **2452 cps / 115 ms** | **2 of 17 (max 1.6 s)** | 10 s, 4 epochs |
| honest, before (run 4) | 3 of 26 s | 2.0, 3.0, 2.3 s (2, 2, 1) | 1883 cps / 105 ms | 1 of 22 | 0 s |
| honest, fixes 1+2 + scheduler cap | 3 of 23 s | 1.7, 1.7, 0.7 s (2, 2, 1) | 1831 cps / 108 ms | 0 of 23 | 0 s |
| honest, all fixes | 3 of 42 s | 2.1, 2.3, 1.2, 0.2 s (2, 2, 1, 1) | 1928 cps / 108 ms | 0 of 21 (max 512 ms) | 1 s |

## Tooling

- `committee_check.py`: over `final_state`, per PR, the committees known (`derived_by`, digest,
  n, members' weights); `COMMITTEES_CONSISTENT` or `_INCONSISTENT` / `_MISSING` (a PR in epoch
  e+2 without C(e)).
- `safety_check.py`: the invariants over the union of what every PR reports, so a violation
  every PR shares still fails: S1 no two conflicting blocks finalized across all epochs, S4
  every finalized block cemented at its height on every PR (sampled), S5 no block discarded as
  late was finalized. Reported, not failed: S2 a slot finalized one way and single-notarized
  another in some epoch, S3 blocks finalized in more than one epoch. The other checks compare
  the PRs with each other.
- `drain_check.py`: where a run's time goes - per epoch the drain (ended → last PR left), the
  close's rounds and duration, the spread of instance counts over the PRs; the per-second
  confirmations inside the drain windows against outside; the busy seconds per epoch.
- `run_faulty.sh`: the verification driver for the faulty-representative options: every check
  on `--byzantine 1`, the honest run and `--offline 1`; on a failed check the open instances are
  dumped from every PR into `compare/<run>.open` before the nodes are torn down.
- `run_fork_settle.sh`: as before, plus the committee and safety checks; on a failed check the nodes are
  kept running for a post-mortem over RPC (clean up by hand: `pkill -f "rsnano --network test";
  rm -rf ~/NanoSpam`).
