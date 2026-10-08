# Close rounds that outlast their timeout (2026-10-08 evening)

## What was debugged

A `fork10-byz1` rerun for the DSN matrix ended without a result and was first read as the "epoch-3 close never ready" stall. The log shows otherwise: the client had finished (45,000 blocks in 37.8 s), epochs 0-2 had closed on every node and epoch 3's close was ready with a proposal in flight when the harness's settle poll failed (5 s RPC timeout against a node busy validating), after which `os.killpg` raised EPERM in the cleanup and no `result.json` was written. `run_gate_b.py` now retries failed polls, writes the record before cleaning up and tolerates the kill error (`../rai-dsn-2026-10-08/rerun`).

Four further runs of the variant on the same binary (`../rai-dsn-2026-10-08/stallhunt`) all settled, but every close with a Byzantine member took 15-18 s. The round timeline (`EPOCH_CLOSE_ROUND`, `EPOCH_PROPOSED`, `EPOCH_MANIFEST_FETCHED`, `EPOCH_VALIDATE_TIMING`) shows why:

- With a Byzantine member every leader's evidence manifest differs from the validators' own (the random votes are seen differently), so each proposal is checked on the slow path: fetch the manifest (45 sequential chunks, 0.6-0.9 s), fetch missing votes (0.2-0.5 s), check the evidence (0.5-1.0 s), rebuild the state (0.5-1.2 s): 2.5-4.4 s per proposal.
- The close round timeout is 2 s. A validator that times out abstains (its first vote goes to the timeout block) and can no longer vote for the proposal once checked, so the round fails for nothing; the next leader proposes a value with yet another manifest and the same happens again. Rounds succeeded only when manifests had converged through the evidence fetches.
- Round 0 fails even earlier: validators enter it as soon as they are ready, the leader is often the last to be ready and derives its value for 1.2-1.7 s first, so its proposal arrives 2.5-3.5 s after the first validators entered and after they abstained.
- A round led by the Byzantine member (one in six) costs the full timeout plus the timeout certificate, about 2.4 s, which is inherent.

This is the paper's own liveness condition, Δ_timeout > κ + τ + δ, violated by a 2 s timeout against a 1.5 s entry skew and a 3 s check.

## The fix (local rules, protocol unchanged)

1. **Check grace.** While the leader's proposal for the current round is being checked here (manifest or evidence still being fetched, state being derived), the round's timeout is three times Δ_timeout instead of one. A check that ends in a refusal restores the normal timeout at once. (`EpochClose::mark_checking`, `clear_checking`, `CHECK_GRACE`; the decision service's `validate` now reports `Accepted | Pending | Refused | Skipped`.)
2. **Leader grace.** While the round's leader is a member whose usable report this node holds and no proposal from it has arrived, the same extended timeout applies: a leader that reported is alive and proposes once it is ready. A leader that never reported (offline, or the harness's Byzantine member) is not waited for.
3. **Re-proposal.** A leader of a later round proposes again a child of election genesis it has already validated (same reports, manifest and state, new slot) instead of deriving a fresh selection with its own manifest; validators that checked that payload in any slot accept it without deriving again (`EPOCH_REPROPOSED`, `EPOCH_VALUE_REUSED`). A payload's check depends only on the payload.
4. **Windowed manifest fetch.** Up to eight manifest chunks are requested at once instead of one per round trip (`MANIFEST_WINDOW`, `ManifestAssembly::missing_chunks`); fetches dropped from 0.6-0.9 s to 0.3-0.5 s.

Safety is untouched: timeouts are local in Kudzu, a re-proposed payload is an eligible value like any other, and the validation cache returns the state the same inputs derived before.

## A/B (same host, same evening, equal-weight f = p = 1, 45k blocks at 2,000/s, 8 s epochs, settle timeout 240 s)

Base `06e099d37`, four runs (`../rai-dsn-2026-10-08/stallhunt`):

| run | cps | p50 / p95 / p99 | longest close | rounds per epoch |
|---|---|---|---|---|
| 1 | 1,086 | 684 / 3790 / 4532 | 17.8 s | 0, 2, 1 |
| 2 | 1,049 | 790 / 4371 / 5068 | 18.1 s | 0, 2, 1, 0, 0 |
| 3 | 1,079 | 560 / 4587 / 5454 | 17.3 s | 0, 2, 1, 0, 0 |
| 4 | 1,076 | 630 / 4319 / 5610 | 18.0 s | 0, 2, 2 |

Rules 1, 3, 4 only (`check-grace/`): fork10-byz1 17.0 s / 15.0 s longest close, p95 12.2 s (a 6-epoch run whose last client blocks waited for a close) and 3.0 s; the round-0 failure remained (proposal after the abstains), re-proposal closed a round in 0.4 s once.

All four rules (`leader-grace/`):

| variant | cps | p50 / p95 / p99 | longest close | rounds per epoch |
|---|---|---|---|---|
| fork10-byz1 (1) | 1,249 | 606 / 3012 / 3584 | 14.4 s | 0, 0, 1, 0, 0 |
| fork10-byz1 (2) | 1,299 | 534 / 3302 / 5181 | 15.6 s | 0, 0, 1, 0, 0 |
| fork5-byz1 | 1,602 | 164 / 1353 / 3115 | 8.1 s (base 12.7) | 0, 0, 1 |
| nofork | 1,912 | 122 / 511 / 1011 | 4.5 s (base 5.3) | 0, 0, 0 |

Every correct-led round now decides in that round; the only extra rounds are the ones the Byzantine member leads. The remaining close time is the serial chain: a close cannot start before the previous one has decided and the node has reported, the Byzantine round costs 2.4 s, the leader derives for 1.2-1.7 s and the validators check for 2.5-3 s.

## Still open

- Round-0 readiness skew: the leader of round 0 could derive its value before the round starts, and the derivation itself (claims, manifest, build) is 1.2-1.7 s of single-threaded work.
- The evidence check with a fetched manifest (0.5-1.0 s) re-reads every entry; only the entries that differ from the validator's own manifest need checking.
- Sequential closes: an epoch's report waits for the previous close; with 8 s epochs and 8-15 s closes the pipeline runs behind by one epoch under this load.
