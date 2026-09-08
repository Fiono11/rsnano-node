# RAI local epoch benchmark

Completed September 8, 2026. One measured run per mode on this machine, sequentially, with local TCP, RPC and WebSocket access. Both used 6 PRs, no priority traffic, 50,000 accounts, 50,000 workload blocks and a target publication rate of 2,000 blocks/s. Separate fresh ledgers used the same account keys. RAI used `--epoch-length 25000`.

| Metric | Baseline | RAI | Change |
| --- | ---: | ---: | ---: |
| Average confirmation time | 226.58 ms | 293.36 ms | +29.47% |
| Confirmation rate | 1831.94 blocks/s | 1799.42 blocks/s | -1.78% |
| Workload duration | 27.29 s | 27.79 s | +1.81% |

Both confirmed all 50,000 workload blocks. Every PR ended with 50,053 blocks cemented and zero unchecked blocks (genesis plus 52 setup blocks are included in ledger totals). The baseline binary was preserved before the final RAI-only canonical epoch changes.

Latency measures publication to WebSocket confirmation on PR0. Confirmation rate is confirmed workload blocks divided by injection-plus-drain time; setup and the subsequent passive epoch-set check are excluded. This is achieved throughput at the requested rate, not a maximum-capacity measurement. One run per mode does not establish statistical significance.

## Canonical epoch agreement

All six RAI PRs reported current epoch 2 and identical sets:

| Canonical epoch | Cemented blocks | Blake2 digest of sorted block hashes |
| --- | ---: | --- |
| 0 | 25374 | `5293E62AE7C3EF243074A36BE7B9E67AD4C38C175BEC4FE1E0B79316CAEAFA1E` |
| 1 | 24679 | `6AC4709C78E08153BADB58565230521A301EDB3CAC83CE3D97E77C6D8515726C` |

The passive check observed matching, stable sets across three subsequent one-second polls, taking 3.208 seconds after the workload. It sends no advertisements or recovery messages. Epoch 0 can contain more than 25,000 blocks because existing elections retain their epoch and earlier confirmations determine canonical assignment; the threshold controls local advancement, not a fixed epoch capacity.

An earlier-epoch vote can create an independent election for a known slot. A newer-epoch vote cannot create another election. Earlier elections must reach quorum before lowering ledger assignments. Lowering never increments the cemented counter. There is no epoch-transition consensus or epoch-advertisement mechanism.

## Reproduction

Build baseline with `cargo build --release -p rsnano_cli -p nanospam` and RAI with the additional `--features rai_protocol`. Preserve each pair of binaries separately. Set PATH to the corresponding binary directory and use a fresh data directory for each run:

```sh
RUST_LOG=warn,nanospam=info NANO_LOG=noansi nanospam \
  --data-dir "$RUN_DATA_DIR" --prs 6 --no-prio \
  --accounts 50000 --blocks 50000 --rate 2000
```

Add `--epoch-length 25000` for RAI. `RUST_LOG` is explicit because this environment otherwise suppresses benchmark metrics.

Raw output: `baseline.log`, `rai.log`. Machine-readable results: `baseline-summary.json`, `rai-summary.json`, `rai-ledgers.json`. Generated ledger data and executable copies were deleted after validation at the user’s request.

## Validation

RAI affected library suites: 971 tests passed. The TCP integration test with differing local node epochs passed and verified identical canonical epoch sets. Tests cover signed epochs, separate election tallies, earlier-only admission, canonical lowering including dependencies, no duplicate counting, and restart persistence. Feature-disabled affected library suites: 963 tests passed (`default-unit-tests.log`). RAI RPC message suite: 316 tests passed (`rai-rpc-tests.log`). `git diff --check` passed.
