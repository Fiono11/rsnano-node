# Gate B attempts — 7 October 2026

**Gate B is not passed.** The Kudzu close decides in round 0 on all six nodes whenever N−f = 5 reports are usable, but without report reconciliation the frozen reports of an epoch rarely share roots, and the close then never becomes ready. Both runs used `2464504bf`, six equal-weight representatives (f = p = 1), no forks, all nodes online, and `--allow-busy` (WindowServer at 26–30 % CPU), so neither is performance-eligible.

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
