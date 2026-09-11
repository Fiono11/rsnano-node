# Epoch-1 contention investigation

Objective: bring fresh epoch-1 election latency close to epoch 0 at 51,260 blocks/accounts, 1,026 blocks/s, six PRs, 5% forks, no priority probes, 40-second epochs and exactly two verified closes. Protocol pause/report rules, f+1, old-vote recovery and immutable decided membership remain unchanged.

## Measured bottleneck

The `b51260-r1026-profile-01` diagnostic run sampled PR0 for one second at 30, 43, 48 and 53 seconds after the schedule anchor. Full tracing/audit stayed disabled and no compilation overlapped the workload. Samples are under `target/nanospam-cut-20260910/`, named `b51260-r1026-profile-01-pr0-<offset>s.sample.txt`. Sampling perturbs the run; it is not an uninstrumented latency comparison. After evidence collection the run was deliberately stopped at 162.285 seconds and its data preserved.

The post-boundary samples show voting threads blocked acquiring AEC access while vote application waits inside its inlined ledger certificate-registration path. At 48 seconds one vote-processing thread had 527 samples at `ActiveElectionsContainer::apply_vote + 27680`, descending through a semaphore wait. At 43 seconds the normal voting thread had 322 samples waiting in `AecService::kudzu_candidates`; the final voting thread had 220 at the same AEC access. The close thread spent 518 of 713 samples under `State::drive`, including snapshot B-tree construction and membership validation. At 53 seconds it spent 570 of 731 samples in that drive path.

The code's lock chain is: `AecService::apply_vote` holds the AEC write lock; `ActiveElectionsContainer::apply_vote` synchronously calls `Ledger::record_epoch_block`, which needs the epoch-block index write lock. Meanwhile `epoch_close_candidate` and `epoch_close_candidate_valid` held that index's read lock across whole-ledger membership scans and dependency checks. This blocks certificate application and consequently AEC readers, including voting and scheduling. ARM64 disassembly of the sampled inlined apply-vote address confirms the wait is in a queued RwLock path adjacent to the certificate-index B-tree insertion.

## First correction

Capture only `(hash, epoch, dependencies)` from the certificate index under a short read lock; build the lookup map, scan the database and validate dependencies after releasing it. Block payloads are not cloned into this snapshot. Candidate construction, validation and diagnostics use the same helper. A concurrent certificate may appear in a subsequent candidate, while the captured metadata remains unchanged. Certificate registration remains synchronous under the AEC lock, preserving the drain/membership invariant.

A ledger regression captures metadata, verifies the index permits a writer while the captured metadata lives, records later and earlier-epoch certificates, and checks that future candidates observe them without changing the captured metadata. All 170 ledger and 667 node unit tests passed with RAI enabled.
b51260-r1026-metadata-lock-01: success=True, wall=100.375s
| Epoch | Roots | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 36,514 | 95.026 / 132.629 / 9,547.014 | 36,400 | 119.331 / 149.610 / 9,680.764 |
| 0 | Fork | 1,906 | 323.867 / 326.097 / 10,047.912 | 2 | 175.548 / 183.040 / 183.040 |
| 1 | Nonfork | 12,413 | 2,103.399 / 3,275.703 / 9,653.957 | 12,291 | 2,444.382 / 3,703.322 / 10,019.883 |
| 1 | Fork | 2,506 | 19,192.144 / 42,856.867 / 55,478.159 | 4 | 6,522.973 / 16,501.321 / 16,501.321 |

Fresh nonfork windows: epoch, start, publication-final mean/p95, FIRST-final mean/p95
0 25 104.179 157.274 59.137 99.097
0 30 105.859 146.106 61.6 90.324
0 35 288.414 177.827 234.892 107.866
0 40 None None None None
1 40 1723.507 2702.475 908.845 1754.792
1 45 3048.713 4125.085 1579.078 2570.122
1 50 2607.026 3507.262 1214.38 2058.624
digests {'success': True, 'epochs_checked': 2, 'closed_epochs': {'0': '47382ECDB17A3871745BDDD053FB9B6FB3D1A6BD60C7669CA97A07FF7C503B44', '1': '75DCFEF3125A6EDDD00FF492B0787B34D9C338A3DBADE71B70FA8AA2CB77603E'}}

The metadata-lock run passed all six PRs and two epochs in 100.375 seconds, but fresh epoch-1 latency remained above epoch 0. The table and windows above retain the measured result; this was an intermediate improvement, not achievement of the objective. No profiling or compilation overlapped that run and successful node data was removed.

## Compact snapshot work

The next correction addresses the allocations and B-tree work in the sampled close path. The certificate index now stores the existing two-slot `DependentBlocks` value directly instead of a separately allocated dependency vector. Capturing metadata copies fixed-size values into one vector and releases the lock before constructing the lookup table. Dependency closure uses hash-based membership and lookup, then sorts the final hashes once to retain deterministic digest semantics. Validation enumerates prior canonical membership once instead of performing a database lookup and allocating a key for every candidate hash. It still checks every candidate's eligible epoch and dependencies, prior closed membership, sorting and uniqueness. No snapshot cache or relaxed validation was introduced.

