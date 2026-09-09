# In-memory RAI block tree

The node retains locally established notarized/finalized blocks by qualified root and epoch, plus timeout-only outcomes. Candidate forks do not modify the account ledger. Election tallies retain representative participation and vote kinds, rather than certificate signatures. Nodes are assumed continuously online; recovery does not survive restart.

Recovery reuses publish/confirm_req/confirm_ack. Each voting peer signs its own statements, with final votes also supplying notarization weight. Timeout recovery replays the peer’s own first-timeout or later-timeout statement. A nonvoting peer can supply block payloads but cannot transfer a quorum through an unsigned outcome claim. Existing shared signing restrictions and final locks remain enforced. Replies batch hashes by representative, epoch and kind.

Nanospam checks canonical outcomes for every published root directly from the tree, and compares finalized block-set digests by epoch. Pending-everywhere roots still fail. Detailed latency auditing is optional. The tree RPC is a local diagnostic snapshot; peers do not trust it as consensus evidence.

All runs use six PRs, no priority scheduler, accounts equal blocks, epoch length half the block count, 5% forks, and 100 ms vote batching. Node/account data is deleted before and after each run.

| Run | Outcome | Extra recovery | Finalized epoch digests |
| --- | --- | --- | --- |
| 5,000 blocks, rate 500, audited | All 5,000 roots terminated on every PR | 0 s | Equal on all six PRs |
| 15,000 blocks, rate 1,500, audit disabled | All 15,000 roots terminated on every PR | 0 s | Equal on all six PRs |

The first two runs precede batching of recovery reply signatures. PR0 latency in the 5,000-block run, measured from earliest local election insertion: nonfork finalization mean 81.7 ms (4,756 samples); fork termination mean 171.6 ms (244 samples). No nonfork finalization or fork termination samples were missing. These are not publication-to-WebSocket measurements.

Validation: 635 RAI node unit tests, 597 legacy node unit tests, three TCP recovery integration tests, and 33 nanospam tests (one ignored).

## Final batched-reply validation

The final 15,000-block/rate-1,500 run passed with all roots terminated on every PR and `finalized_ledgers_equal: true`. It required 14.46 seconds of additional observation for canonical agreement. All 14,207 nonfork roots finalized on PR0 within the performance window; all 793 fork roots terminated. Eight forks finalized, which is not the optimization target.

PR0 mean insertion-to-finalization latency for nonforks was 186.57 ms (p95 428.72 ms). Mean insertion-to-termination latency for forks was 369.57 ms (p95 798.27 ms). These single-run results are not a controlled comparison against earlier runs.

The remaining recovery tail concerned root `F979DBF3…44351DC`: PR3 notarized candidate `8A4A3C6C…` at 0.598 seconds, but learned the other notarized candidate `42B9CE8C…` at 63.042 seconds. The other five PRs notarized both by 0.678 seconds. Thus local root termination was prompt; agreement on the complete canonical notarized candidate set was late. The audit establishes the missing-candidate tail, but does not identify the precise network/request scheduling cause. Full root events are saved in `b15000-r1500-batched/recovery-diagnostic.json`.

All six PRs ended with 14,268 cemented blocks including setup blocks, and identical finalized block-set digests for epochs 0 and 1. Cleanup confirmed data deletion and process shutdown.

### Per-epoch local latency, final run

| PR | Epoch | Nonfork finalized samples | Mean finalization ms | Fork terminated samples | Mean termination ms |
| --- | --- | --- | --- | --- | --- |
| 0 | 0 | 7523 | 97.49 | 429 | 234.62 |
| 0 | 1 | 6684 | 286.83 | 364 | 528.61 |
| 1 | 0 | 7523 | 100.11 | 431 | 233.56 |
| 1 | 1 | 6684 | 303.35 | 362 | 500.91 |
| 2 | 0 | 7523 | 95.92 | 429 | 228.95 |
| 2 | 1 | 6684 | 290.02 | 364 | 506.03 |
| 3 | 0 | 7523 | 109.00 | 431 | 302.10 |
| 3 | 1 | 6684 | 294.74 | 362 | 507.99 |
| 4 | 0 | 7523 | 99.23 | 429 | 262.41 |
| 4 | 1 | 6684 | 290.86 | 364 | 504.61 |
| 5 | 0 | 7523 | 99.56 | 429 | 321.54 |
| 5 | 1 | 6684 | 315.12 | 364 | 522.51 |

The CSV includes p50/p95/p99/max for every row. Latencies use the earliest local election insertion as their start. Full per-PR distributions and censored sample counts are in `latency.json`.
