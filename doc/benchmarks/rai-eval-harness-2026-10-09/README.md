# Restart and equivocation-audit harness: smoke runs (2026-10-09)

Build: `963cdeeac` (branch `rai_eval_harness`), release, `rai_protocol`.
Workload: `run_gate_b.py`, 6 equal-weight PRs (f = p = 1), 45,000 blocks at 2,000/s,
5 % forks, 8 s epochs. Both runs on a quiet host (`performance_eligible`).

| Run | Command extras | Settled | Audit conflicts (slots checked) | Restarts |
|---|---|---|---|---|
| `audit-only` | `--audit-votes` | yes | 0 (544,715) | - |
| `restart` | `--restart 2:close:1 --restart 3:at:12` | **no** | 0 (480,200) | both killed and back up |

`audit-only` shows the signed-vote log and the auditor raise no false alarm on an
honest run (non-fork goodput 1,662 cps, p50/p95 141/695 ms; single run, not a
performance record: the log adds one write per signed vote).

## What the restart run found

Both restarts fired: PR3 SIGKILLed 12 s into the spam and back up 2.7 s later;
PR2 SIGKILLed in round 0 of the epoch-1 close and back up 6.0 s later. Neither
restarted node signed anything its earlier lifetime contradicts (0 conflicts
across restarts).

The run did not settle because **a restarted validator never rejoins the epoch
protocol**. After the restart PR2 and PR3 emit only `EPOCH_RECONCILED` lines
(serving reconciliation of epoch 0 from the restored reports) and never
`EPOCH_START`: the epoch clock T0 is set by the `epoch_start` RPC, which
nanospam sends once at setup, and it is not persisted; decided epoch values
and the epoch ledger are not persisted either, and there is no checkpoint
transfer to catch up with. Both nodes stay in epoch 0 (`current_epoch=0`,
4,857 and 9,722 pending elections). The four other nodes decided epochs 0 and 1
but the epoch-2 close never finalized (two of six validators missing their
reports), so cementing stopped at 43,938 of 45,065 blocks.

