# No-crash evaluation results (2026-09-24)

Binaries and hashes: `/tmp/rai-cross-epoch-artifacts/build-manifest.json`
(`no_crash`, `no_crash_v2`), pinned under `bin/no-crash*/`. Baseline:
`bin/baseline/rsnano` (5e037cfe0). One shared client per batch, built from
the candidate commit and used for both nodes. Host as in PERFORMANCE.md;
LMDB `nosync_unsafe`; **no-crash performance with volatile vote records**.
Every attempt below is preserved; generated databases were deleted after
evidence capture and process-group shutdown (`data-cleanup.json`).

## Attempt 1: paper-model fork diagnostic, membership defect (`no-crash-fork-paper-v1`)

Candidate `368f154cb` (bin/no-crash), 5 % forks, equal-weight model with
`f = p = 1`, certificate-only finalization, 156 s ceiling. Not a
performance run.

| Observation | Result |
|---|---:|
| Input complete (45,000 primaries) | yes |
| Fork pairs published / branch hashes | 2,216 / 4,432 |
| Epochs closed and installed on all six nodes | 0 and 1 (round 1 and round 2), identical roots |
| Epoch 2 close | stalled: `usable=4 required=6` on every node |
| Branches terminal on all six nodes | 101 (all `included_Recovery`) |
| Unresolved by the observer | 4,331 |
| Sketch requests answered | 2 replies, both `incomplete` at the 64-cell sketch; the reports were then reconstructed by root-based differences before a larger sketch was needed |
| Evidence checks / missing entries | 52 / 0 (35,883 entries per epoch-2 report) |
| Recovery-lock discards, checkpoint conflicts | 0, 0 |
| Carried R entries in frozen reports | 189 (epoch 1), 101 (epoch 2) on every node; earlier batches had none |
| Conditional termination upper bounds p50/p95/p99 (101 branches) | 25,301 / 64,448 / 64,487 ms |

Cause: the equal-weight membership rule admitted every identity holding
nonzero delegated weight, so the genesis funding representative (≈0 %
weight) became a seventh member; `N − f = 6` usable reports were required
of six reporters and any single unusable report stalled the close. This is
an implementation defect, not a protocol cost. Superseded by attempt 2; the
attempt stands as recorded. What the run did establish: every node
reconstructed and installed the same epoch-0 and epoch-1 checkpoints (the
previous diagnostic left PR4 without either), the sketch path was exercised, and every N/F entry of every
reconstructed report was justified from retained signed votes.

## Attempt 2: paper-model fork diagnostic, six members (`no-crash-fork-paper-v2`)

Candidate `e1f271c49` (bin/no-crash-v2), same workload and switches, 156 s
ceiling (163 s wall). Not a performance run.

| Observation | Result |
|---|---:|
| Input complete | yes |
| Fork pairs published / branch hashes | 2,271 / 4,542 |
| Committee | 6 members, `q = 4`, as configured |
| Epochs closed on all six nodes | 0 (round 2) and 1 (round 1) |
| Epoch 2 | closed and installed on PR1–PR5 (round 0); not on PR0 |
| Branches terminal on all six nodes | 1,553 |
| Unresolved by the six-node observer | 2,989 |
| Per-node dispositions | included Finalized 571, included Notarized 142, included Recovery 10,739, discarded by certificate-final witness 681, unresolved 15,119 |
| Derived-final entries, checkpoint conflicts | 0, 0 |
| Conditional termination upper bounds p50/p95/p99 (1,553 branches) | 22,023 / 27,955 / 28,557 ms |
| Evidence requests served (all nodes) | 181 |

PR0 (the node the client's RPC and publishing load lands on) reconstructed
every epoch-2 T but could never justify one entry of one report, and its
evidence recheck oscillated between 1 and 9,756 missing entries: the recheck
fetched certificates only for the previously missing hash while verifying
the whole state. Both are implementation defects fixed after this run:
the recheck now covers every tagged entry each time, late finality is
searched in all retained epochs, and the diagnostic names sample missing
entries. Without epoch 2, PR0's epoch-3 projection lacked its base and its
sketches could not peel (109 incomplete at 1,024 cells): that is the
expected consequence of the missing predecessor, not a separate fault.
The 1,553 terminations include certificate-backed finality on both sides
of forks for the first time in this series.
