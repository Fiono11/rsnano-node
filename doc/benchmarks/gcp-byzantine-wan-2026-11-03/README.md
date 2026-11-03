# One Byzantine member across six regions (Nov 3 2026)

Diagnosis runs for the matrix cell `fork10-grace-byznode` (2000/s x 45k, f = p = 1,
8 s epochs) on the six-region n2-standard-4 fleet. The Byzantine member (PR5,
`735FE217`) votes for both blocks of every fork, sends conflicting reports and leads
the first round of every close with two proposals (`--byzantine-node
equivocate,conflicting,equivocate --byzantine-leads-close`). All logs, `host.txt` and
`result.json` files are gzipped (`zcat`).

| Run | Node build | Cell | Result |
|---|---|---|---|
| `smoke-fork10-byz-rai` | a5e3c7cfe | fork10-byz, 2000/s | not settled: closes up to 96 s, ledgers 28.5 k / 43.3 k |
| `smoke-fork10-byz-develop` | Nano afe63ab1c | fork10-byz, 2000/s | settled; p50 525 ms, p95 1.17 s, p99 12.4 s; 2,227 of 4,436 forks confirmed |
| `probe-fork10-honest-rai` | 6baccf679 | fork10, honest, 2000/s | settled; p50 388, p95 519, p99 720 ms; close max 4.0 s; forks resolved |
| `probe-byz-equivocate-honest-honest` | 6baccf679 | votes only Byzantine | **not settled**: closes up to 73 s |
| `probe-byz-honest-conflicting-equivocate` | 6baccf679 | reports + leader only | settled; p95 558 ms; close max 4.7 s |
| `smoke6-fork10-byz-rai` | 078063f88 | fork10-byz, 2000/s | not settled (drain limit), epoch 3 never closed |
| `cap-fork10-honest-2000` | 078063f88 | fork10, honest, 2000/s | settled; p50 407, p95 543, p99 735 ms; close max 4.6 s (no regression) |
| `cap-fork5-byz-2000` | 078063f88 | fork5-byz, 2000/s | **not settled**: close max 90 s, median 9.5 s |
| `cap-fork10-byz-1000` | 078063f88 | fork10-byz, 1000/s | **settled**; p50 421 ms, p95 2.26 s, p99 2.89 s; close median 8.6 s, max 33 s; forks resolved |

## What the runs show

- Only the equivocating votes break it: the honest cell and the Byzantine reports and
  leader alone settle with sub-second tails.
- With equivocating votes thousands of forks stay contested until checkpoints decide
  them, so thousands of instances collect evidence for minutes. Elections put into a
  request went from ~50 k per node (honest) to 1.4-11 M; aggregators overfilled by
  10-50 M hashes, vote processors by up to 1.5 M votes; the node furthest behind
  (Taiwan) dropped 200-330 k block publishes. Dropped evidence made reports unusable;
  with the Byzantine report unusable a close needs all five honest reports, so the
  slowest honest node gates every close and slow closes cascade (an epoch is left
  only once the one before closed).
- The same cell settles on one host because there a request is answered within a
  tick (the dev network's 25 ms base latency, 20 ms AEC tick, 250-500 ms re-sends),
  instances live ~10x shorter and no node falls behind; across 32-313 ms round trips
  every repeat interval multiplies the load.
- Capacity: with one equivocating member RAI settles across six regions at 1000/s
  (10% forks) but not at 2000/s, even with 5% forks.

## Fixes made along the way (rai_multihost)

| Commit | Change | Effect on this cell |
|---|---|---|
| ba6611da7 | `min_request_interval_ms` floor = longest round trip on `--hosts` runs | aggregator overfill 33 M -> still collapsing |
| 6baccf679 | missing report evidence asked of the reporter first, retries backing off | evidence re-flood gone; closes still 100 s |
| aeeee320c | a terminated instance does not re-ask for statements it holds | not enough alone |
| 2914d1a35 | fruitless solicitations of instances collecting evidence back off (to 8 s) | not enough alone |
| 078063f88 | repeated winner broadcasts back off, never before a round trip | not enough alone |
| 71704e31e, 7ff00568f | harness: end a run still draining 60 s after its load; no fork grace for a failed run | stalled runs end in ~1 min |

None regresses the honest cell (`cap-fork10-honest-2000` vs `probe-fork10-honest-rai`).
Open: an instance that cannot finalize under equivocation still generates traffic for
minutes; and a lagging reporter's own early vote lost in transit can only be supplied
by that reporter (`smoke5`, not kept: its close waited on it indefinitely).
