# Five-percent fork diagnostic

Run after the current zero-fork five-pair batch. Use frozen baseline
`5e037cfe0527d7b06b573456c7c876568ce179ea`, candidate `30c452080`, and the same
pinned recovery client on both sides. Set `--forks 5`; keep 45,000 blocks,
45,000 accounts, 2,000 blocks/s, six nodes and 8-second epochs unchanged.

This first run is one diagnostic pair, not a performance gate. Preserve all
attempts and stop on completion or settlement failure. A matching forked
baseline does not yet exist: use the existing baseline-derived 156-second
ceiling for its initial calibration, then derive the candidate deadline as
ceil(1.5 × successful matching forked baseline wall time). Do not use zero-fork
runs as matched references or let candidate time extend the allowance.

Record inclusive goodput, p50/p95/p99, recovery-child count, all-node settlement,
checkpoint rounds, and R/N/F counts. Delete generated run data after results/logs/RPC/configuration evidence is
saved, including a failure summary for an unsuccessful attempt. A failed
baseline remains a failed attempt, not a reason to restart until it passes.

The generator is unseeded. A 5% option means probabilistic fork injection,
not an identical predetermined conflict trace on both binaries. Baseline can
finalize weaker checkpoint evidence: report fork resolution and retained work
alongside timing instead of interpreting every old confirmation as equivalent
to revised finality. Five performance pairs are appropriate only after the
forked diagnostic completes and settles correctly. No committee/signing stage
advancement is implied by running this diagnostic.

## Superseding completion criterion

Following the user's clarification, the original confirmation-only diagnostic
is not a fork-termination evaluation. Use checkpoint inclusion or justified
discard per fork, with unresolved outcomes explicit. See [FORK-DEBUG.md](FORK-DEBUG.md).
Do not start a five-pair fork performance batch before this instrumentation and
the reconstruction bottleneck are addressed.
