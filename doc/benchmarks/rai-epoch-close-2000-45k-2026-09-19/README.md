# RAI epochs, step 3: the epoch close election (2026-09-19)

Same build type, command and machine as `../rai-epochs-step2-no-repropose-2000-45k-2026-09-19`
(`--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks,
`--epoch-terminated-elections 22500`, `run_fork_settle.sh`), with the close election of
every epoch added and the epochs made sequential:

- An epoch's duration ends once 22,500 of its elections are decided (or once more than f
  of the weight is seen ahead). No new election starts in it any more; its instances run
  to their certificate. Once every instance of the epoch is terminated and the epoch before
  is closed, the epoch is left: its close election starts and the next epoch starts with it
  (`EPOCH_ENDED` → `EPOCH_ADVANCED`).
- The close election of epoch e is a multi-round Kudzu instance whose value is the hash of
  the epoch's final state (the blocks finalized by a certificate of e plus its settled single
  notarization certificates). A replica takes part once every instance of e has settled here
  (`EPOCH_CLOSE_READY`): the leader of round r — the (e + r)-th of the representatives that
  voted in e, in public key order — proposes its value with its first vote, a replica first
  votes it if it is its own value and abstains after Δ_timeout (5 s) otherwise, second looks,
  timeout votes and the exit final vote follow Protocol 1. A round without a finalization
  ends by a notarization or timeout certificate and the next leader proposes; a proposal is
  valid only if it chains to the earlier rounds (a timeout certificate, or a notarization
  certificate for the same value). The epoch is closed by the first round with a fast
  finalization or finalization certificate (`EPOCH_CLOSED`).
- Close votes travel as ordinary votes: the epoch field carries the close round
  (`ConsensusEpoch::close_round`), the hash is the value itself; certificates are
  solicited like those of any instance.
- `settle_check.py` now ends the run only once every epoch is closed: after the per-epoch
  states are settled and identical, every PR is told to leave its current epoch
  (`epoch_advance`), and the check waits until every epoch reports the same finalized close
  value on every PR, equal to the value each PR attests itself (`CLOSED_CONSISTENT`).

Two earlier attempts of the day are in `attempts/`: the leaders were first taken from the
ledger's representative weights, which include nanospam's spam accounts' representatives
that never vote (14 rounds without a proposal); then the epoch was left only once its
instances had *settled*, which stalled the pipeline for 4 s at every switch because the
last forks settle only after their missing first votes are solicited (5 s passive period).
Leaving at termination and closing at settlement removed the stall.

## Results

Per epoch, identical on all six PRs, closed in round 0 on all six:

| Epoch | State hash | Close value | Finalized | Single | Conflicting | Ended → left | Left → ready (PRs) | Closed after left |
|---|---|---|---|---|---|---|---|---|
| 0 | `BFC754AD…` | `D3651CD4…` | 21,535 | 347 | 738 | 0.24–0.40 s | 2.4–4.4 s | 4.5 s |
| 1 | `E1FBBE6B…` | `8301C0F3…` | 21,310 | 359 | 754 | 0 s (after `epoch_advance`) | 0 s | 0.1–0.2 s |

Epoch 0's close waited for the settling of the last forks (the value is final only then);
epoch 1 had settled long before it was told to end. Five of the six PRs entered round 1 of
epoch 0's close for a few milliseconds: their round 0 had terminated (notarization
certificate, value in the tree) before the finalization certificate reached them.

nanospam status lines with ≥ 1000 cps:

| Metric | step 2 (no re-propose) | **step 3 (close election)** |
|---|---|---|
| Confirmation rate | 1892 cps | **1854 cps** |
| Median of the per-second averages | 94 ms | **96 ms** |
| Average confirmation time, cps-weighted | 97 ms | **104 ms** |
| Worst second | 137 ms (startup) | **171 ms** (startup) |
| Switch second (`EPOCH_ENDED` 10:35:33.54 UTC) | 1832 cps / 86 ms | **2077 cps / 146 ms** |
| Cemented on every PR | 42,903 | 42,855 |
| Settle phase (settled + closed) | 1 s | 6 s |

The switch costs one second of ~150 ms average confirmation time: the blocks published
while the ended epoch drained (250–400 ms) started with the next epoch. Throughput and
latency are otherwise within run-to-run noise of step 2.

Node side per PR: 2 epochs ended and left, 2 closed, 2–4 close rounds entered, 0 stale
instances started (every PR drains before it leaves, so no vote for an old epoch's
instance arrives after the switch), 382–706 instances started for a vote.
