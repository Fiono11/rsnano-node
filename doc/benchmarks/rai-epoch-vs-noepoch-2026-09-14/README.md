# RAI at 2000 blocks/s: no epochs versus two epochs, 2026-09-14

Question: at 2000 blocks/s the second epoch of a 6-PR nanospam run finalizes non-fork blocks 5-9x
slower than the first. Is that a property of the protocol given the machine, or an engineering limit?

Answer in short: both, in layers. The host is over capacity at 2000 blocks/s in every configuration,
so a backlog forms and latency grows with time even without epochs. On top of that the node spends
extra CPU in epoch 1 (closing epoch 0 while epoch 1 runs, ledger reads inside the AEC write lock,
and a solicitation feedback loop that engages once latency passes one second), and at saturation
that extra cost is amplified by the queue into the 5-9x figure. Two of the costs found today were
pure waste and are fixed in this tree; the remaining ones are engineering items listed at the end.

## Setup

Host: 8-core Apple M1 (4 performance + 4 efficiency cores), macOS 14.6.1, six PRs plus nanospam on
one machine. Branch `rai_close_epochs` at `b424df1c2`, release build with `rai_protocol`.

```sh
nanospam --prs 6 --no-prio --fork-percentage 5 --blocks 50000 --accounts 50000 --rate 2000 \
  [--epoch-terminated-elections 25000 --closed-epochs 2 --close-timeout 240]
```

Fresh node data per run, deleted afterwards. PR0 stats counters, active election count and per-node
CPU were sampled every 2 s (`run_one.sh`); `sample` profiles of PR0 (4 s each) were taken 4, 16 and
23 s into the workload of a separate pair of runs (`prof_run.sh`, `profiles/`). Latencies are
publication to PR0 WebSocket outcome in milliseconds; windows are 5 s of publication time.

## Results

Non-fork finalization, mean / p95 ms. "Baseline" is the committed tree, "fixed" adds the two changes
described below. Windows are aligned on the start of publication; epoch 1 begins in the 10 s window.

| Run | 0 s | 5 s | 10 s | 15 s | 20 s | 25 s | overall |
|---|---:|---:|---:|---:|---:|---:|---:|
| baseline, no epochs | 203 / 387 | 155 / 320 | 142 / 235 | 468 / 669 | 612 / 912 | 650 / 935 | 332 |
| baseline, epochs (e0 then e1) | 457 / 945 | 326 / 719 | 267 / 469, e1 658 / 1034 | 1187 / 1512 | 2033 / 2698 | 2232 / 2705 | e0 360, e1 1557 |
| fixed, no epochs | 238 / 454 | 112 / 154 | 242 / 429 | 257 / 436 | 375 / 588 | 411 / 568 | 256 |
| fixed, epochs | 232 / 463 | 309 / 529 | 305 / 489, e1 919 / 1324 | 1336 / 1952 | 2209 / 3666 | 2188 / 3023 | e0 279, e1 1697 |

PR0 throughput while publishing was 1,600-2,000 elections confirmed per second against 2,000
started per second in every run; the six nodes together used 600-680% CPU (out of 800% nominal,
about 650% usable on 4P+4E cores). Full tables per run are in `tables.md`, raw records in `*.json`.

## Findings

### 1. Capacity: the no-epoch run degrades too

Without epochs the same workload goes from 142 ms (10 s window) to 650 ms (25 s window). Active
elections on PR0 grow from 700 to 2,500 during the run because ~1,900 elections start per second
and ~1,700 finish. That is a queue on a saturated host and has nothing to do with epochs: latency is
backlog divided by throughput, and the backlog grows for the whole run. At 1000 blocks/s the same
tree shows 98 ms in epoch 0 and 123 ms in epoch 1, so below the knee the epoch cost is a modest
constant; above the knee any extra per-block cost turns into unbounded growth.

Where PR0's demand goes (percent of one thread over a 4 s profile; "run" = running or runnable,
"lock" = waiting for a mutex, "sem" = waiting for an LMDB POSIX semaphore; baseline tree):

| Thread role | epochs, e0 | epochs, e1 | no epochs, 4 s | no epochs, 16 s |
|---|---|---|---|---|
| tokio runtime (10 threads: TCP, WebSocket, RPC) | run 233 | run 252 | run 221 | run 227 |
| Block processing (4) | run 114, sem 147 | run 152, sem 147 | run 78, sem 117 | run 118, sem 153 |
| Vote processing (4) | run 70, sem 15 | run 27 | run 72, sem 10 | run 56, sem 8 |
| Request aggregator (4) | run 2, sem 8 | run 43, lock 18, sem 23 | run 2, sem 3 | run 3, sem 5 |
| AEC voter | run 11 | run 36 | run 14 | run 30 |
| Priority scheduler + its activation queue + backlog scan | lock 96 | lock 181 | lock 51 | lock 136 |
| Voting, Voting final, Conf height (LMDB writers) | sem 41 | sem 69 | sem 41 | sem 73 |

