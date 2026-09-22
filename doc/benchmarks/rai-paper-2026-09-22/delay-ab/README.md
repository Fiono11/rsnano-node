# fork5: the weight fix and the close proposal gate (2026-09-22)

Three builds, two runs each, interleaved a-b-c-a-b-c so machine drift falls on
every arm. `fork5` only: 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks, 8 s epochs.

| arm | build |
|-----|-------|
| a | `444900536`, before the committee weight fix |
| b | `26263c83c`, the weight fix |
| c | b with the leader proposal gate removed from `EpochClose::valid_proposal` |

| run | rate | median | weighted | epochs closed in round 1 (of 3) |
|-----|------|--------|----------|----------------------------------|
| a-1 | 2068 | 108 ms | 262 ms | 1 |
| a-2 | 2068 | 108 ms | 194 ms | 0 |
| b-1 | 1878 | 109 ms | 177 ms | 1 |
| b-2 | 1884 | 110 ms | 191 ms | 1 |
| c-1 | 1976 | 104 ms | 207 ms | 2 |
| c-2 | 2062 | 122 ms | 306 ms | 1 |

All six runs SETTLED_CONSISTENT, CLOSED_CONSISTENT and SAFE.

**The weight fix costs throughput because it raises the quorum.** Arm a's
derived committees total 3.10e38 with a 19.8 % top share; arm b's total
3.23e38 with 18.0 %. Arm a understates the total, so every share reads high
and `q` is met with less real weight. Its 2068 cps is not a target for b.

**Removing the proposal gate does not pay.** It was kept because a leader
whose instances have not settled proposes a value its followers have moved
off, so they abstain and the round times out anyway. Removing it gave *more*
round-1 closes (3 vs 2), ~7 % more mean throughput, and ~40 % worse weighted
latency with the series' worst spike (1299 ms on an epoch boundary). The gate
stays until the epoch-value step retires it, at which point the leader
proposes `d_e` derived from `N-f` selected reports and never waits on its own
instances.

Rerun: `./run_ab.sh`. Metrics come from `../tools/summarize.py <log>`; the
script's own `rate=` grep matches nothing, since that line is produced by
`run_matrix.sh`, not `run_variant.sh`.

**Binaries must be built with the protocol feature**, or the node silently
runs legacy consensus and stalls in epoch 0 at ~13-14k blocks with an empty
diagnostics log:
`cargo build --release -p rsnano_cli -p nanospam --features rsnano_cli/rai_protocol`.
Check with `strings <binary> | grep -c EPOCH_START` before trusting a run.
