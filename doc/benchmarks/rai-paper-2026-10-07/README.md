# Paper matrix — 7 October 2026 (run5)

**All nine variants settled on `172fb25b6`**, one run each: six equal-weight representatives (f = p = 1), 45,000 blocks at 2,000/s, 45,000 accounts, 8 s epochs, quiet host (`--wait-quiet 900`), `--timeout 150 --settle-timeout 90`. Settled means identical cemented state and identical decided checkpoints on every running node. These are the RAI rows of the paper's evaluation table (`RAI_LaTeX/tools/eval_table.py run5 <out> run2`).

| Variant | Status | Uncemented, same on all | Close rounds | Last close, median / max | Non-fork goodput | Client p50 / p95 / p99 | Symbols per item / dropped streams |
| --- | --- | --- | --- | --- | --- | --- | --- |
| nofork | settled | 0 | 0, 0, 0 | 1.89 / 3.08 s | 1,942 blocks/s | 108 / 2403 / 3399 ms | 1.38 / 0 |
| fork5 | settled | 2096 | 0, 0, 0 | 2.07 / 3.94 s | 1,807 blocks/s | 133 / 4169 / 4940 ms | 2.02 / 0 |
| fork10 | settled | 4293 | 0, 0, 0 | 4.13 / 4.35 s | 1,440 blocks/s | 401 / 3915 / 4797 ms | 1.56 / 0 |
| nofork-offline1 | settled | 0 | 0, 0, 1 | 1.79 / 3.90 s | 1,978 blocks/s | 117 / 1432 / 1962 ms | - / 0 |
| fork5-offline1 | settled | 2307 | 0, 0, 1 | 3.15 / 4.08 s | 1,815 blocks/s | 126 / 2070 / 3013 ms | 3.56 / 0 |
| fork10-offline1 | settled | 4381 | 0, 0, 1 | 5.38 / 7.81 s | 1,267 blocks/s | 203 / 8188 / 8820 ms | 1.53 / 0 |
| nofork-byz1 | settled | 0 | 0, 0, 1 | 1.57 / 3.75 s | 1,969 blocks/s | 117 / 1280 / 1897 ms | - / 0 |
| fork5-byz1 | settled | 2241 | 0, 0, 1 | 2.70 / 4.00 s | 1,750 blocks/s | 125 / 2191 / 2802 ms | 1.68 / 0 |
| fork10-byz1 | settled | 4481 | 0, 0, 1 | 4.57 / 7.59 s | 1,253 blocks/s | 281 / 5287 / 8469 ms | 1.66 / 0 |

## run7: late notarization

The tail of run5 is the predecessor gate: a block published after an epoch boundary has no closing-epoch NC, so the core overlap exception cannot finalize it and it waits for the checkpoint (1,900 to 8,500 instances released per boundary, client throughput 0 for 1 to 3 s at every close). In run7 each member of the closing committee, after leaving epoch e-1 and until it installs S_{e-1}, casts a notarization-only vote in e-1 for every block it first-votes in e (`VoteType::LateNotar`, signed as a NotarVote, own generator), unless it voted another block at that position in e-1. Late votes are recorded apart (`VoteRecords::late_notar`) and count only towards the closing-epoch NC of the overlap exception (`closing_notarized`); reports, manifests, evidence replies and `certificate_kinds`, which the checkpoint construction reads, never see them, and they count towards no fast or final certificate. They open no instance and are not cached. Safety is the same-domain account exclusion: one member supports one block per position in e-1 across first and late votes. Same workload and settings as run5, binary built from the uncommitted tree on `172fb25b6`:

