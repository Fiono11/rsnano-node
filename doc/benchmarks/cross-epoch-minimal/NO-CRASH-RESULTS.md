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
