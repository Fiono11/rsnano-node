# Kudzu step 3 (`rai_protocol`), 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks (2026-09-18)

Same command and machine as `../kudzu-step1-2000-45k-2026-09-17`, plus `--fork-percentage 5`:
nanospam publishes a second, conflicting block for 5 % of the roots and hands it to every
second node, so each fork starts as a 3–3 split of the six equal PRs. Step 3 adds the
certificate evidence delivery for laggards and the fork candidate exchange.

Under Kudzu a forked slot is usually not finalized: it terminates with one or two notarization
certificates (plus a timeout certificate) and settles, so nanospam never counts most of the
~2,100 forked roots as confirmed. Legacy develop instead finalizes one side of every fork, but
only in a long tail after publishing ends. The comparison below therefore separates the
publishing phase (the first 24 s, ~43k non-fork blocks) from the fork outcome.

The run does not end on a timer. `run_fork_settle.sh` waits until nanospam stops confirming,
then `settle_check.py` polls the six PRs until every root is settled everywhere and in the same
way: a block cemented on one PR must be cemented on every other PR or be that PR's single
notarization certificate (no second certificate, no timeout certificate); a root nobody
finalized must carry identical certificate sets on all PRs. The run stops at
`SETTLED_CONSISTENT` (or `INCONSISTENT` / `TIMEOUT` after 900 s).

## Setup

| | |
|---|---|
| Base commit | `47f6c9f8b` (step 1) + uncommitted step-3 changes |
| Build | `cargo build --release -p rsnano_cli --features rai_protocol`; nanospam from the same tree |
| Command | `nanospam --prs 6 --no-prio --blocks 45000 --accounts 45000 --rate 2000 --fork-percentage 5 --no-kill` (`run_fork_settle.sh`) |
| Develop reference | plain `develop` (`ff2b32710`) built in a worktree, same command (`develop-run.log`) |
| Thresholds | f = p = 19 %: certificate 4 of 6 PRs, fast finalization 5 of 6, second look at 3 of 6 |
| Machine | Apple M1, 8 cores (4P+4E), 16 GB; idle before start, `caffeinate -i` |

## Headline (nanospam, publishing phase = status lines with ≥ 1000 cps)

| Metric | Kudzu step 3 | develop |
|---|---|---|
| Non-fork blocks confirmed | 42,819 (all published non-fork blocks) | 40,055 at t = 25 s, 44,993 at the end |
| Confirmation rate | 1832 cps | 1748 cps |
| Average confirmation time, cps-weighted | **213 ms** (one 695 ms second at ramp-up; 149 ms in the run before) | 402 ms |
| Median of the per-second averages | 123 ms | 344 ms |
| Forks | 2,072 roots settled identically on all six PRs **11 s** after publishing ended; 42,981 blocks cemented on every PR | 4,606 blocks confirmed in a 31 s tail at 12–25 s each; 7 never |

Without forks the same build finalizes in ~100 ms (step 1: 110 ms, develop: 205 ms), so
5 % forks cost Kudzu nothing on the non-fork path while develop doubles.

## Node-side latency (AEC histograms and percentiles, `stats` RPC at the end of the run)

Measured from election start on each PR; `kudzu-node-latency.txt` has the buckets.

| PR | non-fork finalization p50 / p95 | non-fork termination p50 / p95 | fork termination p50 / p95 | fork termination p99 / max |
|---|---|---|---|---|
| PR0 | 101 / 356 ms | 81 / 235 ms | 151 / 442 ms | 885 / 10010 ms |
| PR1 | 97 / 242 ms | 78 / 187 ms | 149 / 361 ms | 518 / 10377 ms |
| PR2 | 99 / 345 ms | 80 / 285 ms | 149 / 446 ms | 769 / 10461 ms |
| PR3 | 99 / 444 ms | 80 / 385 ms | 148 / 592 ms | 9899 / 10541 ms |
| PR4 | 101 / 264 ms | 81 / 203 ms | 153 / 375 ms | 602 / 10394 ms |
| PR5 | 101 / 324 ms | 80 / 262 ms | 150 / 379 ms | 698 / 10550 ms |