The block processing threads wait about 1.5 thread-equivalents on the LMDB writer semaphore in all
runs; reads wait on the reader-table semaphore because the environment is opened with `NO_TLS`, so
every `begin_read` is a semaphore round trip on macOS (`sem_wait`). Both are per-block costs that
set the throughput ceiling; neither is epoch-specific.

### 2. Waste fixed today (both configurations)

**Priority scheduler spin.** `PriorityBuckets::should_schedule` reported a bucket as schedulable when
a queued block outranked the lowest election in that bucket, but under RAI `refill` never evicts, so
it skipped the bucket. The scheduler thread then woke, did nothing, and woke again: 2.39 million
loop iterations in the epoch run (430k without epochs), 100-150k per second whenever the AEC held
more than ~3,000 elections, holding the bucket mutex each time. The block processor's activation
queue and the backlog scan waited on that mutex 60-95% of the time, which is where the "publish to
first vote" component of latency came from. `check_vacancy` also ignored the global cap and the
cooldown that `refill` honours. Fixed in `bucket.rs` and `active_elections_container.rs`; the loop
count fell to 19-26k per run.

**Activation churn.** The backlog scan (10,000 accounts/s), the block-processed hook and the
confirmed-block hook re-activated every account whose frontier was not cemented yet. Under RAI the
election is still in the AEC (retired only at the epoch close) or already decided, so 282-301k of
the 271-400k activations per run were rejected as duplicates after a successor lookup, a block
read, a dependency check, a priority read and a bucket insert; 117k more were rejected as recently
confirmed in the epoch run (forked roots decided at the cut). `activate_with_info` now asks the AEC
first (`is_active_or_recently_confirmed`) and skips; rejected activations fell to under 1,500.

Effect: no epochs 332 -> 256 ms mean (25 s window 650 -> 411 ms); epoch 0 360 -> 279 ms. Epoch 1
did not move (1557 -> 1697 ms, within run-to-run variance), so the epoch-1 cost is elsewhere.

### 3. What epoch 1 pays that epoch 0 and the no-epoch run do not

Per-window PR0 counters at matched offsets, fixed tree, epoch 1 (17-29 s) versus no epochs:

| Counter, per second | epoch 1 | no epochs, same windows |
|---|---:|---:|
| Hashes solicited by peers (`request_aggregator/request_hashes`) | 2,200-3,200 | 1,300-2,100 |
| Hashes this node generated or replayed in replies | 2,100-2,600 | 1,200-1,700 |
| Elections confirmed | 1,270-1,870 | 1,560-2,000 |
| Active elections on PR0 | 2,900-4,300 | 2,100-3,000 |
| Epoch close messages received | 17-20 | 0 |

- **Closing epoch 0 while epoch 1 runs.** Epoch 0 reached its count 13 s into the workload and closed
  on all six PRs at 26 s, i.e. the close overlapped the whole of epoch 1. During that time the closer
  drains pending elections and solicits 255 targets every 2 s per node, the request aggregator
  answers recovery requests (43% of a thread plus 41% waiting on locks and LMDB, versus 2% in epoch
  0), the AEC keeps epoch 0's 25,000 elections until the close commits, and the close commit itself
  holds the AEC write lock while writing ~26,000 records. Epoch 0's elections also stay in the AEC
  voter's and closer's scans (AEC voter 11% -> 36%).
- **Ledger reads under the AEC write lock.** `ensure_not_recently_confirmed` calls
  `Ledger::canonical_confirmation_epoch` (an LMDB read, one or two per insert) while the AEC write
  lock is held. In epoch 1 the scheduler thread spent 14% of its time inside that call waiting for the
  LMDB reader semaphore and the vote processor another 7%, with every other AEC user queued behind
  the write lock. Without epochs the lookup is a cheap miss; with contended semaphores it is not.
- **Solicitation feedback loop.** The first confirm_req of an election is deferred by one base
  latency (1 s). Once epoch-1 latency passed 1 s, every election was solicited, each solicited hash
  is answered by every PR with all of its cached votes (about six votes, 870 bytes per hash, 1.3-2.4
  MB/s of vote replies per node), and the receivers verify those signatures. Solicited hashes rose
  from ~1,500/s in epoch 0 to 2,200-3,900/s in epoch 1. Slower elections cause more solicitation,
  which costs more CPU, which slows elections; this loop is what turns a moderate extra cost into
  the 5-9x figure.

