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

## Tooling

- `committee_check.py`: over `final_state`, per PR, the committees known (`derived_by`, digest,
  n, members' weights); `COMMITTEES_CONSISTENT` or `_INCONSISTENT` / `_MISSING` (a PR in epoch
  e+2 without C(e)).
- `safety_check.py`: the invariants over the union of what every PR reports, so a violation
  every PR shares still fails: S1 no two conflicting blocks finalized across all epochs, S2 no
  finalized block beside a different settled-single one at its slot, S3 blocks finalized in
  more than one epoch (reported, not failed: the code tolerates it, the workload never produced
  one), S4 every finalized block cemented at its height on every PR (sampled), S5 no block
  discarded as late was finalized. The other checks compare the PRs with each other.
- `run_fork_settle.sh`: as before, plus the committee and safety checks; on a failed check the nodes are
  kept running for a post-mortem over RPC (clean up by hand: `pkill -f "rsnano --network test";
  rm -rf ~/NanoSpam`).