| Variant | Status | Uncemented, same on all | Last close, median / max | Non-fork goodput run5 → run7 | Client p50 / p95 / p99, run5 | Client p50 / p95 / p99, run7 |
| --- | --- | --- | --- | --- | --- | --- |
| nofork | settled | 0 | 1.48 / 2.65 s | 1,942 → 1,946 | 108 / 2403 / 3399 | 104 / 305 / 519 |
| fork5 | settled | 2106 | 1.87 / 3.91 s | 1,807 → 1,809 | 133 / 4169 / 4940 | 117 / 1279 / 2274 |
| fork10 | settled | 4361 | 3.52 / 5.13 s | 1,440 → 1,627 | 401 / 3915 / 4797 | 141 / 813 / 1653 |
| nofork-offline1 | settled | 0 | 1.43 / 3.86 s | 1,978 → 1,970 | 117 / 1432 / 1962 | 110 / 176 / 270 |
| fork5-offline1 | settled | 2245 | 3.48 / 3.86 s | 1,815 → 1,817 | 126 / 2070 / 3013 | 127 / 351 / 546 |
| fork10-offline1 | settled | 4482 | 6.14 / 8.57 s | 1,267 → 1,546 | 203 / 8188 / 8820 | 187 / 2111 / 2857 |
| nofork-byz1 | settled | 0 | 1.48 / 3.62 s | 1,969 → 1,980 | 117 / 1280 / 1897 | 109 / 186 / 291 |
| fork5-byz1 | settled | 2194 | 3.57 / 4.08 s | 1,750 → 1,827 | 125 / 2191 / 2802 | 124 / 374 / 807 |
| fork10-byz1 | settled | 4377 | 6.58 / 6.87 s | 1,253 → 1,550 | 281 / 5287 / 8469 | 181 / 956 / 2963 |

Without forks the tail is back at or below run2 (which had the unproven predecessor-backed route): nofork p95 280 / 2403 / 305 ms in run2 / run5 / run7. With forks it remains above run2 for two reasons: blocks whose late votes split or miss one voter still wait for the gate (400 to 1,800 instances per boundary, 79 to 89% of late-voted blocks reach their NC), and in the offline variants the last close takes a second round (6.6 to 8.6 s) that the final client blocks wait for. The skipped-split rule does nothing with an offline member, whose weight counts as possibly still voting. A one-second confirmation dip right after an install remains; run2 shows a smaller one at the same place.

## The day's passes

| Pass | Build | Result |
| --- | --- | --- |
| [run2](run2/) | `a4872d09e`: re-voted-parent fix, predecessor-backed overlap route, no manifest, in-memory records | 9/9 settled; the paper's comparison table of the earlier build |
| [run3](run3/) | `617eeffde`: prefix-wide overlap rule, manifest, install acknowledgements, durable records | 8/9; fork5-byz1 left 337 non-fork blocks unfinalized (first votes split across a boundary leave a recovery lock nobody re-votes) |
| [run4](run4/) | `73ac5d978`: every unfinalized instance re-voted in the new epoch | 9/9 settled, but fork10-offline1 and fork10-byz1 never finished the client measurement: carried fork duplicates filled the election container (cap 5,000) and nothing new was activated |
| run5 (this) | `172fb25b6`: manifest fetched from the proposer, owner continuation in the generator, re-voted recovery lock continues on an open-epoch NC, idle duplicates discarded at install, cap 20,000 | 9/9 settled and measured; the paper's evaluation table |
| [run6](run6/) | run5 + four handoff fixes (O(n²) dedup in `finalized_blocks_in`, gate released right after `install_checkpoint`, signing records in their own `signing.ldb`, unresolvable 3-3 splits not carried) | stopped after three variants on request; nofork p95 2403 → 1269 ms |
| [run7](run7/) | run6 + late notarization (below) | 9/9 settled; p95 4-8x lower than run5 in every variant |

The build before all of these (`25646c27e`, [paper-run1](../rai-gate-d-2026-10-07/paper-run1/)) never closed an epoch in fork10 and fork5-offline1: a block voted in two consecutive epochs lost its recorded parent when the older epoch's slot state was dropped, so the next report carried a residual record with a zero parent and `BuildState` failed at every leader; fixed in `a4872d09e`, which also refuses malformed reports.

Against run2 the medians are within 10 to 30 ms without forks and at 5% forks, while p95 and p99 are an order of magnitude longer and 10%-fork goodput 15 to 25% lower: the prefix-wide overlap rule holds every block published during a close until the checkpoint installs, closes grew (report freeze 0.4 to 0.9 s with the durable write of the frozen sets, reconciliation 1.1 to 2.4 s with two to four times as many retained positions), and an epoch cannot end before the previous close decides. Per-epoch phase timings come from `phases.py` over `run.log` (ENDED, REPORT, RECONCILED, CLOSE_READY, PROPOSED, MANIFEST_FETCHED, CLOSED).

Uncemented positions are forks whose votes split, retained identically everywhere; nanospam never finishes on forks, so settlement is judged on the nodes. Reproduce one variant:

```sh
python3 tools/rai/run_gate_b.py <dir> --wait-quiet 600 --fork-percentage 10 --offline 1 --timeout 150 --settle-timeout 90
```

