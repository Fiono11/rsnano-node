# Gate D — 7 October 2026

**Gate D passed on `fac6df5da`: all nine paper variants settled, one run each.** Six equal-weight representatives (f = p = 1), 45,000 blocks at 2,000/s, 45,000 accounts, 8 s epochs, quiet host. Settled means identical cemented state and identical decided checkpoints on every running node.

| Variant | Uncemented, same on all | Close rounds | Last close, median / max | p50 | Rate |
| --- | --- | --- | --- | --- | --- |
| nofork | 0 | 0, 0, 0 | 1.31 / 1.41 s | 91–92 ms | 1,964 blocks/s |
| fork5 | 1 | 0, 0, 0 | 1.63 / 1.66 s | 88–90 ms | — |
| fork10 | 5 | 0, 0, 0, 0 | 1.58 / 2.61 s | 89–94 ms | — |
| nofork-offline1 | 0 | 0, 0, 1 | 1.55 / 3.80 s | 97–99 ms | 1,944 blocks/s |
| fork5-offline1 | 0 | 0, 0, 1 | 1.51 / 3.56 s | 102–105 ms | — |
| fork10-offline1 | 9 | 0, 0, 1 | 2.03 / 3.80 s | 104–106 ms | — |
| nofork-byz1 | 0 | 0, 0, 1 | 1.52 / 3.46 s | 98–99 ms | 1,986 blocks/s |
| fork5-byz1 | 1 | 0, 0, 1, 0 | 1.24 / 3.81 s | 98–100 ms | — |
| fork10-byz1 | 3 | 0, 0, 1, 0 | 1.87 / 4.08 s | 101–104 ms | — |

No reconciliation stream was dropped; 1.66 to 2.28 symbols per recovered item. Uncemented positions are retained forks, uncemented identically everywhere. The first nofork-byz1 attempt was refused by the host check (Spotlight at 157 %) and rerun.

- [matrix/](matrix/) — the passing runs on `fac6df5da`.
- [baseline/](baseline/) — `7493aa08e`: fork5-byz1, fork10-offline1 and fork10-byz1 diverged in cemented state while every checkpoint agreed; a node kept a losing fork and never received the finalized winner.
- [retained/](retained/) — `5ae9c6e58` (retained instances kept): the same three still diverged.
- [follower/](follower/) — `fac6df5da` (checkpoint follower): the three settled.

One run per variant is not a noise band. Reproduce one variant:

```sh
python3 tools/rai/run_gate_b.py <dir> --wait-quiet 600 --fork-percentage 10 --byzantine 1 --timeout 150
```

## Non-fork goodput (`3fc759b57`)

The [rates](rates/) runs repeat the matrix with the client's non-fork measurement from rai-cross-epoch-minimal: blocks published without a fork are measured apart, a fork position counts once whichever block confirms, and the client ends once every non-fork block is confirmed. Goodput is non-fork confirmations over the client's run; latency is publication to the websocket confirmation the client sees. All nine settled again.

| Variant | Non-fork goodput | Client p50 / p95 / p99 | Node non-fork p50 |
| --- | --- | --- | --- |
| nofork | 1,968 blocks/s | 100 / 952 / 1,519 ms | 86–89 ms |
| fork5 | 1,803 blocks/s | 111 / 1,599 / 2,389 ms | 88–91 ms |
| fork10 | 1,718 blocks/s | 112 / 2,390 / 2,864 ms | 89–94 ms |
| nofork-offline1 | 1,987 blocks/s | 111 / 655 / 1,114 ms | 103–104 ms |
| fork5-offline1 | 1,861 blocks/s | 113 / 926 / 1,384 ms | 103–104 ms |
| fork10-offline1 | 1,738 blocks/s | 117 / 1,086 / 1,551 ms | 105–108 ms |
| nofork-byz1 | 1,986 blocks/s | 114 / 753 / 1,222 ms | 104–105 ms |
| fork5-byz1 | 1,854 blocks/s | 112 / 1,132 / 1,618 ms | 98–101 ms |
| fork10-byz1 | 1,446 blocks/s | 119 / 1,651 / 2,055 ms | 104–107 ms |

The fork5 run overlapped a `cargo check` on the same host and is not performance-eligible. The long p95 and p99 tails are the blocks published while an epoch closes: the predecessor gate holds their finality until the close decides.
