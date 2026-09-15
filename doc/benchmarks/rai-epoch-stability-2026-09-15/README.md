# RAI epoch stability, 2026-09-15

**The activation read precheck was benchmarked and excluded from the commit; the original stability goal remains unmet.** Four controlled runs used candidate/baseline/baseline/candidate order. Candidate epoch-1 mean latency was 633–691 ms versus 435–615 ms for the retained baseline; throughput ranges overlapped and phase-specific gains were inconsistent. All four runs closed both epochs across all six PRs and their generated data was deleted.

The retained baseline uses node SHA-256 `0fcc45714bdb879aa20fc8f7239a162e051778f5bfd9e5d069a3211af888d4c1` and timeline-enabled nanospam `5a476df0b3095fc8367624a1f55b751d56b94086087af31fc8b3b1a65ed1b3f8`. Its earlier `phases-01` capture separates overlap from recovery: fresh non-fork publication cohorts averaged 529 ms while the six PRs closed epoch 0 and 884 ms after all six finished cleanup. Only 1.662 seconds of publication remained afterward, so it cannot establish sustained throughput recovery. The retained source excludes the activation precheck and was validated again after removal.

The preceding atomic-count repeats (`counter-01` / `counter-02`) had epoch-1 mean / p95 ranging from 640 / 999 ms to 1,467 / 2,747 ms and late completion rates of 1,499.5–1,786.4/s. A same-binary no-epoch diagnostic had lower latency and stable completion throughput, supporting additional epoch-related delay beyond general queue growth. The repeats do not demonstrate an improvement over the preceding hinted-scheduler candidate.

The preceding verified hinted-scheduler run (`notified-01`) had non-fork mean / p95 latency of 346 / 660 ms in epoch 0 and 537 / 802 ms in epoch 1: a 1.55× mean ratio. Its late completion rate was 1,727.5/s, down 6.4%, while observed non-fork publication pace fell 9.1%; this run's receipt-rate decline cannot be attributed independently to closure capacity. Latency still increased. All three runs use the requested load, default 100 ms batching, and default workers.

The previous certificate/timer/validator repeats (`cert-01` and `notify-01`) had epoch-1 means of 655–784 ms and 2.58–3.35× epoch ratios. Despite its directory name, `notify-01` is a second certificate-build run: its recorded binary hash confirms it does not contain the hinted-scheduler fix. The verified hinted executable was used in `notified-01`; the counter candidate builds on it.

All 23 completed runs had generated ledger/account data deleted. The [final independent audit](final-cleanup-audit.json) verified every recorded data path absent; the final port check found no listeners on any of the 18 benchmark ports.

## Final activation evaluation: excluded

The four runs isolate the activation read precheck, share the same nanospam executable, and use the exact requested workload/default configuration. The candidate pair shares its node hash, as does the baseline pair. Candidate/baseline/baseline/candidate order reduces simple time-drift bias; the spread still shows host/run variability.

| Run, in execution order | Epoch 0 mean / p95, ms | Epoch 1 mean / p95, ms | Non-fork finalizations/s, 15–25 s |
|---|---:|---:|---:|
| activation-01 | 276 / 633 | 691 / 1,205 | 1,715.7 |
| phases-repeat-01 | 142 / 286 | 435 / 711 | 1,756.2 |
| phases-repeat-02 | 273 / 598 | 615 / 1,362 | 1,637.4 |
| activation-02 | 254 / 725 | 633 / 1,287 | 1,649.8 |

Phase latency below uses **fresh, non-fork epoch-1 publication cohorts**. Phase throughput counts finalization receipts only during the intersection of that phase and the publication span, excluding the quiet observation period.

| Run | During PR0 closing: mean / p95, ms | During PR0 closing: finalized/s | All-six cleanup complete, s | After all-six cleanup: mean / p95, ms | Post-cleanup publication duration / finalized per second |
|---|---:|---:|---:|---:|---:|
| activation-01 | 646 / 1,157 | 1,699.8 | 24.739527 | 804 / 1,075 | 1.566 s / 1,748.1 |
| phases-repeat-01 | 435 / 712 | 1,746.5 | 37.035843 | No publications | 0 s / unmeasurable |
| phases-repeat-02 | 532 / 1,287 | 1,650.8 | 25.128681 | 814 / 975 | 1.146 s / 1,837.2 |
| activation-02 | 506 / 1,002 | 1,723.5 | 27.331430 | No publications | 0 s / unmeasurable |

The first adjacent pair's closing mean worsened about 48% with activation; the second improved about 5%. Closing throughput also moved in opposite directions. After all-six cleanup, only one candidate and one baseline retain any publication, for different tails shorter than two seconds. Their receipt rates include earlier queued work and cannot establish a repeatable steady recovery benefit. Runs without post-cleanup publications cannot measure recovery capacity.