This is the restart departure the paper lists ("the restart path is exercised
by unit tests only"), now reproduced end to end. Fixing it needs, on the node:
T0 and the decided epoch values persisted (signing store), the epoch ledger
rebuilt from them at start, and catch-up for epochs decided while down
(checkpoint transfer). Step 2 of the plan.

Diagnose with the per-node tag: every diagnostic line now ends with
`node=prN t=<ms>`; nanospam logs `RAI_RESTART_{KILLED,UP,RECONNECTED,SKIPPED,MISSED,FAILED}`.

## Step 2: restart rejoin (`step2/`, build `1e1dc5e58`)

Three commits make a SIGKILLed validator rejoin:

- `12d3bbf0e` durable epoch records: T0 (unix ms) with the genesis frontiers and
  history, per decided epoch the close outcome, `d_e` and the committee
  frontiers, and the latest two decided states; `restore_epochs` replays them
  (`EPOCH_RESTORED`).
- `969dbde2c` checkpoint catch-up: an epoch closed here for 1 s without its
  value derived fetches `S_e` by `d_e` (`CheckpointReq`/`CheckpointReply`,
  60 KB chunks) and adopts the certified value (`EPOCH_CATCH_UP`,
  `EPOCH_CAUGHT_UP`). Needed because the other replicas release an epoch's
  reports once N-f successors installed it, so a node one close behind can no
  longer derive.
- `1e1dc5e58` an unready close solicits rounds 0-3 after 2 s (the certificate
  of a close missed while down), and a replica that sees members holding more
  than f of the committee's weight voting in later epochs ends its epoch at
  once (`EPOCH_FOLLOW_AHEAD`); `final_state` lists restored decided closes.

| Run | Extras | Settled | Epochs agreed | Audit conflicts (slots) | Non-fork cps | p50/p95/p99 ms |
|---|---|---|---|---|---|---|
| `audit-only` | `--audit-votes` | yes | 0-3 | 0 (529,071) | 1,733 | 172 / 664 / 897 |
| `restart-at` | `--restart 3:at:12` | yes | 0-9 | 0 (645,621) | 1,526 | 210 / 1,017 / 1,337 |
| `restart-both` | `--restart 2:close:1 --restart 3:at:12` | yes | 0-7 | 0 (654,027) | 1,388 | 177 / 909 / 1,608 |

Single runs each, quiet host. In `restart-both` PR2 (killed in round 0 of the
epoch-1 close, down 6.2 s) restored epoch 2 with the epoch-1 close open,
followed the members ahead, learned the epoch-1 certificate by solicitation and
fetched `S_1` (3.3 MB in 14 ms). Catch-up also fired on nodes that were never
restarted, when they fell one close behind (PR0 in an earlier run, PR4 here):
the release rule strands any laggard, restarted or not. Follow-ahead never
fired in `audit-only`.

Not covered yet: power loss (the signing store still uses the ledger's
`nosync_unsafe` flags), close-election votes are still not persisted, and a
restart before the epochs-started record (`Z`) is written loses
the epochs.

## Step 3: durable signing and its cost (`step3/`)

Commits: `fdd36eb8b` (`signing_sync = none | fsync | full`, default fsync;
close votes persisted per round as `C`; no record-less final replies once the
epochs run), `15675ecd3` (`SIGNING_WRITES` timings), `3594d6753` (decided
states and evidence moved to `epoch_records.ldb` with the ledger's setting;
old-epoch deletions in 2,000-key transactions), `9f892a841` (catch-up by the
certified value's hash; a close open for 2 s solicits rounds 0-3).

On this Mac LMDB's sync is `fsync` (`mdb.c:128`), measured at 0.05 ms per call:
APFS hands the data to the drive without waiting for its media. `F_FULLFSYNC`,
which does wait, measured 4.0 ms; mode `full` issues it after every signing write.

fork5, 6 equal-weight PRs, 45,000 blocks at 2,000/s, two runs per arm in
alternating order, quiet host. Non-fork goodput (blocks/s) and latency:

| Mode | `sync-ab-before-split` (`fdd36eb8b`) cps, p50 / p95 ms | `sync-ab` (`3594d6753`) cps, p50 / p95 ms |
|---|---|---|
| none | 1,737, 176 / 1,031 · 1,717, 164 / 568 | 1,765, 173 / 668 · 1,820, 139 / 677 |
| fsync | 1,740, 334 / 1,606 · 1,776, 346 / 1,244 | 1,802, 282 / 866 · 1,688, 310 / 1,400 |
| full | 1,740, 348 / 1,908 · 1,748, 331 / 2,226 | 1,763, 409 / 1,993 · 1,777, 304 / 1,161 |

Goodput is unchanged; synced signing costs about +130 ms at the median and up
to 2x at p95. The `SIGNING_WRITES` timings explain the first table: the vote
batches themselves took 1.4 ms on average under fsync, but the voter waited
for LMDB's one writer behind synced evidence batches (max 443 ms) and
boundary deletions (max 426 ms). With those moved out the longest signing
write fell from 464 to 172 ms.

`restarts/` (`9f892a841`, fsync): `--restart 2:close:1 --restart 3:at:12`
twice, both settled with every epoch's value equal on all six nodes and no
equivocation (0 conflicts); catch-up fired 1 and 4 times, follow-ahead 2 and
7 times. `fsync-perf` there: 1,786 cps, p50 / p95 273 / 935 ms. Two earlier
restart runs under fsync, before `9f892a841`, did not settle: a node left far
behind could not fetch states whose proposals were no longer repeated, and a
ready close never solicited the round the others had certified.

Still open: a restarted node takes 9-15 s to answer RPC after a respawn (2-3 s
in most earlier runs, 11 s once without sync; not attributed); the parent
records `P` are never forgotten.

## Step 4: committee rotation (`step4/`)

Harness (`a623778b7`): nanospam `--standby K` runs K extra representatives
outside the committee (an eighth of a share) and `--rotation "E:FROM>TO,..."`
moves FROM's whole balance to a holder delegating to TO at the start of epoch
E, so the committee derived from that epoch (used two epochs on) has TO in
FROM's place; the Byzantine representative keeps its weight and sits in every
committee. `check_committees.py` shows the identities change and agree.

Four membership bugs, each a stall in the first close after a rotation:
`b4b7eddd7` the leader selected a non-member's report (its claims cannot be
named by the committee bitsets of the manifest; the close split 2/3);
`0e6fcbd71` a member whose balance had moved away stopped voting with its key
and was tiered by weight; `faaa33833` its votes were dropped by the vote
applier's weight gate; `26da01408` a node missing a frontier block's
delegation derived a different committee (now it waits for the block).
Members are now known by membership (`CommitteeMembers`, the committees from
two epochs before the current one to two after).

Then a protocol hole (`4de75a071`): a fork position split 3-3 in epoch e
(both blocks retained in S_e under recovery records of origin e) was
finalized in e+1 by a changed committee, four of whose members first-voted
one side early, before S_e was known. The install-time recheck kept the
instance (the retained rival is admitted), the exit final vote followed the
notarization, and the block's bypass of the rival's record had no epoch-e
witness (3 supporters in the old committee, 4 needed). Every honest report
then carried an F entry the report rule refuses
(`EPOCH_UNJUSTIFIED_WHY ... bypassed=[Recovery@2 ... witness=false]`,
`partial-stalled-before-fix/`), no report was usable, and the epoch-3 close
never finished: 2 of 5 partial runs. The final vote is now withheld for a
winner `at_closed_lock` once the predecessor is decided, as a first vote is
(`no_final_vote_for_a_block_the_checkpoint_excludes`).

Results on `4de75a071`, fork5, 800 blocks/s, 20,000 blocks, `--stall-abort 40`:

| Scenario | Committees | Settled | Non-fork cps | p50 / p95 ms |
|---|---|---|---|---|
| `partial-1/2/3` (8 PRs, 2 standbys, PR4,PR5 -> PR6,PR7 at epoch 1) | 6 -> 6, two replaced | 3 / 3 | 667 / 747 / 748 | 106/320 · 103/422 · 104/372 |
| `partial-byz` (same, Byzantine member in both) | two replaced, then back at epoch 4 | 0 / 1 here; 2 / 2 earlier (`partial-byz-settled/`: 527 cps, 143/3,153) | - | - |
| `full-byz` (7 PRs, f=1 p=0: 4 seats, PR0-2 -> PR3-5, Byzantine shared) | 4 -> 4, all correct members replaced | 0 / 1 | - | - |

The two failures left are not rotation bugs: one honest member falls behind
in a close (pr6 stuck in round 1 of the epoch-2 close without two members'
timeout votes; one node in `full-byz` likewise in epoch 1), and with the
Byzantine member in the committee the next close needs every honest member's
report (N-f = 5 of 6, 3 of 4), so all wait for the straggler, who assembles
the missed certificate only through close-round solicitations (the same
fragility as in the close-round grace work). The full replacement needs the
4-member committee: 10 nodes at 200 blocks/s gave 54 cps on this host.
No equivocation in any run.

### The straggler, found (`step4/on-4582bf003/`)

The two Byzantine failures above had one cause, visible in the per-node
counters of `result.json`: the straggler is the node buried under vote
traffic. A node that falls behind has more unsettled elections, solicits
more, and the evidence replies overflow its inbound queue (64 messages per
channel): pr6 received 636,794 `ConfirmAck`s and dropped 32,029 of them
(`message/confirm_ack`, `message_processor_overfill/confirm_ack`) where the
other six nodes received 32-113k; `full-byz`'s pr2 received 1.6 million.
Among the drops were the few close-round votes it needed, and a round the
others have left is not re-broadcast, so the straggler never assembled the
certificate and, with the Byzantine member in the committee, every honest
report was needed.

`4582bf003`: close-round votes go to a lane of the inbound queue that is
drained first and exempt from the per-channel cap (4,096 in all); committee
members are peered principal representatives by membership, so the solicitor
asks a member whose weight moved away. Results, same settings as above:

| Scenario | Committees | Settled | Non-fork cps | p50 / p95 ms |
|---|---|---|---|---|
| `partial-byz-1`, `-2` (Byzantine member in both committees) | 6 -> 6, two replaced, back at epoch 4 | 2 / 2 | 578, 581 | 116/572 · 114/385 |
| `full-byz` (f=1, p=0: 4 seats, PR0-2 -> PR3-5, Byzantine shared) | 4 -> 4, every correct member replaced | 1 / 1 | 728 | 208 / 337 |
| `partial-1` (control, no Byzantine) | 6 -> 6, two replaced | 1 / 1 | 758 | 97 / 210 |

`check_committees.py` confirms the identities (`*.committees.txt`); no
equivocation in any run. Single runs each, quiet host.

## Step 5: departing validators hand off (`step5/`)

"Departing validators keep serving until successors hold durable copies,
including a departing validator that crashes." A departing member keeps an
epoch's handoff evidence (its reports and vote records) until `N - f` of the
successor committee acknowledged installing the epoch's checkpoint
(`EpochInstalled`); the release is logged as `EPOCH_EVIDENCE_RELEASED`.

`53f4b4d31`: the acknowledgement was sent when the checkpoint was installed
in memory, before anything of it was on disk, and an epoch whose checkpoint
finalized no new block was never acknowledged. It is now sent from the
report service right after the decided record (`W`, synced) and the decided
state (`D`, bulk environment, synced explicitly) are written, for every
decided epoch. The report exchange also dropped epochs after four whether
released or not; it keeps unreleased epochs up to 16 and logs a drop before
release (`EPOCH_RETENTION_FORCED`, zero in every run below).

`796dfe23a`: the first `depart-crash-byz` run decided epochs 0-5 everywhere
but the restarted departing member stayed 62 blocks behind. It caught up
epoch by epoch through the checkpoint catch-up, but by then the load had
stopped: its epoch 5 never started an election, so it never ended by time,
its close never opened, and nothing the members still sent showed them
ahead. Now a close vote of the current epoch or a later one counts for
`follow_ahead` (a member votes there only once it left the epoch), and while
the current epoch has no election the replica solicits the first rounds of
its close once per epoch duration. In the rerun pr3 logs
`EPOCH_FOLLOW_AHEAD epoch=5 voters_ahead=5`, catches up epoch 5 and installs
it. The same commit has nanospam ask a node that holds every setup block but
leaves one uncemented to confirm its unconfirmed frontiers again
(`block_confirm`); two of the first three runs failed in setup on that.

8 PRs (7 with the Byzantine one), 2 standby, 800 blocks/s, 20,000 blocks,
5 % forks. The rotation is scheduled at epoch 1; committees lag two epochs,
so the departing members serve through epoch 2. One departing member is
killed when the close of epoch 2 - the last epoch it is a member of -
starts, and is down 30 s:

| Scenario | Committees | Restarted | Settled | Decided everywhere | Non-fork cps | p50 / p95 ms |
|---|---|---|---|---|---|---|
| `depart-crash` (`1:4>6,5>7`, pr4 crashes) | 6 -> 6, two replaced | pr4, caught up 2-4 | yes | 0-4 | 340 | 100 / 220 |
| `depart-crash-byz` (+1 Byzantine, `1:3>5,4>6`, pr3 crashes) | 6 -> 6, two replaced | pr3, caught up 2-5 | yes | 0-5 | 335 | 117 / 473 |
| `partial-1` (control, no crash) | 6 -> 6, two replaced | - | yes | 0-3 | 750 | 100 / 210 |

Every decided epoch's evidence was released on every node except the
restarted one for the epochs it adopted by catch-up (it never held their
reports). No epoch was dropped before release, the committees are the same
on every node (`*.committees.txt`), and the vote audit finds no
equivocation. Single runs each, quiet host.

## Step 6: overlap certificates committed in the manifest (`step6/`, `a53482ed0`)

"Commit admission witnesses in the manifest." A block finalized under the
core overlap exception, before the predecessor checkpoint `S_{e-1}` is
known, is admitted on its overlap certificate: the closing-epoch exclusion
witness `XW_{e-1}(B)`, the notarization `NC_e(B)`, a matching-origin witness
for every record of the last closed state `S_{e-2}` it bypasses, and a
finalized prefix. Before this step none of that was fixed by a signed value:
`XW_{e-1}(B)` reached the manifest only incidentally (the earlier-finality
claims), the witnesses discharging `S_{e-2}`'s records not at all, and
nothing said which blocks the overlap admitted. The close checked a fresh
F entry against `S_{e-1}` only.

- `ManifestEntry` carries an `overlap` mark (one byte, 73 bytes per entry,
  in the digest `mu_e`): entry `(e-1, B)` is the closing-epoch witness of
  `B`'s overlap certificate.
- The candidate claims add, for every fresh finalized block, the exclusion
  witnesses that discharge the recovery records of `S_{e-2}` it bypasses.
- `overlap_certified` (`election/overlap_certificate.rs`): the fresh
  finalized blocks whose certificate the manifest proves. It is a function
  of the evidence, the selection, `S_{e-1}` and `S_{e-2}`, so the leader
  marks before proposing, and a follower that builds the same manifest from
  its own votes gets the same digest and keeps the shortcut it had.
- A fetched manifest whose marks are not exactly the certificates its
  evidence proves is refused (`EPOCH_MANIFEST_OVERLAPS_REFUSED`).

What is still not committed: which evidence each individual signer used
when it cast its own early final vote. The marks say what the decided
evidence proves, not what every voter held; that is the base reference of
1c.

The overlap route is the common one here: the nodes' own `overlap_eligible`
counters are about 20,000 of 24,000-26,000 finalized blocks per run, and the
closes mark 5,000-14,000 certificates per epoch (sum about 19,900 in
`B-fork5-1`, matching).

A/B against `7ac344163` (A), fork5, 6 equal-weight PRs, 45,000 blocks at
2,000/s, alternating order, quiet host; `byz1` adds one Byzantine
representative played by the client:

| Run | cps | p50 / p95 ms | close median / max ms | follower evidence check median / max ms | manifest fetches |
|---|---|---|---|---|---|
| A fork5-1 | 1,756 | 310 / 2,084 | 4,039 / 9,143 | 145 / 428 | 7 |
| B fork5-1 | 1,780 | 332 / 846 | 3,109 / 8,642 | 208 / 420 | 5 |
| B fork5-2 | 1,779 | 256 / 1,087 | 2,818 / 8,981 | 176 / 245 | 5 |
| A fork5-2 | 1,792 | 253 / 886 | 2,650 / 8,463 | 148 / 440 | 1 |
| A byz1 | 1,593 | 182 / 1,389 | 2,585 / 8,816 | 203 / 1,189 | 8 |
| B byz1 | 1,537 | 257 / 1,496 | 3,947 / 9,953 | 251 / 813 | 12 |

Every run settled with every epoch decided everywhere; no manifest, value or
claim was refused. Goodput, latency and close time are within the
run-to-run spread; the measurable cost is the follower's evidence check,
about 30-60 ms more at the median (marking walks the fresh finalized blocks
and the anchor's retained positions). Single Byzantine run per arm.

## Step 7: every first vote names its base (`step7/`, `94ee7d7fb`)

"Give each first vote its full base reference, not one bit." A first vote
said only whether its signer had installed the predecessor checkpoint (the
early bit). Which checkpoint it was cast on was nowhere: a receiver counted
every vote not marked early towards a fast certificate, and a manifest's
settled bits could not be checked against the signatures.

- `Vote` carries a signed `base` (32 bytes, RAI wire format only): the state
  hash of the checkpoint a first vote was cast on, `d_{e-1}` for a settled
  vote and `d_{e-2}` for an early one (the genesis state before epoch 0);
  zero for every other kind. The early bit stays.
- `CheckpointBases`: the installed checkpoints' state hashes, recorded by
  the active elections at installation and restore, read by the vote
  generators and the request aggregator's re-signed statements.
- Kudzu keeps each settled first vote with its base. An instance counts
  towards a fast certificate the ones naming the predecessor installed here
  (`SettledBase`); before it is installed they are held, and counted at
  installation; a vote naming another base never counts.
- The vote records keep `(voter, base)`: the manifest's settled bits, the
  certificate kinds behind the report check and the manifest vote check
  take the votes naming the installed predecessor only.
- The signed vote log writes `base=` on first votes.

Not covered: the paper's two other bases (a closing-epoch finalized parent
with its proof, a complete current-epoch parent) are per block, not per
vote; the base here is the checkpoint, the same for every hash a vote
carries. A re-signed early statement names the closed checkpoint held when
it is re-signed, which differs from the original only if `S_{e-2}` was
installed in between.

A/B against `a53482ed0` (A, step 6), same settings as step 6:

| Run | cps | p50 / p95 ms | close median / max ms | fast finalizations (all nodes) | final-vote finalizations |
|---|---|---|---|---|---|
| A fork5-1 | 1,781 | 307 / 1,532 | 4,003 / 9,492 | 119,232 | 154,223 |
| B fork5-1 | 1,533 | 280 / 2,388 | 4,434 / 10,869 | 105,309 | 174,570 |
| B fork5-2 | 1,782 | 308 / 1,731 | 4,126 / 8,853 | 107,404 | 166,641 |
| A fork5-2 | 1,780 | 265 / 1,177 | 4,156 / 8,493 | 119,183 | 169,088 |
| A byz1 | 1,510 | 186 / 1,266 | 3,130 / 10,578 | 111,366 | 117,620 |
| B byz1 | 1,613 | 167 / 1,605 | 2,946 / 8,969 | 121,590 | 106,230 |

Every run settled with every epoch decided everywhere and nothing refused.
The rule has the expected effect: a receiver that has not installed the
predecessor no longer counts settled votes at face value, so fork5 forms
about 10 % fewer fast certificates and finalizes those blocks by final
votes instead. Goodput and latency are within the run-to-run spread except
`B fork5-1`, whose epoch-2 close was derived twice (claims 30,377 twice) and
ran 1.4 s longer; `B fork5-2` matched the baseline exactly. The vote grows by
32 bytes (112 + 32 n to 144 + 32 n bytes for n hashes).

Pitfall: a baseline built from `git archive` into a reused target directory
keeps the old binary - the archive stamps files with the commit time, older
than the previous build. The first attempt of this A/B ran a stale baseline
and was discarded; the script now touches the sources and checks the
baseline binary for a string of the expected commit.

## Paper matrix (`matrix/`)

The nine-variant matrix of the paper's Table III, rerun on `be09dd392` (all of the above, `signing_sync = fsync`), one run per variant, `--wait-quiet 900 --timeout 150 --settle-timeout 240 --stall-abort 60`. Every variant settled with every running validator deciding the same checkpoints. `RAI_LaTeX/tools/eval_table.py matrix RAI_LaTeX/sections/eval-table.tex` regenerates the table.

| Variant | cps | p50 / p95 / p99 ms | Close median / max s | Close rounds | CP | Open | Late-notarized blocks with a closing-epoch witness |
|---|---|---|---|---|---|---|---|
| nofork | 1,845 | 131 / 742 / 976 | 2.6 / 7.9 | 0, 0, 0 | 3 | 0 | 97 % |
| nofork-offline1 | 1,944 | 116 / 257 / 447 | 3.7 / 4.0 | 0, 0, 1 | 3 | 0 | 97 % |
| nofork-byz1 | 1,950 | 117 / 290 / 651 | 2.8 / 3.9 | 0, 0, 1 | 3 | 0 | 91 % |
| fork5 | 1,786 | 294 / 857 / 1,001 | 3.4 / 8.6 | 0, 0, 0, 0 | 4 | 0 | 90 % |
| fork5-offline1 | 1,649 | 173 / 1,238 / 1,424 | 2.6 / 8.4 | 0, 0, 1, 0, 0 | 5 | 0 | 90 % |
| fork5-byz1 | 1,529 | 211 / 1,351 / 1,550 | 7.4 / 9.4 | 0, 0, 1 | 3 | 722 | 88 % |
| fork10 | 1,618 | 383 / 2,774 / 3,178 | 4.5 / 9.8 | 0, 0, 0, 0 | 4 | 0 | 82 % |
| fork10-offline1 | 1,654 | 661 / 3,424 / 3,999 | 11.7 / 14.2 | 0, 0, 1 | 3 | 2,962 | 82 % |
| fork10-byz1 | 1,273 | 740 / 4,332 / 5,241 | 7.9 / 15.7 | 0, 0, 1, 0, 0 | 5 | 1 | 80 % |

Against the unsynced matrix on `2b785288e` (`../rai-dsn-2026-10-08/fixed`): no-fork medians 4-13 ms higher, the all-online no-fork row's p95 from one 7.9 s close; fork rows within the spread seen between passes (fork5 goodput 1,667 -> 1,786, fork10-byz1 1,367 -> 1,273, p50 618 -> 740).
