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

## Attempt 3: paper-model fork diagnostic, recheck fixed (`no-crash-fork-paper-v3`)

Candidate `bin/no-crash-v3` (recheck fix), same workload and switches,
156 s ceiling (163 s wall). Not a performance run.

| Observation | Result |
|---|---:|
| Input complete | yes |
| Fork pairs published / branch hashes | 2,257 / 4,514 |
| Epochs closed on all six nodes | 0 (round 0) and 1 (round 6) |
| Epoch 2 | not closed anywhere: `usable` 1–2 of 6 reports on every node |
| Evidence checks with missing entries | 0 of 40 (every reconstructed report fully justified) |
| Sketch replies | 184: 171 incomplete at up to 1,024 cells, 13 partial pages, none completed a state |
| Root-based reconstructions | 159 reconciled events, 206 refusals (143 unknown target, 63 unknown source) |
| Branches terminal on all six nodes | 866 (included Finalized 559, Notarized 28, Recovery 4,679 node-observations; 559 discarded by certificate-final witness) |
| Unresolved by the observer | 3,648 |
| Derived-final entries, checkpoint conflicts | 0, 0 |
| Conditional termination upper bounds p50/p95/p99 (866 branches) | 19,788 / 22,467 / 22,795 ms |

The evidence path is now clean; the blocker is T reconstruction alone.
Each node sketched its *live* projection, which by the time the sketch is
sent has moved past a reporter's frozen snapshot by thousands of entries
(finalizations and N→F upgrades after the boundary), beyond what a
1,024-cell sketch peels; root-based requests find a shared root only
rarely for the same reason. Epoch 1 closed only after six timed-out rounds.
Fixed after this run: the sketch describes the node's own frozen snapshot
of the same epoch, taken at the same boundary as the reporter's, whose
difference from it is what the two nodes saw in between.
The controller reported a process-group cleanup permission error; the
nodes had exited and the data was removed by hand after verification.

## Single-epoch 5 % fork control, no checkpoint (`no-crash-forks5-noepoch-v1`, `-candidate`)

Requested as "forks 5 with a single epoch". With `--epoch-ms 0` every node
stays in epoch 0 for the whole run: no boundary, no report, no checkpoint.
Shared client `bin/no-crash-v4/nanospam`, 2,000 blocks/s, 45,000 primaries,
156 s calibration ceiling (no matching reference existed).

| | Baseline (frozen HEAD) | Candidate (`bin/no-crash-v4`) |
|---|---:|---:|
| Fork pairs published | 2,244 | 2,25x (see manifest) |
| Confirmed at the ceiling | 42,800 / 45,000 | 42,737 / 45,000 |
| Cemented on every node (identical hash) | 42,865 | 42,802 |
| Positions settled empty (split first votes) | 2,158–2,200 | ≈2,247 |
| Goodput, p50/p95/p99 | not emitted (timed out) | not emitted (timed out) |
| Verdict | BASELINE_CALIBRATION_FAILED | CANDIDATE_ONLY, incomplete |

Both binaries leave the split positions unresolved: single-support voting
gives each validator one first vote, a split never reaches a notarization
certificate, and without a boundary there is no checkpoint lock for the
owner's fresh child. This is the account path as specified, not a defect
of either binary, and it is the control for the one-checkpoint run below.
The candidate attempt was run alone after the baseline calibration failed;
it is a record, not a comparison.

## Single-epoch 5 % forks with one checkpoint (`no-crash-forks5-one-checkpoint-v1`)

"One checkpoint resolves the forks": epoch 0 ends by count
(`--epoch-terminated-elections 40000`), closes once, installs its locks,
the client extends them with fresh children; epoch 1 never reaches its
count within the run. Same client and 156 s ceiling; the candidate runs
the paper's model (equal weight, `f = p = 1`, certificate-only).

