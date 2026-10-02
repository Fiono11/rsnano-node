# Phase 0 / Gate A — 2 October 2026

Implementation and correctness checks passed. **Gate A's performance comparison remains pending a quiet host.** Four strict attempts were rejected before launching nodes because the CPU precheck found processes above 25%. The first caught the release build; later attempts caught browser, macOS indexing, WindowServer or editor activity. No unrelated processes were stopped.

## Workload and result

The [correctness run](correctness/result.json) used `--allow-busy`, six representative nodes, 45,000 primary blocks, 45,000 accounts, 2,000 blocks/s, no forks and a 360-second epoch. It stayed in epoch zero. All six nodes cemented 45,065 blocks (including setup blocks), had zero unchecked blocks, and reported the same final-state hash:

`6FEE1C6EC2CD9D50D441F4E632B08962B7517C275980A61BB8582017107A1566`

| Measurement | rai_kudzu record | Phase 0 correctness run |
| --- | --- | --- |
| Confirmation rate | 1,941 blocks/s | 1,948 blocks/s |
| Finalization p50, six nodes | 89, 89, 89, 88, 90, 90 ms | 83, 83, 83, 83, 83, 83 ms |
| Performance eligible | Historical baseline | No: busy host override |

These observed numbers do not certify the performance gate. The [baseline](baseline.json) identifies the source record; its client interval median is deliberately not substituted for per-block finalization p50. The historical run used short epochs, while Gate A requires a single long epoch.

Node SHA-256: `36bbc9f4ebcfc439c845b5e8b49420494c70a993028d50db43ebcd6b9b2644df`.

Client SHA-256: `762ddd645f9ad3e8778a40e35458d79eacca20a8124ca959b6ee7991116fb517`.

The binaries were built from the phase-0 working tree over `ff2b3271053633e2cc6239589c6640eef39d87c1`, importing from `8aaf98c2c`. Subsequent source changes only corrected comments and feature-off cooldown behavior and added feature-off regression tests; the measured feature-on behavior is unchanged. The result includes exact flags, all six final states and counters. The run directory also retains the host samples, client log and node configs. Temporary ledgers and this run's processes were cleaned up by the runner.

The [per-commit validation record](validation.json) records all five isolated checks.

## Verification

- Formatting and `git diff --check`: passed.
- `cargo check --tests`, with and without `rsnano_node/rai_protocol`: passed.
- Workspace feature-off library suite: passed.
- Feature-on library suites: node 683, messages 82, types 131 tests passed.
- Feature-on election integration suite: seven test cases passed.
- Feature-on vote-processor integration suite: five test cases passed.

Seven integration test cases and six benchmark nodes are independent counts. The election suite contains four quorum cases and three account-election behavior cases.

To finish the performance gate when the host is quiet:

```sh
tools/rai/run_matrix.sh doc/benchmarks/rai-phase0-2026-10-02/quiet-run
```

Compare that run's recorded confirmation rate and six finalization p50 values with the baseline before declaring Gate A passed.