All 170 ledger and 667 node unit tests passed after this change.

The compact-snapshot uninstrumented run failed its existing close deadline after 463.578 seconds. All six PRs closed epoch 0 with digest `1A734B43D909B4E7AE8C49C5C07C8E4B8F8B2F04029840B50EF0DEBC03020D3C`; epoch 1 retained one common pending cut root. Its data is preserved. The captured RPC evidence is in `compact-pending-active.json` and `compact-pending-info.json` under the benchmark artifacts. The root begins `C3E66E1A6EC4D92E`; both competing hashes have epoch-0 notarizations. The general confirmation-info RPC resolves the earliest retained election, so its tallies do not diagnose the pending epoch-1 instance.

A second diagnostic run (`b51260-r1026-profile-02`) collected the same four stack samples with the compact metadata implementation. Unlike the first profile, the close thread no longer dominates CPU in snapshot construction. At +53 seconds both normal and final reply generators spent all 627 samples blocked in `VoteProcessorQueue::enqueue_local`; the local queue was full. Vote-processing workers mainly waited on election-access locks. This run also retained a pending epoch-1 cut root and was deliberately stopped after collecting evidence; it is not a latency comparison.

## Solicitation and participation correction

A diagnostic build measured AEC lock hold times above 20 ms in `b51260-r1026-lock-diagnostic-01`. Election scans in the ticker reached 690 ms and voter scans reached 288 ms; vote application reached 356 ms. This tracing perturbs timing and is diagnostic only. The temporary lock timers were removed afterward. The pending-cut diagnostic showed epoch-1 forks with empty block FIRST/notarization/final tallies and only three FIRST-timeout participants despite timeout eligibility. The diagnostic workload was stopped and data retained.

The periodic confirmation solicitor previously cloned and solicited every active retained epoch, including frozen non-cut elections. It now takes a snapshot of the same pause/cut signing filter before accessing AEC, excludes frozen elections before expensive cloning, and continues periodic solicitation for current-epoch and resumed-cut elections. Explicit recovery and old-vote application are unchanged.

The generator previously recovered its original FIRST hash only when the requested fork had second-look support. A later-epoch FIRST-timeout has no block weight and needs no such support. A representative locked to its original value could therefore remain silent when asked about the other fork. The generator now attempts original-hash participation through the existing authorization rules independently of that condition. A regression signs FIRST and FINAL for one fork in epoch 0, persists that final lock, requests the other fork without second-look support in epoch 1, and requires exactly an epoch-1 FIRST-timeout routed through the original hash. No authorization threshold or final lock is weakened.

The solicitation/participation patch passed all 668 RAI node unit tests. Its benchmark must still establish the latency objective; passing the new liveness regression alone is not sufficient.

The uninstrumented `b51260-r1026-solicitation-01` workload still had 2–4 second confirmation windows after the boundary. Five PRs closed epoch 0, then reported empty epoch-1 pending sets; one PR remained in epoch-0 snapshot recovery. It was deliberately stopped at 214.593 seconds with data preserved. No successful two-epoch result is claimed.

A further redundant-work path remained: timeout-certified elections retain state `Active`, and the solicitor treated their absent block notarizations as missing votes forever. The periodic solicitation/broadcast scan now excludes timeout-certified elections, while explicit recovery still retains their evidence. The AEC voter also uses the pause/cut filter before scanning per-election tallies. A regression confirms that an active-state election becomes ineligible for periodic solicitation after receiving a valid five-of-six timeout certificate. All 669 node unit tests passed.

## Snapshot refresh cadence

`b51260-r1026-timeout-solicitation-01` still showed multi-second confirmation windows after epoch 0. It was reclassified as diagnostic when a late two-second PR0 stack sample was captured; that sample arrived after most backlog had drained and is not evidence about the boundary cause. The run was stopped and retained. `b51260-r1026-profile-03` then sampled PR0 precisely at 38, 40, 42, 45 and 48 seconds. At 42 seconds, `epoch_block_metadata` accounted for 185 of 679 samples, with 260 samples in snapshot drive work and another 245 waiting on election access. All scheduled samples were captured before stopping that diagnostic and retaining its data.

The close driver rebuilt full local snapshots every 100-ms tick even after signing the current round's immutable FIRST. It now rebuilds immediately whenever an eligible local signer needs a proposal, and otherwise refreshes local recovery bases at the existing two-second retransmission cadence. Candidate membership validation and decided membership are unchanged; there is no cache of validation results beyond the preexisting candidate validation cache, no new quorum/round deadline and no full-list recovery fallback. The leaderless refresh regression now explicitly advances the recovery-refresh clock, checks that an immediate tick does not rescan, and retains its original assertions about fresh recovery contents and immutable FIRST. All 669 node unit tests passed.

The snapshot-cadence run showed a faster epoch-1 tail (one-second confirmation windows fell to 150–200 ms), but retained an early multi-second burst and did not converge. It was stopped and retained. Suppressing all periodic frozen-election solicitation had removed a needed old-vote recovery path; the final implementation restores that path in rotating batches of `ConfirmReq::HASHES_MAX` elections per ticker pass. This bounds recovery work while eventually revisiting every frozen election. Current/cut solicitation remains eligible, and timeout-certified elections are still retained for explicit recovery.