One PR's drain-start record is malformed or missing in each of `activation-01` and `phases-repeat-01`; the global closing start remains unknown for those runs. All six persisted-close and cleanup-complete markers are available, so their latest cleanup boundary and post-cleanup measurements remain usable. The local PR0 phase comparison is complete in every run. For the second pair, global closing mean was 632 ms with activation versus 543 ms without it.

The candidate avoided the write lock for 7,102 of 43,650 activation requests (16.27%) in run 1 and 5,050 of 45,383 (11.13%) in run 2. That confirms the precheck was exercised, but the measurements do not support retaining it. The final decision is to exclude the activation change. [Compact comparison](activation-comparison.json) retains exact values, publication rates, crossing counts, hashes, and marker limitations.

## Repeated interim results

PR0 non-fork finalization latency is mean / p95 in milliseconds. Each pair of writer, certificate, and counter runs shares binary SHA-256 hashes within that pair.

| Run | Epoch 0 latency | Epoch 1 latency | Epoch 1 / epoch 0 mean | Finalizations/s, 15–25 s | Both closes agree across six PRs |
|---|---:|---:|---:|---:|---|
| baseline-01 | 423 / 826 | 1,516 / 2,171 | 3.59× | 1,634.8 | No: epoch 1 timed out |
| writers-01 | 311 / 670 | 1,092 / 1,643 | 3.51× | 1,650.4 | Yes |
| writers-02 | 291 / 643 | 1,003 / 1,739 | 3.45× | 1,648.7 | Yes |
| cert-01 | 234 / 451 | 784 / 1,190 | 3.35× | 1,654.9 | Yes |
| notify-01 (certificate repeat) | 254 / 500 | 655 / 1,054 | 2.58× | 1,778.7 | Yes |
| notified-01 (verified hinted fix) | 346 / 660 | 537 / 802 | 1.55× | 1,727.5 | Yes |
| counter-01 | 373 / 722 | 1,467 / 2,747 | 3.94× | 1,499.5 | Yes |
| counter-02 | 210 / 474 | 640 / 999 | 3.04× | 1,786.4 | Yes |

Against the one baseline run, the writer repeats improved epoch-1 mean latency 28–34% and p95 20–24%; late throughput was about 1% higher. The repeats support these observed ranges, but do not isolate the contribution of each change.

### Completion throughput in full five-second windows

Offsets are relative to first publication. Counts combine all epochs and fresh/carried non-fork cohorts. The initial window includes startup, 10–15 s crosses the transition, and publication-tail/drain windows are excluded.

| Receipt window, s | Baseline finalized/s | writers-01 finalized/s | writers-02 finalized/s |
|---|---:|---:|---:|
| 0–5 | 1,580.8 | 1,620.4 | 1,628.8 |
| 5–10 | 1,811.4 | 1,816.8 | 1,967.8 |
| 10–15 | 1,552.2 | 1,757.8 | 1,524.0 |
| 15–20 | 1,535.0 | 1,562.4 | 1,712.0 |
| 20–25 | 1,734.6 | 1,738.4 | 1,585.4 |

PR0 observed epoch 0 closed at 26.8 and 24.0 s in the final runs, then epoch 1 closed at 37.0 and 38.3 s. These times use the shared schedule anchor and approximately two-second sampling. Baseline epoch 0 closed at 26.6 s; epoch 1 failed the 240 s verification timeout.

Six-node `ps` CPU averages were 614.5% early / 632.3% late in writers-01 and 620.5% / 622.2% in writers-02. This is substantial load on the 8-core Apple M1 host (four performance and four efficiency cores). These process averages are not instantaneous utilization and do not isolate closure cost.

## Changes retained

The benchmarked changes include:

- Staging epoch-close membership writes before taking the active-election write lock, with the finalization assertion retained before commit; cached membership bookkeeping.
- Reusing candidate-read snapshots and skipping WebSocket confirmation preparation when no client subscribes.
- Avoiding writer transactions for empty rollbacks and batches containing only rejected blocks.
- Reducing repeated timeout/candidate work in the active-election vote-target scan.
- Reporting throughput by outcome receipt time, separately from publication-based latency cohorts.
- Avoiding duplicate certificate publication and unused ledger payload caching; indexing RAI timers and skipping unnecessary validator reads.
- Removing hinted-scheduler notification reads that could not advance the worker, preserving shutdown wakeup; an atomic termination count and empty local-vote-history guard.
- Recording bounded publication/outcome timelines and separate close/cleanup markers with explicit PR-to-PID identities.

## Workload and measurement

Baseline node source: `2a99cbe07`. Requested-workload runs used six local PRs, RAI release binaries, no priority traffic, 50,000 blocks and accounts, 2,000 blocks/s, 5% requested forks, and epochs of 25,000 terminated elections. Separately labeled controls disable epochs:

```sh
nanospam --prs 6 --no-prio --blocks 50000 --accounts 50000 \
  --rate 2000 --fork-percentage 5 \
  --epoch-terminated-elections 25000 --closed-epochs 2 \
  --close-timeout 240 --data-dir <fresh-directory>
```

`--epoch-length` means seconds; count-based epochs require the flag above. All runs generated and published 50,000 blocks. Earlier comparisons use the same instrumented nanospam binary; `phases-01` adds the precise timeline export, with its distinct hash retained in metadata.

Latency is first publication to PR0 WebSocket election-outcome receipt. Completion throughput counts one receipt per root/outcome/epoch in fixed five-second windows. Publication-window counts describe latency cohorts, not completion throughput. The roughly 790 cps global benchmark value uses a 60-second observation denominator and is not the workload throughput reported here.

Only PR0 latency is measured; both close hashes are checked across all six PRs in the requested workload. Carried elections keep their original publication timestamps and have separate cohorts. The failed baseline also reported 46 non-fork finalizations in epoch 2, excluded from the epoch 0/1 latency table. The diagnostic control below helps distinguish general queue growth from additional epoch-related work, without isolating the cost of every closing operation.

### Epoch 1 during closure versus after closure

`phases-01` provides precise local and network-wide boundaries. Times below are seconds after first publication; the clock anchor interval is two microseconds wide, with an additional microsecond of offset rounding uncertainty.

| PR | Epoch 0 closing starts | Persisted close/AEC return | Cleanup complete |
|---|---:|---:|---:|
| 0 | 13.313365 | 23.598345 | 23.940086 |
| 1 | 13.295227 | 23.652644 | 24.699791 |
| 2 | 13.299125 | 23.600936 | 24.209706 |
| 3 | 13.325018 | 23.685756 | 24.577716 |
| 4 | 13.385513 | 23.653257 | 24.338299 |
| 5 | 13.364514 | 23.652438 | 24.588550 |

PR0's closing interval lasted 10.285 s and its remaining cleanup took 341.741 ms. Other PR cleanup continued for 759.705 ms after PR0 finished. The network boundary therefore uses the first drain, latest persisted close, and latest cleanup completion across all six PRs; outcomes are still measured at PR0.

Fresh, non-fork epoch-1 **publication cohorts**:

| Boundary scope / publication phase | Roots | Mean / p95 finalization latency, ms |
|---|---:|---:|
| PR0: during closing | 18,494 | 524 / 1,212 |
| PR0: post-persistence cleanup | 604 | 1,016 / 1,164 |
| PR0: after cleanup complete | 4,290 | 893 / 1,176 |
| All six: during closing | 18,646 | 529 / 1,210 |
| All six: post-persistence cleanup | 1,642 | 934 / 1,159 |
| All six: after cleanup complete | 3,100 | 884 / 1,176 |

Publication ended at 26.361649 s: only 2.422 s after PR0 cleanup and 1.662 s after all-six cleanup. Of 6,020 fresh receipts after PR0 cleanup, 1,730 came from earlier publications; after all-six cleanup, 2,558 of 5,658 receipts came from earlier publications. All carried non-fork outcomes finished before these cleanup boundaries. Mixing that queued work into newly published recovery cohorts would change the question being measured.

Only the 25–26 s window is both a full second after all-six cleanup and entirely inside the publication span. It had 1,959 non-fork publications and 1,636 fresh epoch-1 finalizations, with receipt mean / p95 894 / 1,158 ms. The 26 s publication tail lasted only 0.362 s and its cohort mean declined to 617 ms as publication ended. These observations show some decline from the close-time spike, but provide no full five-second post-close publication window or sustained recovered-capacity measurement. Whole-phase rates include approximately 36 s of quiet drain/observation time and must not be used as recovered throughput.

The complete [phase report](phases-01/phase-analysis.json) retains receipt cohorts, crossings, one-/five-second windows, uncertainty, and both boundary scopes. The [compressed original results](phases-01/results.json.gz), [markers](phases-01/phase-markers.json), and [metadata](phases-01/metadata.json) support replay.

#### Earlier approximate evidence

Legacy `EPOCH_CLOSED` stderr records have neither a timestamp nor a PID, so merged node logs cannot identify an exact PR0 close instant. RPC samples bracket when PR0's persisted close becomes visible, not when all generator/state cleanup finishes. Sample timestamps precede the RPC request; the bounds below use the last open sample and the next sample after the first closed observation minus the runner's two-second sleep. The first-publication origin is also only bounded between `Starting with` and the first positive confirmation log, approximately 0–1 s after the schedule anchor.

