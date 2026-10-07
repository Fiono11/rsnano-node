# Gate C — 7 October 2026

**Gate C passed on the final binary, with one caveat.** fork0, byz1 and three fork5 runs settled, with zero dropped reconciliation streams. An earlier fork5 run on the previous binary diverged in its cemented state, so the fork5 result is four settled runs out of five, not a guarantee. Symbols per recovered item were 1.7 to 2.3, above the plan's target of about 1.35.

All runs: six equal-weight representatives (f = p = 1), 45,000 blocks at 2,000/s, 45,000 accounts, 8 s epochs, quiet host (the runner waited for one). Node binary `7493aa08e`, SHA-256 `929e77b2a379cf2c…` (full value in each `result.json`). The fork5 runs record `7493aa08e` with the runner's settlement fix applied on top; the node binary is the same.

| Variant | Settled | Uncemented, same on every node | Closes (round) | Last close after leaving | p50 (ms) | Rate | Symbols per item | Dropped |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| [fork0](fork0/) | yes | 0 | 0, 0, 0 | 1.76, 1.36, 1.09 s | 88–89 | 1,953 blocks/s | 1.71 | 0 |
| [fork5](fork5/) | yes | 3 | 0, 0, 0 | 1.81, 1.76, 0.98 s | 89–91 | — | 2.12 | 0 |
| [fork5-repeat](fork5-repeat/) | yes | 2 | 0, 0, 0 | 1.47 s max | 87–90 | — | 2.12 | 0 |
| [fork5-repeat2](fork5-repeat2/) | yes | 2 | 0, 0, 0 | 1.98 s max | 88–90 | — | 2.29 | 0 |
| [byz1](byz1/) (5 nodes) | yes | 0 | 0, 0, 1 | 1.40, 1.45, 3.40 s | 99–101 | 1,647 blocks/s | 1.81 | 0 |

A fork run leaves a few forked positions uncemented on every node alike. A split first vote leaves such a position retained by the checkpoint, and under single support only a child block resolves it; the client never sends one. The plan's settlement rule asks for the same cemented state everywhere, not for every position to finalize. The client keeps waiting for these blocks, so it prints no confirmation rate for fork runs.

In byz1 the Byzantine representative led epoch 2's round 0, which timed out after Δ_E; round 1 closed it 3.4 s after the epoch was left.

## The divergent fork5 run

[earlier/fork5-diverged](earlier/fork5-diverged/) ran on `3e1fa4422`, which differs from the final binary only in the symbol batch sizes. All six nodes decided the same three checkpoints, but their cemented counts were 45,065, 45,064 and 45,059, and two nodes counted 13,121 finalized blocks in epoch 2 where four counted 13,120. One node finalized a block locally that the decided checkpoint did not finalize, and nothing brings the others to it. This is the residual rai_kudzu showed in September (blocks finalized on one node, single-notarized on the rest). The plan's Phase 3 ports address it: the cumulative frozen T with the R tag, BuildState per Figure 3, and installation that fetches, forces and cements checkpoint-finalized blocks.

## Reproduce

```sh
cargo build --release -p rsnano_cli -p nanospam --features rsnano_cli/rai_protocol
python3 tools/rai/run_gate_b.py <dir> --wait-quiet 600 --fork-percentage 0 --timeout 300
python3 tools/rai/run_gate_b.py <dir> --wait-quiet 600 --fork-percentage 5 --timeout 150
python3 tools/rai/run_gate_b.py <dir> --wait-quiet 600 --byzantine 1 --timeout 300
```
