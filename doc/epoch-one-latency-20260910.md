# Epoch-1 latency investigation

The epoch-close omission fix passed six-PR runs from 4,500/90 through 34,173/684 blocks/second. Publication-to-PR0 outcome averages nevertheless increased in epoch 1.

## Measurements before scheduling correction

A 6,300-block / 90 blocks/s run published for approximately 70 seconds. PR0 RPC polling observed epoch 0 closed at 40.790 seconds (500 ms sampling resolution). All six PRs agreed on exactly two epoch hashes. Full vote tracing and audit were disabled; one FIRST-to-outcome duration was added to the existing compact PR0 outcome message.

For nonfork roots with no prior-epoch outcome:

| Publication window, seconds | Epoch | Roots | Publication → termination, mean ms | FIRST → termination, mean ms |
|---|---:|---:|---:|---:|
| 25–30 | 0 | 421 | 65.21 | 34.20 |
| 30–35 | 0 | 425 | 65.13 | 36.45 |
| 35–40 | 0 | 409 | 63.87 | 36.35 |
| 45–50 | 1 | 424 | 111.41 | 69.04 |
| 50–55 | 1 | 431 | 109.58 | 68.72 |
| 55–60 | 1 | 431 | 107.55 | 68.69 |
| 60–65 | 1 | 425 | 106.94 | 67.47 |
| 65–70 | 1 | 387 | 112.20 | 73.04 |

Thus the persistent difference cannot be explained solely by waiting for epoch 0 to close. A separate 4,500-block control with nanospam republication disabled also retained the FIRST-to-termination difference (fresh nonfork means: epoch 0 36.57 ms; epoch 1 62.94 ms).

## Concrete scheduling defect

`SharedState::run` used one global `next_broadcast` deadline. At a wakeup, `broadcast` serviced only the epoch of the queue’s first candidate. It rotated candidates from other epochs back into the queue, then `run` reset the global deadline. Old-epoch recovery could therefore leave already-ready epoch-1 work waiting another configured batching interval. The configured default is 100 ms. The resulting delay can repeat after the old epoch closes because notarized, unfinalized elections remain available for recovery.

A deterministic regression queued an epoch-0 candidate, its fork, and an epoch-1 candidate. Before the correction, the epoch-1 candidate remained pending after broadcast. The assertion failed in 0.01 seconds. After the correction, each distinct ready epoch receives its own batch in the same wakeup, while the second value for the same root remains pending for a separate batch. The set of epochs is captured before processing to bound the pass. Votes still carry one epoch each, and signing/spacing restrictions are unchanged.

All five vote-generator fork/recovery tests pass. The controlled six-PR rerun passed with all PRs agreeing on exactly two epoch hashes; successful-run node data was deleted.

## Controlled rerun after correction

The same 6,300-block / 90 blocks/s workload published for 69.70 seconds. PR0 polling observed epoch 0 closed by 41.162 seconds. Fresh nonfork publication windows after that close measured:

| Publication window, seconds | Roots | Publication → termination, mean ms | FIRST → termination, mean ms |
|---|---:|---:|---:|
| 45–50 | 430 | 74.19 | 48.59 |
| 50–55 | 422 | 71.10 | 47.22 |
| 55–60 | 424 | 70.26 | 43.47 |
| 60–65 | 417 | 70.56 | 44.07 |
| 65–70 | 405 | 71.35 | 47.91 |

In the same rerun, the final three complete epoch-0 windows had FIRST-to-termination means of 39.97, 41.24, and 43.16 ms. The persistent 67–73 ms post-close plateau was reduced to 43–49 ms. This supports the shared batching deadline as a concrete cause; the randomized rerun does not establish exact equality of epoch distributions.

Corrected results: `/private/tmp/nanospam-fix-20260910/b6300-r90-independent-batches.json` and `b6300-r90-independent-batches-pr0-counts.json`.

## Measurement interpretation

The requested primary metric remains first publication to PR0 WebSocket receipt. It intentionally retains pre-epoch time for repeated roots. FIRST-to-outcome uses PR0’s monotonic clock from its first accepted non-timeout FIRST vote to its local certificate transition, excluding WebSocket delivery. Timeout-only outcomes can have no FIRST sample and must not be treated as zero-duration consensus. Publication minus FIRST duration includes both pre-FIRST waiting and outcome delivery; it is not a pure queue-time measurement.

Evidence: `/private/tmp/nanospam-fix-20260910/b6300-r90-windows.json`, `b6300-r90-windows-pr0-counts.json`, and `b4500-r90-no-republish.json`. Successful-run node data was deleted.