Fork termination (first certificate) takes about 50 ms longer than a non-fork finalization at
the median and stays under half a second at p95. The maxima (~10 s) are the few forks whose
candidate or votes were lost on one PR and recovered through two 5 s solicitation rounds.
Before the fork candidate exchange (same build minus the request aggregator reply) fork
termination averaged 5.3–8.4 s: the other candidate only reached the other half of the PRs
through the legacy winner broadcast, about 5 s after the election started.

Run-to-run variance on this machine is large: in about one run out of three one PR (usually
PR3 or PR5) is starved by the machine and shows 350–450 ms p50 / 5.5 s p95 while the other
five show the table above; the settle phase and the agreement are unaffected.

## Settlement and certificate agreement (`kudzu-settle-check.txt`, `kudzu-certificates.txt`)

`settle_check.py` reported `SETTLED_CONSISTENT` 11 s after publishing ended (10 s in the run
before): all 2,072 forked roots settled on all six PRs with identical certificate sets (1,046
with two notarization certificates, 775 with a timeout certificate), every block finalized on
one PR finalized on all (42,981 cemented everywhere), and no PR ever held a certificate set
that contradicts a finalization elsewhere (Lemma 5.7). With the candidate exchange both halves
of a 3–3 fork take their second look at once, so the slot dies with both blocks notarized;
4–2 forks finalize the majority block through the final votes of the four holders.

Five fixes were needed to get there (each one left ~0.1–1 % of the forked roots different
for good, or delayed their convergence by the 60 s duplicate-filter cutoff):

* a settled election keeps soliciting while a finalization certificate is still possible
  (`SlotVotes::can_finalize`: the weight that notarized nothing but the certified block can
  still final-vote for it), and the solicitor plugin routes `Settled` elections through that
  gate; a PR that dropped a final vote gets it from the representatives, which answer with
  their final vote for the cemented block;
* request aggregator replies carry the evidence flag, so a re-sent, byte-identical vote is not
  dropped by the requester's duplicate filter (the rep crawler accepts such replies as direct);
* a received block stays a duplicate for 5 s instead of 60 s (`PUBLISH_AGE_CUTOFF`), so a fork
  candidate or evidence block whose first copy was lost gets through on the next request;
* on a winner change the node keeps its own statements in the election (legacy withdraws its
  votes for the previous winner to vote again; under Kudzu that silently removed the node's
  own first and timeout votes from its tally, e.g. a timeout certificate at 3 of 4 votes);
* the quorum `n` is fixed and identical on all PRs: nanospam sets `online_weight_minimum` to
  the whole voting weight in every node config, and only starts publishing once every PR
  reports the full stake online and peered (`wait_for_full_quorum`). Before that each PR
  derived its thresholds from its own view of which representatives had voted recently.

## What step 3 changed (all under `rai_protocol`)

* Evidence for laggards: a `ConfirmReq` for a terminated election is answered with the
  candidate blocks and the node's own statements re-signed for exactly that election
  (`ConfirmAck` evidence flag, bypasses the duplicate filter, never cached or rebroadcast).
* Fork candidate exchange: a `ConfirmReq` for a block we do not hold, for a root where our
  ledger has a different successor, is answered with that successor (`Publish`). This is what
  brings fork termination from seconds to ~140 ms.
* Terminated elections leave the AEC buckets (no capacity cost), elections holding votes are
  never evicted, the vote cache keeps one vote per kind, forks are flooded to the PRs when
  added to an election, solicitation asks every representative whose final vote is missing.
* AEC stats: termination / finalization latency histograms with p50 / p95 / p99, split by
  fork / non-fork; `confirmation_info` reports the Kudzu state, certificates and whether the
  election can still be finalized.

Rejected on the way: requesting missing blocks from the hinted scheduler (vote cache driven).
At 2000 bps half of the blocks are still in flight when their votes arrive, so it requested
~23k blocks per node and the replies overloaded the vote processor (latency 100 ms → 18 s).