| Run | PR0 epoch-0 persisted-close bracket, s after schedule | Available epoch-1 phase evidence |
|---|---:|---|
| cert-01 | 36.307–38.373 | All publication and receipt bins precede close; no recovery workload observed. |
| counter-02 | 35.287–37.370 | All publication and receipt bins precede close; no recovery workload observed. |
| notified-01 | 24.962–27.206 | 15–20 s is before close; 20–25 / 25–30 s cannot be split reliably. |
| counter-01 | 23.164–25.379 | 15–20 s is before close; 20–25 / 25–30 s cannot be split reliably. |
| writers-02 | 21.584–24.085 | 25–30 s is definitely after close, but contains only the publication tail. |

For fresh, non-fork epoch-1 publications in `writers-02`, the clearly pre-close 15–20 s cohort had mean / p95 755 / 1,002 ms; the clearly post-close 25–30 s tail had 1,069 / 1,467 ms over 2,670 roots. Post-close receipt latency was 1,245 / 1,660 ms over 5,373 outcomes. This short tail does not demonstrate recovery to the earlier latency. Dividing the drain count by five seconds would not measure sustained recovered capacity. The carried non-fork outcomes in these captures fall before close and remain separate from the fresh cohorts.

The [phase analyzer](phase_analysis.py) requires timeline schema 1 with all published roots, per-epoch outcome offsets, a bounded wall-clock anchor, an observation cutoff, and explicit `nodes_by_pr` PID metadata. It joins `EPOCH_COUNT_REACHED`, `EPOCH_CLOSED` (AEC close return after persistence/discard), and `EPOCH_CLOSE_COMPLETE` (remaining cleanup complete) markers. It preserves PR0-local analysis and adds six-PR boundaries only with complete explicit marker coverage; missing coverage is unknown. It classifies these phases separately:

- **During closing:** count threshold reached until persisted close returns.
- **Post-persistence cleanup:** persisted close return until cleanup completes.
- **After complete:** cleanup completion until the observation cutoff.

Publication cohorts show latency of work published in each phase; receipt cohorts show when finalizations arrive. A crossing table isolates earlier publications finalized later, and carried elections are reported separately. Timestamp intervals overlapping a boundary remain uncertain. Full one- and five-second windows use the first-publication origin and identify whether publication was still active. Whole-phase rates use the actual observed phase duration, including quiet drain time, and are explicitly not steady capacity estimates. The report also gives the duration from cleanup completion to the last publication; zero or a short tail cannot establish sustained recovery under the exact 50,000-block workload.

Replay the retained timeline-enabled run:

```sh
python3 doc/benchmarks/rai-epoch-stability-2026-09-15/phase_analysis.py phases-01 \
  --output /tmp/rai-phase-analysis.json
```

Legacy captures remain unavailable to this exact analyzer; their approximate brackets above are retained. Seven semantic tests cover crossing work, carried roots, duplicate observations, uncertain boundaries, marker identity, empty post-close publication tails, and local versus six-PR recovery.

### Publication-rate confound

The requested 2,000/s does not guarantee a constant observed publication pace on this shared host. Reconstruct publication timing by summing `publication_windows.termination.count` across epochs only where `prior_epoch_outcome=false`; using termination counts includes first outcomes that did not finalize. This counts each observed root once. Summing finalization counts or retaining carried groups would distort the publication estimate. In the seven captures below, observed roots across both fork classes total exactly 50,000, matching `published_blocks`; no aggregate publication coverage gap is visible. This method cannot recover missing-outcome roots in general.

| Run | Non-fork publications/s, early → late | Publication decline | Finalizations/s, early → late | Receipt decline |
|---|---:|---:|---:|---:|
| cert-01 | 1,860.2 → 1,824.1 | 1.9% | 1,962.8 → 1,654.9 | 15.7% |
| notify-01 (certificate repeat) | 1,788.2 → 1,801.4 | −0.7% | 1,915.4 → 1,778.7 | 7.1% |
| writers-noepoch-control | 1,845.0 → 1,795.6 | 2.7% | 1,806.8 → 1,785.6 | 1.2% |
| notified-01 (verified hinted fix) | 1,955.4 → 1,777.4 | 9.1% | 1,845.2 → 1,727.5 | 6.4% |
| counter-01 | 1,955.8 → 1,811.9 | 7.4% | 2,057.2 → 1,499.5 | 27.1% |
| counter-02 | 1,789.8 → 1,803.6 | −0.8% | 1,844.0 → 1,786.4 | 3.1% |
| counter-noepoch-control | 1,782.8 → 1,805.9 | −1.3% | 1,779.8 → 1,788.2 | −0.5% |

Early is 5–10 s and late is 15–25 s after first publication. Lower publication pace does not explain most of the certificate repeats' completion decline. In `notified-01`, publication pace falls more than receipt pace, so the throughput comparison is confounded by offered-rate variation. Its publication-cohort finalization means still rise from 395 ms at 5–10 s to 526 / 584 ms at 15–20 / 20–25 s; the certificate repeats rise from 234 / 243 ms early to 761–894 / 553–709 ms late. Latency growth therefore remains independently visible in the cohorts.