A further queue issue is that `batch_size=1024` previously leased 1,024 whole vote packets to each worker. A packet can hold 255 hashes, so recovery can occupy workers with hundreds of thousands of hash applications before they revisit fresh queued work. RAI batches now budget actual hash applications (a filtered cache replay costs one), retaining the existing local/remote split and channel fair-queue ordering. The final packet may exceed the budget by at most 254 hashes. Non-RAI batching continues to count packets. A regression queues twenty full recovery packets, verifies a 1,024-work batch takes five packets, then verifies a newly queued local vote is included first in the next batch. All 670 node tests passed before replacing a one-item temporary dequeue with the equivalent direct fair-queue pop.
b51260-r1026-bounded-recovery-01: success=True, wall=103.877s
| Epoch | Roots | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 36,265 | 169.691 / 484.098 / 4,951.577 | 35,976 | 189.310 / 523.780 / 3,548.944 |
| 0 | Fork | 1,962 | 387.405 / 930.949 / 5,541.424 | 15 | 705.141 / 1,140.838 / 1,140.838 |
| 1 | Nonfork | 12,728 | 797.060 / 1,328.618 / 15,375.967 | 12,603 | 857.553 / 1,421.321 / 3,350.937 |
| 1 | Fork | 2,622 | 17,530.339 / 40,014.380 / 44,785.744 | 13 | 1,689.629 / 3,733.986 / 3,733.986 |

Fresh nonfork windows: epoch, start, publication-final mean/p95, FIRST-final mean/p95
0 25 172.21 758.039 79.174 224.225
0 30 397.866 721.244 206.278 356.657
0 35 387.667 696.8 220.632 331.835
0 40 None None None None
1 40 955.085 1367.88 448.838 913.231
1 45 875.818 1453.671 384.0 845.326
1 50 644.527 890.438 374.265 666.613
digests {'success': True, 'epochs_checked': 2, 'closed_epochs': {'0': '251FDC342BA5FFC23ED9B3F6E16DAE7CAEB4D1CF8E2684E263D313A578B8C4F5', '1': '53004F803B24FEF2723464BEF50C000F1B1FDB65E720144F144C6C52A96C6E95'}}

The bounded-recovery run passed on all six PRs with exactly two epochs in 103.877 seconds; successful data was deleted. Its full results are above. Nonfork finalization means were 189.310 ms (epoch 0) and 857.553 ms (epoch 1), so the objective was not met. Late epoch-0 windows also degraded as notarized forks accumulated. The next patch places notarized elections in the same bounded rotating recovery pool as frozen elections, instead of repeatedly soliciting every retained notarized fork from every PR. Local final generation remains on the normal immediate path. All 670 node unit tests passed.
b51260-r1026-terminated-recovery-01: success=True, wall=103.625s
| Epoch | Roots | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 36,903 | 98.112 / 247.838 / 1,379.490 | 36,770 | 116.765 / 278.391 / 2,474.647 |
| 0 | Fork | 1,880 | 260.610 / 504.334 / 7,708.199 | 3 | 558.662 / 765.407 / 765.407 |
| 1 | Nonfork | 12,036 | 515.566 / 930.668 / 1,647.730 | 11,968 | 587.732 / 1,012.078 / 2,730.357 |
| 1 | Fork | 2,508 | 17,575.820 / 41,710.449 / 43,288.126 | 10 | 963.622 / 2,037.279 / 2,037.279 |

Fresh nonfork windows: epoch, start, publication-final mean/p95, FIRST-final mean/p95
0 25 93.906 163.943 57.463 89.327
0 30 107.567 156.988 60.46 103.577
0 35 293.177 580.147 163.618 287.479
0 40 None None None None
1 40 666.6 1081.031 324.002 691.308
1 45 661.384 1004.44 287.922 570.48
1 50 368.97 533.093 198.453 339.393
digests {'success': True, 'epochs_checked': 2, 'closed_epochs': {'0': '0E8F608AEE85D82FFD5649A048701C0B6083CE6981EE8D47F6583BA1A640B04B', '1': '1C19438252C72F270C136E7772A9F4D2F6ABF4FB4CDEDB16B1BFEDC7CC02E13A'}}

The terminated-recovery run passed both epochs on all PRs in 103.625 seconds. Nonfork finalization improved to 116.765 ms versus 587.732 ms, still above the target. The full table is above and successful data was deleted.

The next patch incrementally maintains a separate compact hash table of certificate metadata under a short lock. Snapshot capture clones contiguous hash-table storage directly, avoiding repeated B-tree traversal over block payloads and rehashing every metadata key. Certificate insertion and earlier-epoch updates synchronously update both indexes before AEC can observe termination. Database scans and dependency closure remain outside the metadata lock. The existing snapshot regression now checks that neither index is pinned, that earlier certificates update metadata, that later replays never move an epoch forward, and that captured snapshots remain unchanged. All 170 ledger and 670 node tests passed.
