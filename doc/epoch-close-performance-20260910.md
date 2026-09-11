# Epoch-close recovery performance follow-up

The working tree at the start of this continuation already contained a change that delays construction of new local recovery snapshots until draining completes. Existing immutable snapshots remain available during drain. That change and its regression are preserved and included in the validation below.

## Reproducible retry-selection defect

`Recovery::requests` selected peers with one global counter shared by every candidate and page. With two pending requests and two serving channels, each retry increments that counter twice, so each request selects exactly the same channel on every pass. The same aliasing can occur for other batch/source counts. A peer that cannot reconstruct the target or lacks the shared base may therefore be selected indefinitely even though another advertised peer could answer.

The deterministic `epoch_close_recovery_retries_rotate_each_page_source` regression creates two authenticated candidate headers with two available channels each, issues requests, expires their existing retry intervals, and checks that each retry selects another channel. Before the correction it fails with channel 1 selected again. Afterward all 17 close-related tests pass.

The correction retains the next peer index independently for each candidate/page alongside its request time. Existing retry intervals, page limits, shared nonempty-base requirements, signatures, digest verification, consensus deadlines, thresholds, and certified membership remain unchanged. Request state is released when its candidate is complete or removed.

This establishes a request-selection defect, not its contribution to the earlier 404.64-second run. Historical compact progress records show epoch-0 rounds reaching 45 and up to 23 incomplete candidates on a node. They do not record which peers were selected for each page.

## Validation artifacts

Under `target/nanospam-debug-20260910/`:

- `performance-rotation-before.log`: deterministic regression failure before the retry fix.
- `performance-close-after.log`: 17 passing close-related tests.
- `performance-release-build.log`: RAI release build output.

The RAI release build passed. Same-load results follow; no higher load was started.

## Same-load combined-change run: PASS

`b51260-r1026-performance-01` passed in **114.462 seconds**, compared with 404.64 seconds in the prior timeout-recovery run (71.7% less wall time). Six fresh PRs agreed on exactly two closed epochs, with 40-second epochs, 51,260 requested blocks/accounts, 1,026 blocks/s, 5% forks, and `--no-prio`. Compact PR0 outcomes were enabled; full tracing/audit and validation diagnostics were disabled. No compilation overlapped the benchmark. Shutdown was clean and successful node data was deleted.

This run combines the pre-existing snapshot-during-drain change with the independent retry rotation correction. Randomized workloads and both changes prevent assigning the improvement to either change individually. Epoch-0 closes occurred at rounds 12–13, compared with approximately 45 previously. Epoch-0 nonfork finalization mean/p95 increased from 104/173 ms to 133/356 ms even though aggregate completion improved substantially.

Primary timing is first publication to PR0 WebSocket receipt, in milliseconds. Counts are election roots deduplicated within each epoch; roots may recur across epochs.

| Epoch | Roots | Terminated | Mean / p95 / max | Finalized | Mean / p95 / max |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 37,413 | 138.183 / 397.297 / 4,008.096 | 36,168 | 132.765 / 355.686 / 2,058.756 |
| 0 | Fork | 1,946 | 432.511 / 949.235 / 13,215.745 | 8 | 690.890 / 903.087 / 903.087 |
| 1 | Nonfork | 12,539 | 10,566.456 / 22,740.509 / 33,387.491 | 12,493 | 11,964.449 / 23,765.489 / 34,849.340 |
| 1 | Fork | 2,546 | 27,126.497 / 50,529.834 / 64,030.093 | 0 | — |

PR0 polling first observed epoch 0 closed at **61.970 seconds** after the schedule anchor, versus 194.542 seconds previously. Publication had already ended; there are **no fresh post-close publication windows** for assessing normalized epoch-1 consensus latency. Epoch-1 aggregates still include boundary and recovery delays. Detailed cohorts, FIRST-to-outcome durations, and publication windows remain in `b51260-r1026-performance-01.json`. Some detailed outcomes occur in voting epoch 2 while lagging peers finish; exactly epochs 0 and 1 were closed, as verified by the runner.

Epoch digests:

- Epoch 0: `9A44824ED81165879F38C9D3EFACB81F0CC025ADE53BFF66392E5F591E6D4162`.

- Epoch 1: `69D708F8C0B2B19D41802951961B089EC4F5378F9B7F5914B1C3B7378E11D24D`.

Evidence: `b51260-r1026-performance-01.json`, `b51260-r1026-performance-01.log`, and `b51260-r1026-performance-01-pr0-counts.json` under `target/nanospam-debug-20260910/`. No higher load was started. Both historical failed datasets and `kudzu.pdf` remain untouched.
