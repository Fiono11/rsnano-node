# Epoch-close convergence changes

The objective is faster close decisions with different local memberships, not identical drain sets. Recovery remains digest-only voting plus separate shared-nonempty-base deltas. No pre-publication checkpoint was added.

Implemented changes:

- Drain admission counts distinct participating representative weight across FIRST, NOTARIZE, FINAL, FIRST_TIMEOUT, and TIMEOUT, once per root/epoch. The threshold remains strictly above f. Local FIRST obligations and the existing termination condition remain intact.
- Once the epoch deadline starts draining, retain local workload snapshots and announce their digests using authenticated advisory message kind 8. Announcements cannot enter vote tallies or certify anything. Announcement admission is bounded to 64 candidate entries; authenticated voting evidence retains its separate admission rules. Actual close signing still waits for drain completion.
- Before immutable FIRST is signed, adopt the smallest-ID reconstructed, valid current-round proposal with the selected parent that contains every local required member. Otherwise propose the local snapshot. Existing FIRST statements are never replaced.
- Prioritize notarization-certified snapshot recovery ahead of uncertified future candidates, retaining the existing per-page retry rotation and bounded request budget.
- For complete certified snapshots, request hashes missing from local eligible membership using ordinary epoch-specific ConfirmReq messages. Requests are bounded and rotate among advertising sources. Hash-only recovery can locate active unsaved forks without knowing their root. This requests ordinary block/vote evidence; it does not treat a close certificate as a block certificate or bypass membership validation.
- An authenticated old-epoch vote triggers bounded direct replay of retained close ancestry to its sender. Per-channel cursors rotate through the archive, and the existing shared-base delta service remains available. Periodic broadcast and archive acknowledgment rules remain intact.
- Compact EPOCH_DRAINED and EPOCH_CLOSED records include PID and wall-clock milliseconds, separating workload draining from subsequent close-election duration.

Validation before the benchmark: 674 node unit tests, four close-message tests, and three recovery integration tests passed. The feature-disabled node check passed with existing warnings. Regression coverage includes distinct participation, advisory-message authentication/non-voting semantics, certified recovery priority, valid-superset adoption constraints, immutable FIRST, snapshot recovery during draining without signing, and hash-only unsaved-fork lookup.

Message kind 8 is a RAI wire-format extension. Compatibility with mixed old/new nodes has not been established; the benchmark uses six upgraded nodes. The changes do not prove a shorter close in every network schedule, and the expanded drain set may increase workload draining. Benchmark results are required before claiming a performance improvement.

## Six-PR result

The unchanged 51,260/1,026 run passed in 361.909 seconds with exactly two verified closed epochs. Shutdown was clean, successful data was deleted, and no compilation overlapped the benchmark. All PRs closed both epochs at round indices 0–1.

Epoch 0 persisted at 67.922–68.882 seconds after the shared start (27.922–28.882 seconds after its deadline). Four PRs completed their drains at 59.688–63.990 seconds and then closed 4.046–8.281 seconds later. Two PRs accepted a valid reconstructed certified decision before completing their own drains, as allowed by the existing decision-recovery path; they did not sign early. The previous clean run observed PR0 epoch 0 closed at approximately 119.9 seconds and closed at round indices 41–42. This randomized single-run comparison supports fewer epoch-0 close rounds but does not isolate which change caused it.

Epoch 1 persisted at 339.356–342.735 seconds, also at indices 0–1. Two PRs were ready at 187.337–189.110 seconds, while two others became ready only at 302.761 and 328.927 seconds. The early voters spent about 150–153 seconds waiting for close. Thus fewer close rounds did not remove the delay waiting for other PRs to finish workload recovery/draining. Overall runtime increased from the preceding clean run's 250.857 seconds; no end-to-end improvement is claimed. The broader participation condition and extra recovery work have not been individually isolated.

Publication-to-PR0 WebSocket receipt, milliseconds:

| Epoch | Roots | Terminated count | Mean / p95 / max | Finalized count | Mean / p95 / max |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 48,722 | 384.178 / 1,412.218 / 3,279.720 | 36,401 | 182.819 / 494.085 / 1,222.682 |
| 0 | Fork | 2,538 | 823.800 / 4,342.520 / 10,153.026 | 2 | 428.271 / 654.444 / 654.444 |
| 1 | Nonfork | 12,268 | 31,834.503 / 48,065.192 / 138,687.050 | 10,724 | 31,579.928 / 44,005.739 / 69,190.111 |
| 1 | Fork | 1,087 | 72,587.738 / 175,046.674 / 296,157.611 | 0 | — / — / — |

Publication ended before epoch 0 closed; there are no fresh post-close publication windows. Aggregate epoch-1 outcomes include boundary/recovery time. Full results and maxima: `target/nanospam-debug-20260910/b51260-r1026-convergence-01.json`. Per-PR close/drain timings: `b51260-r1026-convergence-01-close-timing.json`.
