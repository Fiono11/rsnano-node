# Close-proof phase — 2026-09-23

Base: `21a1d9e07`, branch `rai_kudzu`.

Added 0x19/0x1a proof request/reply, original signed placement/vote retention,
and predecessor-ordered verification against locally derived committee pools.
The verifier rejects unsigned state changes, repeated signers, wrong domains,
one-sided certificates and mixed fast/normal pairs. Transfer and installation
follow in the next two commits; this phase does not yet orchestrate catch-up.

Validation: required feature suite passed (96 message / 762 node tests); the
full default library suite passed with localhost sockets available. Nanospam:
23 passed, one ignored, after correcting its stale initial-account delegation
test. Release build passed; `EPOCH_START` string count: 1.

| Variant | Busy cps | Median ms | Gate |
|---|---:|---:|---|
| fork0 | 2406 | 94 | all four checks passed |
| fork5 | 2518 | 103 | all four checks passed |
| offline1 | 2096 | 109 | all four checks passed |
| byz1 | 2086 | 111 | SAFE; consistency checks failed |
| fork5-silent1-byz1 | 2572 | 218 | all four checks passed |

**The overall benchmark gate is not passed at this phase.** In byz1 every PR
closed epochs 1 and 2 on the same state, but `EPOCH_FRONTIER_MISSING` omitted a
frontier delegation on some PRs. Committee digests diverged at epoch 1 and
remained different at epoch 2. The existing instance-dependent installation
must be completed from ledger blocks (piece 3) before accepting the final gate.
Failed logs are retained, not replaced with a successful verdict.

The harness now allocates its own temporary data directory, records node PIDs,
checks their command path before cleanup, handles termination signals, and
sets nanospam logging explicitly. It neither deletes ~/NanoSpam nor uses broad
pkill patterns. It aborts if the idle check never succeeds. The initial attempts
in the subdirectories were invalid due to suppressed throughput logs and then
leftover test-port listeners after interruption; they are not performance runs.
No compilation ran concurrently with the matrix. `DID NOT FINISH` means the
harness used its established 15-zero-second stop condition and subsequent RPC
checks, rather than nanospam's own completion summary.
