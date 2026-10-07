# Gate B — 7 October 2026

**Gate B passed** with the [strict run](strict-rateless/) on `172460c73`, after the Phase 2 rateless reconciliation. The host precheck found no busy process. Six equal-weight representatives (f = p = 1), 45,000 blocks at 2,000/s, no forks, all nodes online, 8 s epochs.

| Measurement | Gate A (one epoch) | Gate B strict run |
| --- | --- | --- |
| Confirmation rate | 1,973 blocks/s | 1,931 blocks/s (−2.1 %) |
| Finalization p50, six nodes | 88, 88, 88, 89, 89, 88 ms | 89, 88, 89, 89, 89, 87 ms |
| Cemented on every node | 45,065 | 45,065 |
| Epochs decided on all six, same value | — | 0, 1, 2, all in round 0 |

Time from the first node leaving an epoch:

| Epoch | Reports usable everywhere | Proposal | Last close certificate |
| --- | --- | --- | --- |
| 0 | 611 ms | 706 ms | 1,664 ms |
| 1 | 624 ms | 307 ms | 1,754 ms |
| 2 | 569 ms | 360 ms | 1,093 ms |

The last close certificate came 1.1–1.75 s after the epoch was left, above the Kudzu record of 0.1 to 1.3 s. Reconstruction of the five other reports took 50–680 ms per node; the rest is proposal validation and the vote rounds. Throughput and p50 are within one run's noise of Gate A.

Node SHA-256: `4d7f96000c31fddaa2f3327c6712d558e245816661c4f13b2abd7e3266cf1378`. Client SHA-256: `14f5145c29e7de7eeaf09a91e45d875a6f429992e33fcbecf023301e087fa08f`.

## Earlier attempts, before reconciliation (`2464504bf`)

**Not passed.** The Kudzu close decides in round 0 on all six nodes whenever N−f = 5 reports are usable, but without report reconciliation the frozen reports of an epoch rarely share roots, and the close then never becomes ready. Both runs used `2464504bf`, six equal-weight representatives (f = p = 1), no forks, all nodes online, and `--allow-busy` (WindowServer at 26–30 % CPU), so neither is performance-eligible.

| Run | Workload | Epochs decided on all 6 | Close after the first node left the epoch | Stall |
| --- | --- | --- | --- | --- |
| [correctness](correctness/) | 45,000 blocks at 2,000/s, 8 s epochs | none | — | epoch 0: six reports, five different certified roots, 1–2 usable of 5 needed |
| [smoke-low-load](smoke-low-load/) | 360 blocks at 10/s, 5 s epochs | 0, 1, 2 (all round 0) | 541–579, 794, 574–593 ms | epoch 3: three different certified roots, 4 usable of 5 needed |

While a close is not ready, the predecessor gate holds the next epoch's account finality, so confirmation stops: the correctness run confirmed 15,555 of 45,000 blocks, the smoke 207 of 360.

The diagnosis is the one the plan anticipated for fork runs, now seen without forks: Phase 0 has only the identical-root shortcut for obtaining another node's report. In-flight blocks at a timed boundary differ per node, so certified sets differ even with no forks and at low load. Gate B therefore needs the Phase 2 rateless reconciliation (or rai_kudzu's paginated differences) before it can pass.

Reproduce:

```sh
cargo build --release -p rsnano_cli -p nanospam --features rsnano_cli/rai_protocol
python3 tools/rai/run_gate_b.py doc/benchmarks/rai-gate-b-2026-10-07/<new-dir>
python3 tools/rai/run_gate_b.py doc/benchmarks/rai-gate-b-2026-10-07/<new-dir> --blocks 360 --accounts 360 --rate 10 --epoch-ms 5000
```

Node SHA-256: `30fce3499954c8c90e43c52e69bdd695022cc0ba5a06d68c82af7ff828a74fe7`. Client SHA-256: `5fa01c3d8bf49322acd2e0ec0e096fe51ae369ef053f84d177862183edd2c638`.