These are descriptive comparisons: receipt windows include work published earlier, and the publication timestamp precedes serialization/TCP writes rather than peer receipt. The captures do not establish whether publisher variation comes from host saturation, generation, or transport scheduling, or isolate consensus delay from transport/WebSocket delivery. [Full five-second bins and coverage checks](publication-rate-check.json) retain the diagnostic evidence.

## Follow-up: which work grows in epoch 1?

The **no-epoch diagnostic control** (`writers-noepoch-control`) is outside the requested workload: it uses the same writer-build binary as writers-01/02 and load but disables epoch advancement/closure. It is a diagnostic comparison, not a replacement for the count-25,000 epoch runs. It exited successfully and its generated data was deleted. Non-fork mean / p95 over the control was 251 / 483 ms; publication-cohort means at 5, 15, 20, and 25 s were 154, 288, 358, and 414 ms. General latency growth therefore remains even without closing epochs. Its completion rate stayed closer to the offered load: 1,806.8/s early and 1,785.6/s late, compared with approximately 1,649/s late in those writer-build epoch runs.

PR0 cumulative counters identify additional recovery work during the overlap. Values are approximate events/s in 5–10 s → 15–25 s; endpoints are interpolated between roughly two-second samples. Epoch runs use the shared schedule anchor; the control uses its `Starting with` log timestamp.

| Counter | writers-01 early → late | writers-02 early → late | No-epoch control early → late |
|---|---:|---:|---:|
| Elections started | 1,997 → 1,845 | 1,907 → 1,819 | 1,936 → 1,894 |
| Incoming requested hashes | 503 → 1,412 | 422 → 1,091 | 335 → 301 |
| Hashes submitted to reply voting | 355 → 1,102 | 291 → 740 | 263 → 263 |
| Incoming confirm-request packets | 20 → 33 | 17 → 26 | 17 → 16 |
| Incoming confirm-ack packets | 435 → 597 | 384 → 492 | 395 → 388 |
| Vote packets accepted into processing queue | 1,339 → 1,199 | 1,255 → 1,154 | 944 → 1,011 |
| Locally emitted vote batches | 104 → 136 | 85 → 107 | 84 → 84 |
| Dropped outbound publishes | 0 → 194 | 0 → 70 | 0 → 0 |
| Dropped outbound confirm-acks | 0 → 196 | 0 → 108 | 0 → 0 |
| Cementer loop iterations | 134 → 32 | 201 → 45 | 197 → 95 |

The epoch runs accept fewer new elections while recovery request hashes increase 2.5–3.1× and outbound drops appear. Total block-processing volume stays similar to the control. This supports recovery traffic/work competing with live elections. It does **not** show a general increase in all signature-verification volume: total accepted vote packets and applied vote hashes decrease in the epoch runs, even as incoming confirm-ack packets increase. Local vote-batch emissions grow 26–32% in the epoch runs and stay flat in the control; this sum of broadcast/reply counters points to more local signing/batching work.

The captures have no direct writer-transaction count, writer-wait counter, or signature-verification duration. `requests_generated_hashes` counts hashes submitted to reply voting before authorization, not hashes actually signed; `vote_processor/process` counts queue acceptance, not completed verification. Cementer loop rates fall more than cemented-block rates, indicating larger batches; this alone does not measure writer contention. Full counter definitions, rates, and per-election normalization are retained in [counter-phases.json](counter-phases.json). The diagnostic profile is retained alongside this evidence.

### Writer-build profile: membership bookkeeping hotspot

`writers-current-profile` is diagnostic and used the same node binary as the two interim runs. It exited 1 because PR4 was still draining epoch 1 at the 240-second verification timeout; the other five PRs had closed both epochs. PR0 had observed its own second close by 38.6 s, illustrating why PR0 observations alone cannot establish six-PR convergence. Generated data was deleted. PR0 non-fork mean / p95 was 520 / 1,282 ms in epoch 0 and 1,233 / 1,873 ms in epoch 1.

Disassembly maps the hot `ActiveElectionsContainer::apply_vote` offsets `+22320` / `+22400` to 40-byte `(epoch, block hash)` BTreeSet lookup/insertion in `Ledger::record_epoch_block`'s `epoch_candidates`. One vote-processing thread had **193 of 716 samples (27%)** directly in that block. This is one thread/sample interval, not a whole-node CPU percentage. The nominal 24 s sample actually began **26.685 s after the schedule anchor**, during the workload tail, so it should not be treated as an early epoch-1 sample.

The newer certificate candidate avoids repeated certificate-membership publication, removes an unused ledger block-payload cache, indexes RAI timer deadlines, and skips validator reads only where existing validation conditions do not need them. These changes have passed the suites below and a release build. The [diagnostic summary](writers-current-profile/diagnostic-summary.json) retains the finding and actual capture offsets; compressed profiles are linked there and below.

