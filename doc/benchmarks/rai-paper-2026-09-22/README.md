# RAI: paper compliance, phase by phase (2026-09-22)

Bringing the `rai_kudzu` implementation in line with *RAI: Multi-Tree Kudzu with Epochs
and Lagged Reconfiguration* (simplified specification, 2026-09-22, `RAI.pdf`), one phase
at a time. After every phase the four benchmark variants are rerun; a phase is kept only
if performance is maintained against the baseline.

Base: plain HEAD `699a0cd1e` (the user's choice; the uncommitted Sept 21 fixes in
`stash@{0}` are not used).

## The runs

Same workload as `../rai-committees-3epochs-8s-2000-45k-2026-09-20`: release build with
`--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks, three epochs of 8 s
(`tools/run_variant.sh`). Four variants per build (`tools/run_matrix.sh`):

| variant    | nanospam                          | nodes | checks |
|------------|-----------------------------------|-------|--------|
| `fork0`    | `--fork-percentage 0`             | 6     | settle, committees, safety |
| `fork5`    | `--fork-percentage 5`             | 6     | settle, committees, safety |
| `offline1` | `--fork-percentage 5 --offline 1` | 5     | as above, pending instances allowed |
| `byz1`     | `--fork-percentage 5 --byzantine 1` | 5   | as above, pending instances allowed |

Headline numbers (`tools/summarize.py`): confirmation rate and median confirmation time
over the seconds at >= 1000 cps. "Performance maintained" means rate and median within
the run-to-run noise of the baseline (about ±5 % rate, ±10 ms median), and every check
passing.

## Phases

Numbered items refer to the change list agreed on 2026-09-22.

| phase | items | what |
|-------|-------|------|
| 7a | 21 | The timeout block is notarized in place of a many-voted block this replica can not obtain |
| 4 | 13, 14 | Close counted in the old and the new committee; ordinary instances of e+1 in C(e−1) alone |
| 1 | 1–4 | Reports: per-epoch authenticated vote map, signed report, reconciliation, retention test |
| 2 | 5–7 | State S_e = (Σ_e, U_e), validity rules, committee derivation from Σ_e |
| 3 | 8–12 | Close block object, sticky close value, certificate objects, validation instead of comparison, deterministic leaders |
| 5 | 15–17 | Stop voting at the boundary, open e+1 at the boundary, time-only epochs |
| 6 | 18–20 | Discard by S_e, retention for U_e, resolution of U_{e−1} |
| 7b | 22 | Per-slot Δ_timeout first vote - deferred until after phase 3, see below |

## Results

Confirmation rate and median confirmation time over the seconds at >= 1000 cps.

| build | fork0 | fork5 | offline1 | byz1 |
|-------|-------|-------|----------|------|
| baseline (HEAD 699a0cd1e) | 1957 cps / 95 ms | 1818 cps / 110 ms | 1858 cps / 108 ms | 1 stall, 1 pass |
| phase 7 (items 21+22) | 2100 cps / 95 ms | 1903 cps / 106 ms, **settle fails** | - | - |
| phase 7a (item 21) | 1955 cps / 94 ms | 1963 cps / 107 ms | 1911, 1910 cps / 109, 110 ms | pass |
| phase 1 (reports) | 2046 cps / 96 ms | 2066 cps / 108 ms | 1995, 1920 cps / 108, 107 ms | pass |

`offline1` is the A/B of `offline1-ab/` (phase 7a against the base) and `phase1-ab/` (phase 1
against phase 7a); the other three are the matrix runs. Every check passes on every variant of
both phases.

Every check (settle, close, committees, safety) passes on `fork0`, `fork5` and `offline1`.

### Phase 1: what a report costs, measured

The first phase-1 build reconciled every report as it arrived, walking a two-level dictionary of
256 x 256 buckets. It was correct - every reconciliation reached the signed root, including the
full-report fallback from an empty base - and unusable: the leaf level was so sparse that each
differing entry sat in its own leaf, so a reconciliation cost about one round trip per differing
entry. `phase1-smoke.log`:

```
EPOCH_RECONCILED epoch=0 complete=true requests=0     entries=0     total=30601
EPOCH_RECONCILED epoch=0 complete=true requests=2455  entries=2570  total=30766
EPOCH_RECONCILED epoch=0 complete=true requests=24719 entries=30601 total=30601
EPOCH_RECONCILED epoch=0 complete=true requests=45587 entries=30601 total=30601
```

The report traffic starved the consensus traffic: median 1734 ms against 110 ms, and the safety
check failed only because the run never converged (PR4 left with 7636 undecided roots, PR5 with
1483, against 175 on the others).

The numbers the run produced are the useful part. A replica's report holds **~31000 entries** per
epoch (a first and a final vote over ~15500 slots), and two replicas differ in **~800-2700** of
them, spread over the whole key space. A fixed partition of the key space therefore has nearly
every bucket differing, whatever the bucket count, so a reconciliation transfers most of the map.
The paper's Merkle search tree behaves the same way for a spread difference; what bounds the
transfer by the size of the difference is set reconciliation (an IBLT). That is the plan for when
the close starts to depend on reports, and it is recorded in `VoteReport`'s documentation.

Two changes followed, both from the measurement:

- **Reconcile on demand.** Section 6.2 reconciles a report when a close proposal has to be
  validated against it. Nothing decides on a report yet, so a report is now verified and stored,
  and `reconcile(epoch, reporter)` starts the walk when the close needs it.
- **One bucket level.** 256 buckets over the hash of the key, a bucket's digest the XOR of its
  entries', and a reconciliation fetches whole buckets. Same guarantees, far fewer messages, less
  code.

`phase1-smoke2.log` after the change: every check passes at 1876 cps / 108 ms, six reports of
~31000 entries per epoch, zero reconciliations.

### What phase 1 does not do yet

The report is signed when the node leaves the epoch, and the node keeps casting the exit final
votes of that epoch's instances afterwards (the drain). A final vote issued after the signature
is therefore not in the report, while Lemma 7.1 assumes a correct replica reported every final
vote it issued. That is item 15 - voting stops at the boundary - and it can only land together
with a close that decides by `possible_Q` instead of waiting for every instance to settle. Until
then nothing reads a report, so nothing depends on the gap; it is the first thing to fix when the
close starts to.

### `offline1` is A/B'd, not compared against a stored sample

Two phase 7a `offline1` runs came out at 190 ms and 178 ms against the baseline's single 108 ms
sample, which looked like a regression. It was the machine: the variant's setup (the wait for
every PR to see the same quorum) ranges from 2 ms to 43 s, and the runs that took long also
confirmed slowly. `offline1-ab/` therefore alternates the two builds, with the base commit built
in a worktree and picked by `NODE_BIN`, so thermal and background drift falls on both sides
equally.

### Phase 7: Δ_timeout on an ordinary slot has to wait for the report phase

The two rules of phase 7 were measured together first (`phase7/`). Throughput and latency
were at the baseline (fork0 2100 cps / 95 ms, fork5 1903 cps / 106 ms), but the fork5 settle
check failed: epoch 2 never reached the same state on every replica. More than twenty slots
read `finalized | - | finalized | - | finalized | -` - decided in epoch 2 on PR0, PR2 and PR4
and not on PR1, PR3, PR5 - and PR5 cemented 28 blocks fewer than the rest. The alternating
pattern is nanospam's fork distribution, which sends the fork to every second node.

A bisect with item 22 alone disabled (`bisect/item21-only-fork5.log`) passed every check at
1959 cps / 111 ms, so item 22 is the cause.

Why: a replica that spends its first vote on the timeout block can no longer cast the exit
final vote of that instance (line 11 needs notarized ⊆ {B}), and it adds timeout weight. With
Δ_timeout the choice between first-voting the block and abstaining is made by each replica's
own clock, so two replicas terminate the same instance differently - one with the block in the
tree, one by timeout - and the epoch's state hashes differ. The paper allows exactly this: the
close does not compare states, it selects N−f reports and keeps every hash that can still be
certified (Lemmas 7.1 and 7.2). The implementation's close instead agrees on one state hash,
so any vote decided by local timing leaves a replica attesting a value nobody else holds.

Item 22 therefore belongs after phase 3, not before it, and is deferred (phase 7b). Item 21
fires only for a block that is genuinely missing and is kept (phase 7a).

### `byz1` is not a performance gate

The first Byzantine-representative run stopped confirming at 35375 of 45000 blocks (2004 cps
while it ran, median 458 ms, 178 late discards, epochs 0 and 2 closing in round 1). The repeat
(`baseline/byz1-repeat.log`) finished every block and passed every check - CLOSED_CONSISTENT
over five epochs, COMMITTEES_CONSISTENT, SAFE - but took 87 s to publish against ~23 s for the
honest runs, with 15 busy seconds and a cps-weighted confirmation time of 1416 ms. The faulty
representative's random votes make the run's timing useless as a measurement, and its outcome
is not reproducible either: the Sept 20 record had `--byzantine 1` passing at 1862 cps, and the
Sept 21 investigation (`rai-termination-2026-09-21`, logs not kept) traced stalls of this kind
to two bugs whose fixes are not in this tree (committee weights over the supply wrapping
`u128`; a fork's loser kept in the ledger).

The gate for a phase is therefore: `fork0`, `fork5` and `offline1` within noise of the baseline
with every check passing, and `byz1` passing its checks in at least one of two runs.