None of these is required by the protocol: the RAI rules fix what a close must contain and that
recovery replies replay signed votes, not when a node solicits, how many ledger transactions an
insert takes, or that the closer's work competes with live elections on the same threads.

## Changes in this tree

- `node/src/consensus/election_schedulers/priority/bucket.rs`: under RAI a bucket is available only
  with vacancy (no eviction means no priority override).
- `node/src/consensus/active_elections/active_elections_container.rs`: `check_vacancy` returns false
  while cooling down or at the global election cap, matching `refill`.
- `node/src/consensus/active_elections/aec_service.rs`, `priority_scheduler.rs`: activations skip
  blocks that already have an election or a recent outcome (one AEC read lock instead of four ledger
  reads and a bucket insert).
- Unit tests: `full_bucket_with_better_priority_block`, `no_vacancy_at_global_cap`.

Validation: 690 RAI node unit tests and all default `cargo test --lib` suites pass.

## Follow-up the same day: the two caveats and the first two items

Requirement stated for this round: every election must terminate on every PR, by itself without
epochs, and by itself or through the close with epochs.

### One cause behind both caveats

nanospam sends each fork to every second node, so PRs 1, 3 and 5 only learn the other candidate from
peers. The RAI recovery reply republished the block of every block-tree entry of every requested
root (the comment above the code said "only the others"; the code did not filter). A 255-hash
confirm_req therefore produced 255 or more Publish messages into a per-channel send queue of 64
(`Channel::MAX_QUEUE_SIZE`), and the same tail was dropped on every retry: 420k to 1M
`drop/publish/out` per node and 360k to 900k replayed hashes for a 50k-block run.

- Without epochs the PR that had never received a fork block could not obtain it, so it kept one
  notarization certificate fewer than its peers on 8 to 24 forked roots. The block-tree diff across
  the six PRs (`confirmation_active` with `block_tree: true`) showed exactly that pattern: all
  differing roots were forks, the union had two certificates, one PR had one. Every election had
  terminated everywhere; the certificate sets did not agree. That is what nanospam's `failed_roots`
  reported.
- With epochs the same missing certificates left memberships 1 to 2 members apart after the drain.
  The stalled run sat at 25739/25740/25741 members for 240 s: a replica behind the highest count can
  only learn what it is missing from the candidate's page list, but a delta page needs a snapshot
  both sides hold and each side advertises only its own latest snapshot (`State::bases`), so neither
  side could reconstruct the other and the blind solicitation never delivered the missing block.

### Changes

- `vote_generators.rs`: the reply republishes only blocks the requester did not name.
- `confirmation_solicitor.rs`: under RAI a request names every candidate the election holds, so the
  reply knows what the requester lacks.
- `confirm_req_sender.rs`: the first request of a live election waits two base latencies (2 s);
  notarized or timed-out elections are re-solicited every 30 s instead of every 5 s. Fork termination
  moves from ~1.3 s to ~2.0 s, which the stated goals allow.
- `ledger.rs`, `epoch_closure.rs`, `consensus_epoch_store.rs`: the canonical memberships (block hash
  to the epoch of its first close) live in an in-memory map, loaded when the epoch configuration is
  read and maintained at every close, so `ensure_not_recently_confirmed` no longer opens an LMDB read
  transaction under the AEC write lock; before the first close the lookup returns immediately.
- Unit test `canonical_epochs_are_served_from_memory_after_loading`.

Validation: 174 ledger and 690 node unit tests with `rai_protocol`, all default `cargo test --lib`
suites pass.

### Results, 2000 blocks/s

Non-fork finalization mean ms (PR0 WebSocket), agreement check, PR0 close times in seconds after
publication start. Rounds are cumulative.

| Build | No epochs: mean | agreement | Epochs: e0 / e1 mean | e1 / e0 | exit | e0 drain -> closed | e1 drain -> closed |
|---|---:|---|---:|---:|---|---|---|
| baseline | 332 | 8 failed roots | 360 / 1557 | 4.3 | 0 | 15 -> 29 | 29 -> (after sampling) |
| + scheduler spin, activation churn | 256 | 22 failed roots | 279 / 1697 | 6.1 | 1 (e1 close not converged in 240 s) | 17 -> 32 | 32 -> never |
| + recovery reply filter, candidate naming, cadence | 275 | pass, 0 failed | 232 / 969 | 4.2 | 0 | 14 -> 22 | 29 -> 52 |
| + canonical epochs in memory, run 1 | 223 | pass, 0 failed | 452 / 707 | 1.6 | 0 | 16 -> 27 | 29 -> 33 |
| same build, run 2 | | | 196 / 980 | 5.0 | 0 | 15 -> 28 | 28 -> 50 |
| same build, run 3 | | | 187 / 857 | 4.6 | 0 | 15 -> 25 | 25 -> 50 |