### Certificate/timer/validator candidate

`cert-01` and `notify-01` use the certificate candidate, retaining the previous writer, staged-close, WebSocket, and measurement changes. They use the requested six PRs, 50,000 blocks/accounts, rate 2,000, 5% forks, epochs of 25,000 terminated elections, no priority traffic, and default batching/workers. The older writer runs remain interim comparison evidence.

| Run | Epoch 0 mean / p95, ms | Epoch 1 mean / p95, ms | Finalizations/s, 15–25 s | Both closes agree | Generated data deleted |
|---|---:|---:|---:|---|---|
| cert-01 | 234 / 451 | 784 / 1,190 | 1,654.9 | Yes | Yes |
| notify-01 (certificate repeat) | 254 / 500 | 655 / 1,054 | 1,778.7 | Yes | Yes |

The intended build before `notify-01` targeted the daemon library instead of the CLI executable. Its recorded node SHA-256 is `d0a6dd66080cfc99a7faf4b82f4943a8f0e1c72ec90a0708f83b34debe3c74d4`, identical to `cert-01`; its nanospam hash also matches. The original directory and metadata are preserved. The result is therefore certificate-build repeat evidence, with no hinted-scheduler attribution.

Against the averages of the two writer runs, cert-01 reduces epoch-1 mean latency by about 25% and p95 by about 30%, while late throughput is essentially unchanged. PR0 first observed epoch 0 closed with epoch 1 draining at 38.34 s, and both epochs closed at 42.40 s. In the repeat those observations were 26.92 s and 49.26 s, respectively. Six-node CPU sample means were 595.9% early / 625.2% late in cert-01 and 596.0% / 620.6% in the repeat. The repeat's early throughput was 1,915.4/s, falling 7.1% to 1,778.7/s late; epoch-1 latency remains higher in both runs.

Its [counter phases](cert-01/counter-phases.json) still show recovery growth: incoming requested hashes rise 389 → 1,232/s and hashes submitted to reply voting rise 293 → 885/s. Accepted vote-processing queue entries rise 1,041 → 1,534/s (+47%) and local emitted batches 78 → 127/s (+63%), while applied vote hashes fall 18,445 → 17,547/s. In this candidate there are more vote-processing units per applied hash late in the workload. Late publish/ack drops remain approximately 174 / 121 per second. The additional signing, queue, and recovery work was not isolated further.

### Hinted-scheduler notifications: first verified run

Profile analysis identified 459 of 716 samples waiting in the election-ended fact path that calls `HintedScheduler::notify`, which previously acquired two active-election reads to check vacancy. This is a sampled thread interval, not a whole-node CPU share. The scheduler's `wait_timeout_while` predicate only checks whether it has stopped, so ordinary notifications do not make it proceed before its periodic timeout. The candidate removes those ordinary notification reads/wakes while preserving prompt shutdown. Node suites passed, and `notified-01` used the corrected CLI executable with node SHA-256 `c6a15d28b970f9d08b53278f1f39d8095d3f790e95fea1675578449630c87a35`; `notify-01` did not exercise this change.

The verified run's epoch-1 mean / p95 was 537 / 802 ms, lower than both certificate repeats, while epoch-0 mean / p95 increased to 346 / 660 ms. The improved epoch ratio therefore reflects both changes. Late throughput of 1,727.5/s lies inside the certificate repeat range and needs the publication-rate qualification above. PR0 observed the two closes at 27.11 / 37.33 s; six-node CPU sample means were 551.2% early / 597.0% late. Both close hashes agreed across all six PRs, exit status was zero, and the generated data path was independently verified absent. This is one run, not a repeatability result.

### Atomic termination count: variable repeated results

`counter-01` and `counter-02` add an atomic termination count on top of the notified candidate. Both used node SHA-256 `17b310ddba2ec011509dac58d53f874220c91e9fd59bcb55b59df2130513c215`, the same nanospam binary as previous runs, and the requested workload/default batching and workers. Both exited zero, agreed on both close hashes across all six PRs, and had their generated data independently verified absent.

The first run had epoch-0 mean / p95 of 373 / 722 ms and epoch-1 values of 1,467 / 2,747 ms; the repeat had 210 / 474 ms and 640 / 999 ms. Receipt rates fell from 2,057.2 to 1,499.5/s (27.1%) in the first run and from 1,844.0 to 1,786.4/s (3.1%) in the repeat. This large spread, including a first-run epoch-1 p95 above baseline, does not demonstrate a performance improvement over `notified-01`. No stronger improvement claim is supported by these repeats.

