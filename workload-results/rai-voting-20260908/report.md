# RAI Kudzu latency below baseline

Same workload for both final validations and baseline: `nanospam --prs 6 --no-prio --accounts 50000 --blocks 50000 --rate 2000 --epoch-length 25000`. Fresh data, RAI release builds, RUST_LOG=info, local TCP/WebSockets. The two final runs used byte-identical binaries, with no concurrent profiling or compilation. Nanospam source, measurement logic, load settings and scheduling intervals were not changed.

| Metric | Clean HEAD baseline | Final implementation | Repeat, same binaries |
|---|---:|---:|---:|
| Average confirmation time | 517.73 ms | 153.71 ms | 243.73 ms |
| Confirmation rate | 1794.60/s | 1888.96/s | 1870.55/s |
| Injection plus drain | 27.86 s | 26.47 s | 26.73 s |
| Epoch agreement after workload | 3.22 s | 3.22 s | 3.13 s |
| Workload confirmations | 50,000 | 50,000 | 50,000 |

Latency decreased 70.3% in the first final run and 52.9% in the repeat. All six PRs in each final run reached epoch 2, cemented 50,053 total blocks and had zero unchecked blocks. Epoch counts and digests agree exactly within each run. Canonical epoch counts are 25,201 / 24,852 in the first run and 25,175 / 24,878 in the repeat; epoch length is the existing advancement trigger, not a cap on final canonical membership.

PR0 fast/slow counters from the first final run: 49,804 fast and 353 slow (99.30% fast). These are election confirmations including setup and earlier-epoch recovery, not distinct workload-block counts. fixed-stats.json preserves the per-node counters.

## Findings and fixes

The original phase integration minted fresh timestamped First statements on retransmission and duplicated First emission in active final-vote jobs. Immutable First/Notarize statements now have stable zero timestamps (duration bits retained), allowing the existing duplicate filters and vote cache to recognize retransmissions. Active final jobs avoid re-emitting an already-issued First; request recovery and post-confirmation recovery remain available. This final change set produced the large, repeatable improvement above.

Load profiles also exposed serial waits and work under shared locks. The implementation now batches eligibility and active-hash lookups, skips election lookups for unconditional First signing, avoids allocating per-election temporary vote-target vectors, uses the existing fast hash-map implementation for internal RAI state, and caches known confirmation epochs in a bounded index to bypass redundant late-vote ledger reads. Earlier epochs still follow the original recovery checks.

First recovery uses its own request queue and eligible First statements are emitted before final-lock I/O. In-memory final reservations permit releasing the signing mutex before acquiring the ledger writer. Empty/replay-only final writes are avoided. Votes are signed once, and boolean certificate checks do not allocate certificates.

Phase-aware solicitation and preservation of final summaries handle Final-before-First delivery. Late RAI phases remain eligible for the existing rebroadcast mechanism. Fast-confirmed elections can recover First statements after removal without manufacturing a Final certificate.

## Scope and validation

No changes to p=f=19%, certificate thresholds, epoch length/advancement, legacy request intervals, workload parameters, or ledger schema/persistence. No new persisted votes/certificates. Feature-disabled legacy behavior is retained. The user assumption of continuously operating nodes remains.

Final RAI/legacy unit-suite output is in unit-rai-final.log and unit-legacy-final.log. The previous final-production build passed the two recovery integration tests, all three Kudzu network tests, and the different-local-epochs convergence test; logs are included alongside this report. The last source edit after benchmarking only adds retransmission assertions to an existing unit test and updates documentation. Production code is unchanged from the measured binaries.

Raw intermediate attempts and diagnostic profiles are retained locally outside version control. Two final successful runs demonstrate the requested local target; they are not a statistical guarantee across environments or a proof of general protocol liveness. The profiles support the identified bottlenecks, but individual optimization contributions were not isolated in a full ablation study.

All benchmark ledger data and saved benchmark binaries have been deleted as requested. This directory retains the baseline, original Kudzu, and two final measurements, exact command metadata, logs, and validation output. Command metadata records the historical binary hashes and paths; those binaries and data paths no longer exist. The implementation and regression tests are committed in the source tree.
