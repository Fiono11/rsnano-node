# Revised cross-epoch lock: incremental implementation

Baseline: `5e037cfe0527d7b06b573456c7c876568ce179ea` on `rai_kudzu`.
Implementation branch: `rai-cross-epoch-minimal`, based exclusively on that
commit. The baseline excludes the original checkout's uncommitted work.
Selected recovery helpers were subsequently reviewed and adapted on the
implementation branch; the original dirty checkout remains preserved.

Authoritative target: updated `RAI.pdf` (2026-09-24). See [PLAN.md](PLAN.md)
for the revised R/N/F report stage, file-level changes, tests and measurements.

## Sequence and gates

0. Pin the baseline, instrument one shared client, preserve every attempt.
1. Remove certificate-free checkpoint finalization; distinguish retained
   ancestry, represented notarization and recovery-only locks in the state
   commitment and checkpoint transfer. Measure before advancing.
1b. Include inherited recovery-protected blocks in frozen R/N/F `T_i`, validate
   R against the predecessor, and derive exact `G_i = V_i \ keys(T_i)`.
2. Equal-weight integer thresholds and old-committee-only checkpoint agreement.
3. Durable local signing records and the preceding-epoch first-vote lock.
4. Full selected-path attachment, both overlap exceptions, provisional recheck.
5. Signed report evidence, independent successor reconstruction and exactly-once
   installation, including preservation of live finality.
6. Membership replacement, faults, reconstruction scaling and long runs.

An intermediate step is not a claim that the entire revised protocol is done.
Do not advance past a correctness failure or an unexplained performance gate.

## Reproducible comparison

Build the node with `cargo build --locked --release -p rsnano_cli --features
rai_protocol --bin rsnano` from each pinned revision. Keep copies of both
binaries. Build **one** instrumented `nanospam` client with matching RAI message
features and use it for both nodes. Save compiler version, source revisions,
Cargo.lock SHA256, build command and binary SHA256 alongside the results.

Example (absolute paths required for binaries):

```sh
python3 run.py --baseline /path/to/baseline/rsnano \
  --candidate /path/to/candidate/rsnano --client /path/to/shared/nanospam \
  --out results/step1-handoff --repetitions 5
```

Use `--epoch-ms 0` for the account-path comparison, then repeat with handoffs.
Default load is 45,000 blocks, 45,000 accounts, 2,000 blocks/s, six
representatives, no forks, 8-second epochs. This measures a fixed offered load,
not maximum sustainable capacity. Alternate baseline/candidate order. Do not
run other benchmark or build processes concurrently. The runner needs local
network/process permissions and exclusive use of the node ports.

The runner pins both binaries by SHA256, records all attempts including
timeouts, preserves logs and node data, and collects final RPC snapshots. It
requires all requested blocks to be confirmed. Its paired bootstrap screening
gate is a 95% interval with goodput ratio >= 0.95 and both p50/p95 ratios <= 1.10; ambiguous
results are inconclusive, not passes. Fewer than five pairs are smoke tests.
The gate measures performance/completion, not protocol safety. Check final
states and protocol invariants separately.

The shared client adds a millisecond confirmation histogram at the point where
it already tracks confirmations. These are observed **block** confirmation
latencies; they are not completed send/receive transfer latencies or evidence
that an observed confirmation has a valid certificate. The existing generator
is unseeded; paired runs match parameters and client, not exact transaction
traces. Exact seeded workloads, time-windowed resource/latency sampling,
transfer measurements and membership changes remain evaluation work.

The frozen HEAD is an engineering baseline, not a correctness-equivalent
protocol: it can finalize weaker checkpoint evidence. In forked workloads,
compare certificate-backed goodput and pending work rather than treating all
old confirmations as equivalent to revised finality. Preserve stalled fork
runs; do not retry them until one succeeds.

For baseline-derived timeouts, pass `--baseline-reference /path/to/prior-run`.
The default deadline is 1.5× the slowest matching completed baseline wall time,
rounded up. Current-batch baseline observations update it; candidate durations
never do. `--timeout` is only the first calibration ceiling if no reference
exists. Failed baseline calibration stops the batch. Every result records its
timeout and derivation. See [EVALUATION.md](EVALUATION.md) for current results.

## User-directed primary latency focus (2026-09-24)

For subsequent comparisons, the primary latency metrics are **p50 and p95**,
each with the existing 10% non-inferiority allowance, plus the 5% goodput
allowance. p99 remains recorded as a diagnostic and no longer determines the
gate. Harness manifests label this `p50-p95-v1`. Earlier p99-based results retain
their original gate labels and verdicts; do not silently relabel them as passes.
Baseline-derived process deadlines remain unchanged.

Full recorded results: [PERFORMANCE.md](PERFORMANCE.md), including the latest
optimized batch stopped on a baseline settlement failure. The 2026-09-24
no-crash pass and its runs: [NO-CRASH-IMPLEMENTATION.md](NO-CRASH-IMPLEMENTATION.md),
[NO-CRASH-RESULTS.md](NO-CRASH-RESULTS.md).

## Run data cleanup

At the user's request, subsequent runs delete generated `data/` after process
cleanup and saving results/logs/RPC snapshots, including timed-out attempts.
TOML configuration is copied to `saved-config/`; `data-cleanup.json` inventories
the deleted files and records free disk space. Data is kept if process cleanup
fails or required evidence is missing. `--keep-data` opts out when later database
inspection is required. Earlier failed-run databases remain preserved.

The reconciliation-refresh batch used a companion watcher without changing
its pinned measurement harness. Its first seven successful data directories
were reclaimed during attempt eight's setup; subsequent directories were
removed on completion. The fork diagnostic's data was removed after saving
an additional failure summary. Results are retained, not retried or discarded.