PR0 observed both closes by 37.55 s and 37.32 s, respectively. Six-node CPU sample means were 608.1% early / 618.8% late in the first run and 596.2% / 633.6% in the repeat. The first run's 27.1% receipt-rate decline substantially exceeded its 7.4% publication decline; the repeat's publication pace rose slightly. Publisher variation therefore does not explain the full counter-build latency/throughput spread.

`counter-noepoch-control` is an **off-request diagnostic** using this same binary with epoch advancement disabled. It exited zero, its generated data was independently verified absent, and two-close verification is inapplicable. Overall non-fork mean / p95 was 173 / 327 ms. Completion rates stayed at 1,779.8/s early and 1,788.2/s late while publication pace was 1,782.8/s and 1,805.9/s. Its publication-cohort means at 0 / 5 / 10 / 15 / 20 / 25 s were 182 / 115 / 119 / 200 / 236 / 214 ms; CPU sample means were 581.3% early / 603.7% late.

The control still has some general latency growth, but at similar publication rates its late cohort means of 200–236 ms are well below counter-02's 645–698 ms (and counter-01's 1,241–1,864 ms). This supports additional work associated with epochs beyond general host/queue growth. One control and two variable epoch runs do not isolate individual close operations or establish their causal cost.

## Other experiments

- **200 ms vote batching:** same final binary; both closes passed and data was deleted. Epoch 0 mean / p95 was 160 / 278 ms and epoch 1 was 1,000 / 2,129 ms. Late throughput improved to 1,713.9/s, about 4% over the default repeats, but epoch-1 p95 was about 26% worse than their average and the mean epoch ratio increased to 6.24×. This configuration is not recommended for the stated stability goal; default 100 ms batching is retained.
- **One block-processing worker:** rejected. Epoch-1 mean / p95 reached 5,825 / 13,569 ms, late throughput fell to 1,094.7/s, and epoch 1 did not close. Data was deleted.
- **One vote-processing worker (`cert-one-voter`):** rejected. The same certificate candidate binary with `[node.vote_processor] threads = 1` closed both epochs on all six PRs, but epoch 0 mean / p95 reached 3,286 / 7,530 ms and epoch 1 reached 10,259 / 18,735 ms. Late throughput fell to 731.5/s. Metadata records the override, the effective node configuration is retained, and deletion of the generated data was independently verified.
- **Earlier low-risk candidate:** both closes passed, but epoch-1 mean / p95 regressed to 2,185 / 3,163 ms. Its smaller epoch ratio reflected a slower epoch 0.
- **Staged profile run:** diagnostic only. Writer-wait shares rose from early to later samples: block writer 21% → 64%, final-vote writer 34% → 42%, cementer 35% → 65%. No sample captured the close commit, so these profiles identify contention without measuring that commit's contribution. Profiling adds load.
- **Counter-build profile (`counter-current-profile`):** diagnostic only. It exited successfully, both close hashes agreed, and generated data deletion was independently verified. Its profiles are retained for investigation; profiling changes timing, so this run does not establish an improvement.

## Validation and reproduction

The final retained source passed 710 RAI and 605 default node tests; nanospam passed 40 tests with one ignored. The RAI suite was rerun after excluding the activation precheck. Earlier ledger checks passed 187 RAI and 172 default tests; WebSocket checks passed three library tests and one confirmation integration test. The phase analyzer passed seven semantic tests. Formatting, script parse, and whitespace checks passed.

Run from the repository root. Allow IPv6 local TCP listen/connect for PR indices 0–5: peering `17075 + 10*i`, RPC `17076 + 10*i`, WebSocket `17078 + 10*i`.

```sh
cargo build --offline --release -p rsnano_cli -p nanospam --features rai_protocol

python3 doc/benchmarks/rai-epoch-stability-2026-09-15/run.py repeat-01 \
  --bin-dir target/release --output-dir target/nanospam-stability-20260915

python3 doc/benchmarks/rai-epoch-stability-2026-09-15/analyze.py \
  --input-dir target/nanospam-stability-20260915 repeat-01
```

The runner stops its process group and deletes generated data in cleanup. Use a fresh name each time. Its default output directory is `target/nanospam-stability-20260915` below the invocation's working directory, independent of script location. `--delay 200` reproduces the delay experiment; `--block-processor-threads 1` and `--vote-processor-threads 1` reproduce the respective rejected worker configurations; `--profile` uses macOS `sample`. `--no-epochs` is only for the off-request diagnostic control; normal runs still use epochs of 25,000 terminated elections. The [counter helper](counter_phases.py) reads raw run logs/samples through `--input-dir` and estimates early/late rates; derived values are retained for the archived runs.

Inspect the retained comparison without running nodes:

```sh
python3 doc/benchmarks/rai-epoch-stability-2026-09-15/analyze.py \
  --compare baseline-01 writers-01 writers-02 writers-delay200-01
```

## Evidence

