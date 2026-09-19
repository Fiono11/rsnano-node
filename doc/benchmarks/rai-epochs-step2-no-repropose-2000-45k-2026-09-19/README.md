# RAI epochs, step 2 without re-proposing undecided blocks (2026-09-19)

Same build, command and machine as `../rai-epochs-step2-two-epochs-2000-45k-2026-09-18`
(`--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks,
`--epoch-terminated-elections 22500`, `run_fork_settle.sh` until `SETTLED_CONSISTENT`),
with one rule changed, now the behaviour of the branch: a block whose instance of an
earlier epoch exists — running or ended undecided — is never proposed again in the current
epoch; only a vote of another replica opens an instance of a later epoch. The earlier run
let the backlog scan propose a block again once its instance ended undecided (timeout
certificate, two notarizations, single notarization without enough final votes), which
usually finalized it in the new epoch because every PR held the notarized candidate by then.
The run was made with a temporary `--repropose` nanospam flag set to `false`; the flag was
removed again and the rule made unconditional.

Only one switch happened: epoch 1 never reached 22,500 decided elections (21,353 finalized +
576 single + 497 conflicting = 22,426), so the run ends in epoch 1.

## Results

Per epoch, identical on all six PRs (`settle_check.py`, first poll after publishing):

| Epoch | State hash | Finalized | Single notarizations | Pending | Cemented undecided | Empty | Conflicting |
|---|---|---|---|---|---|---|---|
| 0 | `39F51E9E…` | 21,543 | 552 | 0 | 0 | 0 | 525 |
| 1 | `23A86CE7…` | 21,353 | 576 | 0 | 0 | 0 | 497 |

nanospam status lines with ≥ 1000 cps:

| Metric | step 1 (single epoch) | re-propose on | **re-propose off** |
|---|---|---|---|
| Confirmation rate | 1851 cps | 1870 cps | **1892 cps** |
| Median of the per-second averages | 108 ms | 101 ms | **94 ms** |
| Average confirmation time, cps-weighted | 111 ms | 257 ms | **97 ms** |
| Worst second | 157 ms | 1839 ms | **137 ms** (startup) |
| Switch second (`EPOCH_ADVANCED` 09:26:45.82 UTC) | – | 2544 cps / 1839 ms | **1832 cps / 86 ms** |
| Blocks confirmed during publishing | 42,579 | 43,002 | 41,628 |
| Cemented on every PR | 42,855 | 45,019 | 42,903 |
| Settle phase | 3 s | 5 s | 1 s |

With re-proposing off the switch is an ordinary second and the run is the best of the three
on every throughput and latency number, while ~2,100 blocks (4.7 %: the undecided forks and
their successors) never cement, as in the single-epoch run. Everything the switch costs with
re-proposing on is the price of deciding those blocks.

Node side per PR: 12.6–16.9k votes through the vote processor (on: 16.7–24.1k), 250–680
instances started for a vote (on: 1.6–2.9k), 6–10k vote-cache insertions, 5.6–6.1k
`confirm_ack` in.