**Baseline attempt** (frozen HEAD): 2,271 fork pairs; input complete;
epoch 0 closed (round 1) and installed 40,227 finalized positions on five
nodes; PR4 never closed it (`usable=1` of 6 held reports, 204 unknown-source
refusals: the baseline's root-only reconstruction). Confirmed 44,608 / 45,000
at the ceiling, then no progress; the five installed nodes ended settled
with an identical hash, 44,673 cemented, 134 single-notarized and 258
empty positions; PR4 had 40,154 cemented and 4,701 single-notarized.
Verdict BASELINE_CALIBRATION_FAILED; no histogram. The candidate was then
run alone under the same ceiling (below).

**Candidate attempt** (`bin/no-crash-v4`, run alone, `-candidate`): 2,261
fork pairs; input complete; all six nodes closed epoch 0 (round 2) and
installed the identical checkpoint: 38,220 certificate-backed positions,
0 derived, 0 conflicts, 19 evidence checks with nothing missing, 10 refusals,
no sketch needed. Confirmed 42,948 / 45,000 at the ceiling. The checkpoint
retained 1,820 positions as locks (blocks in flight at the count-based
boundary with first votes but no certificate; the frozen baseline finalizes
such sole survivors, the certificate-only rule keeps them as locks). The
client published fresh children for them; about 1,815 children ended
unchecked on PR0/PR2/PR4 (parent not in the ledger) and as pending
elections on PR1/PR3/PR5, so they never reached a quorum. Every node held
43,014 cemented blocks with the same hash and 233 empty positions.
Verdict CANDIDATE_ONLY, incomplete; no histogram. The recovery step is the
remaining blocker; a focused diagnostic follows.

## One-checkpoint fork diagnostics (`no-crash-forks5-one-checkpoint-diag-v1..`)

The same count-based one-checkpoint workload under the fork-termination
observer (checkpoint inclusion or certificate-backed discard on all six
nodes), candidate only, 156 s ceiling.

| Attempt | Binary | Checkpoint 0 installed | Terminal branches / total | Dispositions (node-observations) | Notes |
|---|---|---|---:|---|---|
| diag-v1 | v4 | PR1–PR5; PR0 never closed (froze 35,258 vs 38,852 entries; sketches stuck at 1,024 cells) | 0 / 4,432 | Recovery 6,685, Finalized 1,685, discarded-by-certificate 1,685, Notarized 1,520 on five nodes | client took locks from PR0, which had none: no recovery children |
| diag-v2 | v5 (paged sketches, alternating base, client takes newest checkpoint) | all six, round 0, identical | 2,583 / 4,572 | Finalized 3,210, discarded-by-certificate 3,210, Notarized 366, Recovery 8,712, unresolved 11,934 | 1,525 recovery children published; ≈1,440 unchecked on PR0/PR2/PR4, pending on PR1/PR3/PR5 |

diag-v2 conditional termination upper bounds over the 2,583 terminated
branches: p50 39,033 ms, p95 47,136 ms, p99 47,957 ms (observation upper
bounds, censored population). Confirmed 43,316 / 45,000 at the ceiling.

Why the children stalled: the checkpoint retained the fork *primaries* as
locks (1,507 recovery, 87 notarized), and the client sends every
alternative to the even nodes and every primary to the odd nodes, so the
even nodes' ledgers held the omitted rival at the locked position and the
child of the retained primary had no parent there; the odd nodes could
give it only three votes. The v6 change makes a ledger follow the retained
branch on installation (roll back the omitted rival, install the retained
block from the fork cache or retained report data, or when it arrives as
evidence). diag-v3 below measures that.

| diag-v3 | v6 (ledgers follow retained branches) | all six, round 0, identical | 2,282 / 4,406 | Finalized 1,926, discarded-by-certificate 1,926, Notarized 1,056, Recovery 8,784, unresolved 12,744 | 1,784 retained; even nodes forced 1,402–1,425 retained primaries into their ledgers, odd nodes 1–6; 1,642 children; every node ended with 46,707 blocks, 46,465 cemented, 0 unchecked, ≤ 11 pending, identical hash |

diag-v3 confirmed 44,758 / 45,000 at the ceiling (168 s wall). Conditional
termination upper bounds over 2,282 terminated branches: p50 38,290 ms,
p95 46,536 ms, p99 46,866 ms. No evidence stayed missing, no conflicts, no
derived finality.

What remains unresolved and why: about 244 positions per node settled
*empty*, identically on all six nodes. They are most likely exact 3–3
first-vote ties (inferred from the workload, not checked per position):
the workload sends each alternative to exactly half of six nodes, so with
`r = 3` both branches reach the recovery threshold and Rule 2 retains
neither, and no boundary or fresh child can decide them without an owner
continuation, which the client only publishes for lock tips. The frozen
baseline leaves the same ~250 positions empty (258 in its one-checkpoint
attempt). The observer's remaining "unresolved" branch hashes are mostly
the omitted rivals of recovery locks whose primaries the children then
finalized live: with a single checkpoint there is no later checkpoint entry
to witness the discard, so the conservative classifier cannot count them.
A second checkpoint, or a fork distribution that is not an exact tie, is
needed to measure full termination; this is a property of the workload
and the observer, not evidence of a stalled node.

**Candidate v6 under the paired harness** (`no-crash-forks5-one-checkpoint-v6-candidate`,
run alone, 156 s ceiling): 2,236 fork pairs, 1,731 locks, 1,585 recovery
children. Confirmed 44,765 / 45,000 at the ceiling; timed out, so no
goodput or latency histogram was emitted. Five nodes ended identical
(46,415 cemented, 0 unchecked); PR5 was three blocks behind with a
different hash when the snapshot was taken. 214–232 empty positions per
node remain, as in diag-v3. Verdict CANDIDATE_ONLY, incomplete. No
performance result exists for the forked workload: the client's completion
condition needs every primary (or its alternative) confirmed, which the
tied positions prevent on both binaries.

## One pair, non-fork metrics and end-state equality (`nonfork-forks5-one-checkpoint-*`)

Requested measure: throughput and latency of non-fork blocks, and whether
all PRs end with the same state. Client v7 counts fork and non-fork
publications apart and ends when every non-fork block is confirmed.
45,000 blocks at 2,000/s, 5 % forks, one count-based checkpoint (40,000
decided elections), six nodes. Baseline: frozen HEAD, weighted model.
Candidate: v7, equal-weight model `f = p = 1`, certificate-only. One pair;
not a performance gate. The five-pair batch was stopped after the baseline
attempt at the user's request; the baseline's recorded `settled=false`
used the earlier predicate (pending elections required to be zero), and it
meets the revised one (identical hash and cemented count on all PRs). The
candidate then ran alone under the baseline-derived deadline
ceil(1.5 × 142.5 s) = 214 s.

| | Baseline | Candidate v7 |
|---|---:|---:|
| Non-fork blocks confirmed | 42,804 / 42,804 | 42,720 / 42,720 |
| Measurement duration | 31.8 s | 61.6 s |
| Non-fork throughput | 1,345 blocks/s | 693 blocks/s |
| Non-fork p50 / p95 / p99 | 104 / 9,706 / 10,608 ms | 232 / 1,940 / 2,932 ms |
| Forks unresolved at the end | 221 of 2,196 | 217 of 2,280 |
| Recovery children | 0 | 1,600 |
| Same end state on all six PRs | yes: hash CE05C468, 44,844 cemented on every node | no: five nodes BBFA707B with 46,455 cemented; PR2 9256A5EB with 46,260 and 1,847 pending |

The candidate's PR2 never installed checkpoint 0: it had 2 of the 5
required usable reports. Its frozen T (38,202 entries) differed from every
other reporter's, the six frozen roots were all distinct, and its 15
sketch replies were incomplete. This is the T-reconstruction straggler
problem again, with a straggler whose snapshot is ~550 entries off. The
baseline's p95/p99 come from a ~10 s confirmation stall at the checkpoint
boundary; the candidate's lower throughput comes from a measurement twice
as long, whose cause has not been diagnosed.

## Straggler fix and candidate-only runs (v8)

Diagnosis of the v7 divergence: PR2 reconstructed every reporter's T (sketch
and root differences both worked) and justified every N/F entry, but could
never derive reporter 633D9D61's G. Every other node had derived it from
3,294 hashes; PR2, deriving later, saw 3,297 and then up to 4,875. The
request aggregator answered PR2's epoch-0 vote requests by signing *new*
epoch-0 votes after the responders' reports had frozen, so their vote sets
outgrew the signed G roots. v8 (`ed69aa3d5`) signs no new account vote in an
epoch the node has left and serves only retained statements there.