Per-node traffic while publishing (fixed build, no epochs): publish drops 423k -> 37k, replayed
hashes 368k -> 23k, confirmation requests 71k -> 5k. In the epoch runs solicited hashes in epoch 1
fell from 2,200-3,900/s to 550-1,300/s. The remaining kind-7 (timeout, no block) entries differ by a
few roots between PRs; nanospam accepts that because a canonical outcome exists.

Epoch 1 is still about 4.5x epoch 0 at this rate (roughly 850 versus 190 ms) and the excess sits in
the 15 to 28 s windows, which is exactly when the epoch-0 close runs on every PR (drain at 15 s, close
at 25 to 28 s). Item 3 below is therefore the next lever. The profile taken on this build is in
`profiles/fix3-*` and summarised in the last section.

## Remaining engineering items, in order of expected effect on epoch-1 latency

1. ~~Break the solicitation loop~~ Done above: the reply filter removed most of the load; a
   load-adaptive first-request delay is still possible if needed.
2. ~~Take `canonical_confirmation_epoch` out of the AEC write lock~~ Done above.
3. Keep the close of epoch e from competing with epoch e+1: retire epoch e's elections from the
   voter and closer scans at the drain, rate-limit close solicitation when the AEC is above a
   threshold, and split the close commit so it does not hold the AEC write lock for ~26,000 writes.
4. LMDB transaction pressure (all configurations): fewer write transactions (final vote store and
   cementing compete with the block processor) and fewer `begin_read` calls per vote and per request;
   on macOS each is a POSIX semaphore syscall because of `NO_TLS`.
5. Close page recovery cannot bootstrap between two replicas whose snapshots never coincided
   (`State::bases` advertises one digest; deltas must be additive). It no longer matters once
   memberships converge, but a replica could advertise every snapshot it holds to make it robust.

Binary identities (final build): see `canonical-*.json`; the earlier rounds are `baseline-*.json`,
`fixed-*.json` and `recovery-*.json`.

## Profile of the final build (epoch run, PR0, 4 s samples)

Percent of one thread-window; "run" = running or runnable, "sem" = waiting on an LMDB POSIX
semaphore (`begin_read` / `begin_write`). Sample at 4 s is epoch 0; at 18 s epoch 1 is live and
epoch 0 is draining; at 24 s publication has just ended.

| Thread role | 4 s (e0) | 18 s (e1 + e0 drain) |
|---|---|---|
| Block processing (4) | run 68, sem 86 | run 123, sem 187 |
| tokio runtime (10) | run 210 | run 215 |
| Node event processor (WebSocket) | run 58, sem 7 | run 52, sem 25 |
| Vote processing (4) | run 50, sem 5 | run 51, sem 17 |
| AEC voter | run 18 | run 28 |
| Voting, Voting final, Conf height (LMDB writers) | sem 24 | sem 77 |
| Request aggregator (4) | sem 1 | run 2, sem 15 |
| Epoch close thread | 0 | run 5 |
| Priority scheduler, activation queue, backlog scan | lock 3 | lock 4, sem 17 |

After the recovery and canonical-epoch changes the closer itself costs about 5% of a thread and the
request aggregator 2%. What grows in epoch 1 is LMDB transaction waiting: block-processing threads
wait 1.9 thread-equivalents on the writer semaphore (0.9 in epoch 0), and the final-vote writer,
cementing, the WebSocket notifier and the aggregator all wait more on the reader semaphore. So the
remaining epoch-1 excess is item 4 (LMDB transaction pressure: the close commit's ~52k puts in one
write transaction, final-vote and cementing writes competing with the block processor, and one
`begin_read` per vote reply and per request on a `NO_TLS` environment) more than closer CPU. The
scheduler convoy of the baseline is gone (lock 3-4%).

Binary identities (final build): `rsnano` SHA-256
`aaca44e2b5dc9da1207f05f42973b56121ae63c0f251e513bb6e581d0996acf8`, `nanospam`
`7d89b3d18afee3412a01f36ce976403c3fc30da08b478479c1be77605191750c`.