Each retained run has outcome records, command/binary metadata, and derived CPU/epoch-state observations in `summary.json`. Large logs and generated ledgers are excluded from this packet. Metadata paths identify the deleted run directories.

- Baseline: [results](baseline-01/results.json), [metadata](baseline-01/metadata.json), [summary](baseline-01/summary.json).
- Final run 1: [results](writers-01/results.json), [metadata](writers-01/metadata.json), [summary](writers-01/summary.json).
- Final run 2: [results](writers-02/results.json), [metadata](writers-02/metadata.json), [summary](writers-02/summary.json).
- Delay 200 ms: [results](writers-delay200-01/results.json), [metadata](writers-delay200-01/metadata.json), [summary](writers-delay200-01/summary.json).
- No-epoch diagnostic: [results](writers-noepoch-control/results.json), [metadata](writers-noepoch-control/metadata.json), [summary](writers-noepoch-control/summary.json).
- Certificate candidate: [results](cert-01/results.json), [metadata](cert-01/metadata.json), [summary](cert-01/summary.json), [counter phases](cert-01/counter-phases.json).
- Certificate repeat (original directory name `notify-01`; identical binary hashes): [results](notify-01/results.json), [metadata](notify-01/metadata.json), [summary](notify-01/summary.json).
- Verified hinted-scheduler candidate: [results](notified-01/results.json), [metadata](notified-01/metadata.json), [summary](notified-01/summary.json).
- Atomic-count run 1: [results](counter-01/results.json), [metadata](counter-01/metadata.json), [summary](counter-01/summary.json).
- Atomic-count run 2: [results](counter-02/results.json), [metadata](counter-02/metadata.json), [summary](counter-02/summary.json).
- Same-counter-binary no-epoch diagnostic: [results](counter-noepoch-control/results.json), [metadata](counter-noepoch-control/metadata.json), [summary](counter-noepoch-control/summary.json).
- Final measured timeline run: [compressed results](phases-01/results.json.gz), [metadata](phases-01/metadata.json), [summary](phases-01/summary.json), [phase markers](phases-01/phase-markers.json), [local and six-PR phase analysis](phases-01/phase-analysis.json).
- Counter-build diagnostic profile: [results](counter-current-profile/results.json), [metadata](counter-current-profile/metadata.json), [summary](counter-current-profile/summary.json), profiles labelled [7 s](counter-current-profile/pr0-7s.sample.txt.gz), [18 s](counter-current-profile/pr0-18s.sample.txt.gz), [24 s](counter-current-profile/pr0-24s.sample.txt.gz).
- [Publication-rate diagnostic](publication-rate-check.json), including full five-second bins and observed-root coverage.
- Rejected one-vote-worker configuration: [results](cert-one-voter/results.json), [metadata](cert-one-voter/metadata.json), [summary](cert-one-voter/summary.json), [effective configuration](cert-one-voter/config-node.toml).
- Current-build profile: [results](writers-current-profile/results.json), [metadata](writers-current-profile/metadata.json), [derived summary](writers-current-profile/summary.json), [last six-PR close observation](writers-current-profile/last-close-observation.json), [diagnostic finding](writers-current-profile/diagnostic-summary.json), profiles labelled [7 s](writers-current-profile/pr0-7s.sample.txt.gz), [18 s](writers-current-profile/pr0-18s.sample.txt.gz), and [24 s](writers-current-profile/pr0-24s.sample.txt.gz).
- [Exploratory summaries](exploratory-summary.json), [intermediate default results](final-default-01/results.json), [runner](run.py), [analyzer](analyze.py).
- Exact recovery-phase [analyzer](phase_analysis.py) and [semantic tests](test_phase_analysis.py).
- Final activation decision: [comparison](activation-comparison.json), [cleanup audit](final-cleanup-audit.json).
- Activation run 1: [compressed results](activation-01/results.json.gz), [compressed phase analysis](activation-01/phase-analysis.json.gz), [metadata](activation-01/metadata.json), [markers](activation-01/phase-markers.json), [summary](activation-01/summary.json).
- Controlled baseline 1: [compressed results](phases-repeat-01/results.json.gz), [compressed phase analysis](phases-repeat-01/phase-analysis.json.gz), [metadata](phases-repeat-01/metadata.json), [markers](phases-repeat-01/phase-markers.json), [summary](phases-repeat-01/summary.json).
- Controlled baseline 2: [compressed results](phases-repeat-02/results.json.gz), [compressed phase analysis](phases-repeat-02/phase-analysis.json.gz), [metadata](phases-repeat-02/metadata.json), [markers](phases-repeat-02/phase-markers.json), [summary](phases-repeat-02/summary.json).
- Activation run 2: [compressed results](activation-02/results.json.gz), [compressed phase analysis](activation-02/phase-analysis.json.gz), [metadata](activation-02/metadata.json), [markers](activation-02/phase-markers.json), [summary](activation-02/summary.json).