From here on only the candidate is run (user's instruction). Same workload:
45,000 blocks at 2,000/s, 5 % forks, one checkpoint, paper model, 214 s
deadline, v8 client.

| Run | Non-fork throughput | p50 / p95 / p99 | Same end state on all six PRs |
|---|---:|---:|---|
| v8 #1 (8.2 GiB free) | 882 blocks/s | 1,057 / 3,890 / 6,165 ms | yes, 46,677 cemented |
| v8 #2 (8.2 GiB free, overlapped a 17 GB `du` scan) | 814 blocks/s | 1,041 / 2,578 / 3,291 ms | yes, 46,451 cemented |
| v8 A/B (15.6 GiB free, quiet host) | 925 blocks/s | 128 / 1,635 / 2,914 ms | yes, 46,544 cemented |
| v7 A/B, same client and conditions | 999 blocks/s | 213 / 1,282 / 1,746 ms | no: two hashes (46,405 and 46,462 cemented) |

The two slow v8 runs are environmental: the same binary on a quiet host
with free space gives p50 128 ms. They stay recorded as measured. The A/B
pair is one run each, not a gate.

## Two checkpoints (v8, candidate only)

`--epoch-terminated-elections 20000`: epochs 0 and 1 end and close, epoch 2
stays open for the tail. Same workload, paper model, 214 s deadline; the
run's database was deleted after evidence capture (`data-cleanup.json`).

| | |
|---|---:|
| Non-fork blocks confirmed | 42,740 / 42,740 in 36.6 s |
| Non-fork throughput | 1,167 blocks/s |
| Non-fork p50 / p95 / p99 | 1,896 / 5,423 / 9,247 ms |
| Checkpoints | epoch 0 (round 1) and epoch 1 (round 0) closed and installed identically on all six nodes: 19,216 and 22,915 finalized, none missing, 0 derived, 0 conflicts |
| Retained branches followed | epoch 0: 939 (up to 780 forced per node); epoch 1: 2,262 (1,180 forced on three nodes) |
| Recovery children | 793, for checkpoint 0 only: the client stops once the non-fork blocks are confirmed, before checkpoint 1's locks could be extended |
| Forks unresolved at the end | 1,226 of 2,260 |
| Same end state on all six PRs | no: five nodes at 44,636 cemented with one hash; one node at 44,635 with another, after the 30 s settle window |

Only one cemented block differs, in the open epoch 2; both checkpoints
agree everywhere. The data was deleted before the block could be
identified, so the cause is not diagnosed. Latency is higher than with one
checkpoint (p50 128–213 ms on a quiet host): with two boundaries, a larger
share of the non-fork blocks wait at a boundary.

## Two checkpoints: termination fixes and latency (v12c to v16, candidate only)

Same workload: 45,000 blocks at 2,000/s, 5 % forks, six nodes, paper
model `f = p = 1`, certificate-only, 214 s deadline, one run each, data
deleted after every run. Two checkpoints means
`--epoch-terminated-elections 20000`; one checkpoint means 40,000.

| Run | Change | Same end state | Non-fork goodput | p50 / p95 / p99 |
|---|---|---|---:|---:|
| v12c two | chain-root diagnostic only | no, timed out: epoch 1 never closed | n/a | n/a |
| v13 two | G derived from every signed vote hash; unplaced G blocks fetched | no: one node never decided checkpoint 1 | 1,109 blocks/s | 1,248 / 9,836 / 11,238 ms |
| v14 two | children started and voted on complete current-epoch parents | yes, 44,767 cemented on all six | 1,516 blocks/s | 1,238 / 3,130 / 3,560 ms |
| v14 one | same binary | no: two nodes lagged on 42 accounts, 0 conflicts | 1,186 blocks/s | 252 / 1,835 / 2,214 ms |
| v15 one | retained branches re-checked every 2 s | yes | 1,071 blocks/s | 530 / 1,802 / 2,095 ms |
| v15 two | same binary | no: two nodes lacked 522 and 2,889 checkpoint-1 finalized blocks, 0 conflicts | 1,290 blocks/s | 704 / 4,161 / 5,241 ms |
| v16 two | missing checkpoint-finalized blocks fetched; re-gossiped evidence kept off the block processor | yes, 44,968 cemented on all six | 1,435 blocks/s | 946 / 2,118 / 2,606 ms |

**Why epoch 1 never closed (v12c).** Three nodes could not derive the G
set of the other reporters. The manuscript derives Ĝ from the hashes the
reporter signed votes for, checks the root, and only then fetches blocks
and ancestry. The implementation placed every vote before deriving and
left out votes whose block it lacked. A lagging node missing 14 to 45 G
blocks never matched the root. v13 derives G from all signed vote hashes,
keeps a report unusable until every G member is placed, and fetches the
missing block or its deepest missing ancestor with the evidence request,
which now also returns blocks.

**Why a node did not decide checkpoint 1 (v13).** It refused the decided
value with `MissingAncestry`. The error now names the position and hash.
It did not recur in v14 to v16, so it was not diagnosed further.

**Children of non-final parents (v14).** The manuscript lets a validator
first-vote a child on a complete current-epoch parent, one with an epoch
NC, and finalizes an eligible child on its own certificate together with
its unresolved ancestors. The scheduler only started a child once its
parent was cemented here. It now walks past blocks complete in the current
epoch and is woken when an instance gets its NC. A receive still needs a
final send: attachment reports an unfinalized source before the parent. A
unit test finalizes a child on its own certificate while its parent is
only notarized. The benchmark client publishes a block only after its
predecessor is confirmed, so this workload cannot show a latency effect.

**Lagging nodes after installation (v14 one, v15 two).** Two failure
modes, both liveness, never a conflicting account. A retained lock target
forced in once at installation was not in the ledger afterwards, and the
owner's extension stayed in the unchecked table. v15 re-checks every 2 s
and re-forces unless a retained sibling is held or the rival is final.
Separately, installation only looked for checkpoint-finalized blocks in the
fork cache and report data; a node that never received 2,889 of them never
got them. v16 fetches them with the evidence request and forces them in on
arrival.

**Latency.** In v12c about 3,000 epoch-1 instances per node waited at the
predecessor gate. Every waiting chain root had a final parent, so parent
cementing was not the cause. After v13 only exact three-three fork splits
wait at the gate, which cannot finalize before the checkpoint. The rest of
the two-checkpoint penalty is the close running during live load. Each
node's report thread was busy about 15 of the 17 s the epoch-0 close took,
and a CPU sample of one node during the close showed its block-processing
thread spending the whole window verifying block signatures. Reporters
re-gossip their G blocks with ancestry every few seconds, and every copy
went through the live block processor. v16 sends an evidence block there
only when the ledger can take it: 17,000 to 41,000 blocks per node stayed
out. The host has 8 cores for six nodes and the client, and repeated runs
of one binary vary widely, so single runs do not settle the comparison.

In the v14 to v16 two-checkpoint runs the client finished before epoch 1's
close was due, so only checkpoint 0 was exercised under load there; v15
closed both checkpoints identically on all six nodes.

### Alternating one- and two-checkpoint runs (v16 to v21)

Each version ran one-checkpoint and two-checkpoint back to back, twice, on
the same host. Single runs of one binary vary widely here, so only the
ranges across the pairs are meaningful.

| Version | Change | One checkpoint p50 / p95 (ms) | Two checkpoints p50 / p95 (ms) | Runs with identical end state |
|---|---|---|---|---|
| v16 | (above) | 252 / 1,245; 116 / 1,696 | 622 / 2,300; 1,204 / 3,928 | 2 of 4 |
| v17 | G votes pulled by signed root; reply votes retained | 813 / 3,394; 560 / 1,616 | 1,636 / 2,740; 1,057 / 2,503 | 3 of 4; the fourth had identical cemented ledgers but one node without checkpoint 1 |
| v19 | G placed from block data; cached fork blocks not reprocessed | 238 / 1,478; 153 / 1,644 | 362 / 4,388; 483 / 3,810 | 4 of 4 |
| v20 | G derived from signed vote keys; final certificates shared across report checks | 171 / 1,296; 119 / 1,072 | 724 / 3,027; 440 / 2,750 | 4 of 4 |
| v21 | placements memoized; G blocks gossiped once per epoch | 266 / 2,304; 388 / 2,795 | 787 / 4,224; 2,841 / 6,717 | 3 of 4; the fourth: 51 blocks cemented on one node only, 0 conflicts |

**v17: a node without checkpoint 1.** Its derived G for one reporter
missed 5 of 1,300 hashes. It had dropped thousands of inbound messages under
load, and the reporter's periodic re-gossip was dropped too. A validator
whose G misses the signed root now names that root in its evidence request,
and the reporter answers with its retained signed votes. Separately, votes
signed to answer a vote request went only to the requester and were never
retained by the signer, so its own G could omit a hash others held a signed
vote for.

**v18: a refused checkpoint-1 value.** One node placed a G block at height 5
with a zero parent and refused the value the others built. G commits hashes
only, and each node placed G blocks from its own election metadata. G
members are now placed from the owner-signed block.

**v19: fork reprocessing.** Each node processed 42,000 to 62,000 fork
results for about 2,250 forks, twice the one-checkpoint count, because
unresolved positions wait for the checkpoint and their forks keep arriving.
A published block already in the fork cache now skips the block processor.

**Why two checkpoints stay slower.** Checkpoint 0 took 13 to 18 s to close
while an epoch lasts about 11 s at this rate. A node stops signing epoch-1
votes when epoch 1 ends and cannot open epoch 2 before checkpoint 0 is
decided, as the manuscript prescribes, so blocks published in between wait.
The close time is dominated by readiness: 4 to 23 s until N−f reports are
usable, on nodes whose report thread runs 3 to 7 s ticks and whose frozen
ledger lags the others. v21's once-per-epoch G gossip made readiness wait on
pulls with a 5 s retry, which v22 shortens to 1 s.


### Resumed investigation: v22 and v23

The v22 failed two-checkpoint attempt failed **before measurement**, during
`wait_for_equal_ledgers`: all six PRs held 65 setup blocks, but PR5 had
cemented only 64 after the 120 s setup wait. Its RPC snapshot has one pending
instance and no conflicting accounts. The retained evidence does not identify
the block or missing votes, and the database was already removed. This is an
unresolved setup confirmation failure, not a measured checkpoint latency.

Correction to the explanation above: manuscript §5.1 explicitly defers the next
boundary while an older epoch is closing and allows successor voting to
continue. Inspection of `end_epoch`, `try_advance_epoch`, `advance_epoch`, and
`kudzu_votes_due` confirms that the draining flag alone does not stop RAI
signing: freezing occurs on advancement. The prior assertion that reaching the
count limit itself stops epoch-1 signing is unsupported. Slow report readiness
and provisional settlement remain observable, but are not a demonstrated
protocol-required lower bound.

| Run | Non-fork goodput | p50 / p95 / p99 (ms) | Same end state on all six PRs |
|---|---:|---:|---|
| v22 one #1 (prior session) | 1,250 | 113 / 1,087 / 1,288 | yes |
| v22 two #1 (prior session) | n/a | n/a | no: setup exited 1, PR5 64/65 cemented |
| v22 one #2 (prior session) | 1,177 | 300 / 3,642 / 4,725 | yes |
| v22 two #2 (prior session) | 1,441 | 1,046 / 3,535 / 4,555 | yes |
| v22 resume one #1 | n/a | n/a | invalid environment attempt: sandbox denied node startup; harness timed out |
| v22 resume one #2 | 1,241 | 188 / 1,417 / 1,846 | yes, 46,329 cemented |

The resumed successful run used the unchanged pinned v22 binaries. The failed
sandbox attempt ran no node workload. Tests began after the successful run's
client measurement completed; this run is a diagnostic, outside the new
alternating batch.

v23 sizes the initial sketch to the local projection (one cell per 16 entries,
rounded up to a power of two, minimum 64 and maximum one 1,024-cell message).
Larger failed sketches retain their existing growth and paging. This avoids
64- and 256-cell failures for ordinary boundary skew in a large report.
Reconstruction still checks the signed root and all report semantic evidence.
A regression reconstructs a 20,000-entry target with 500 missing entries on the
first sketch; the complete node suite passes (836 tests). `cargo fmt --all` ran.

Pinned v23 node SHA256: `760b4bd87e5b9a20dcf9702bc4c3af03203ea6dc4411f50e42d23a3c3acbd5dd`.
Client SHA256: `9135ae7b7cbe7b8194351765700bf3810710fa5d494cfb6f222ef99577635dc7`.
Release build: locked, offline, successful. Alternating measurements below use
this same binary and client, with no concurrent builds or tests.

| Run | Non-fork goodput | p50 / p95 / p99 (ms) | Same end state on all six PRs |
|---|---:|---:|---|
| v23 one #1 | 1065 | 297 / 5090 / 5573 | yes |
| v23 two #1 | 1439 | 1185 / 5031 / 5569 | no; lagging=30, conflicting=0 |
| v23 one #2 | 1250 | 175 / 1640 / 2190 | yes |
| v23 two #2 | 1665 | 2223 / 4391 / 4702 | yes |

**v23 outcome:** p50 ranges do not overlap (one 175–297 ms; two
1,185–2,223 ms). The first two-checkpoint run differed on 30 accounts (zero
conflicts); PR4 installed checkpoint 1 about 3 s before evidence collection
ended. All six checkpoint state values agreed, but ledger convergence did not.

**Harness gap:** v23 two #2's `settled_consistent=true` meant only equal
cemented ledgers. Its RPC snapshots show current epoch 1 and no installed
checkpoint-0 value on any node. It therefore does **not** demonstrate completed
two-checkpoint convergence. Existing result JSON remains unchanged. Future
alternating runs use `--required-checkpoints 1` or `2`: settlement now also
requires identical installed state values and decided close values for every
required checkpoint. The 30 s settle window and measured client latency are
unchanged. All 24 harness tests pass, including missing and mismatched
checkpoint cases. This tightens measurement; it changes no protocol rule.


Profiling run `profile-v23-two-1` (excluded from performance comparisons):
1283 blocks/s; p50/p95/p99 1059/4778/5677 ms;
unequal ledgers (one lagging account, zero conflicts). PR2 cemented one block
more than the other five. A 5 s `sample` of PR4 is retained alongside the run;
an initial attempt named a nonexistent PID and collected nothing.
The successful sample shows network threads blocked acquiring the AEC lock in
`follows_retained_branch` and report-thread waits in `signed_votes_for_hashes`
and evidence verification. Report block lookup also enters the AEC before
checking the independent fork cache and retained data.

v24 avoids those AEC reads when possible: check ledger presence before the
checkpoint lookup, and consult cached forks/retained report blocks before the
AEC. Already held blocks still stay off the processor; missing retained blocks
still enter the forced path, and placement still checks owner signatures.


All 837 node tests pass; formatting and locked offline release build pass.
Pinned v24 hashes:

```text
7cfd6b5969d91744fac8b761baa6ebc6c7a2b39c5698ab62637073870dd8830a  rsnano
9135ae7b7cbe7b8194351765700bf3810710fa5d494cfb6f222ef99577635dc7  nanospam
```

The following batch requires 1 or 2 completed identical checkpoints, respectively.

| Run | Non-fork goodput | p50 / p95 / p99 (ms) | Same end state on all six PRs |
|---|---:|---:|---|
| v24 one #1 | 1083 | 316 / 1970 / 18065 | no; lagging=1, conflicting=0 |
| v24 two #1 | 1299 | 1942 / 5937 / 21589 | no; lagging=0, conflicting=0 |
| v24 one #2 | 1213 | 590 / 2196 / 2920 | yes |
| v24 two #2 | 1347 | 2678 / 6733 / 7286 | yes |

v24 still fails the target: two-checkpoint p50 1,942–2,678 ms versus
316–590 ms with one checkpoint. One #1 differed by one account (zero conflicts).
Two #1 had no installed checkpoint-1 value on PR2 by collection, despite zero
lagging/conflicting frontiers in the later frontier comparison. One #2 and
two #2 met the stricter ledger-and-checkpoint requirement. A 2.4 s harness
unit-test run overlapped this batch while adding post-verdict failure evidence;
retain these measurements, but do not use them as a clean final comparison.
Future failed runs save `failure-node-N.json` with checkpoint contents and
per-epoch entries after the settlement verdict, before deleting databases.
This evidence capture does not extend the measurement or revise a failed verdict.
All 24 harness tests pass.
