# Twelve seats, fork10 with a Byzantine validator: fixed (Nov 15 2026)

Build baba6248c: a proposal is checked against the part of its manifest whose votes the
validator holds, and the rest is fetched only if that part does not prove the same overlap
certificates, justify every claim and derive the proposed state; manifest chunks are asked 32
at a time. Same cell, seed and fleet as `../gcp-n12-confirm-2026-11-12` and
`../gcp-n12-byzfix-2026-11-12` (twelve seats, f = p = 2, 2,000 blocks/s, 10 % forks, the
Byzantine validator leading round 0 of every close).

| | b49726dce | b02c83039 | **baba6248c** |
|---|---|---|---|
| Non-fork blocks/s (1,800 offered) | 209 | client gave up | **1,663** |
| p50 / p95 / p99 | 585 ms / 27.1 s / 28.1 s | – | **573 ms / 2.5 s / 3.4 s** |
| Close median / max | 19.3 / 154 s | 45.3 / 129 s | **5.0 / 9.7 s** |
| Rounds per close, at most | 11 | 6 | **2** |
| Votes dropped per honest validator | 0.5 - 2.0 M | 0.1 - 1.8 M | 200 - 291,000 |

Settled, every block final and identical on all twelve validators, one decided state per epoch.
Across all validators 61 proposals were accepted on the held part and none needed a vote fetched
(`EPOCH_MANIFEST_VOTES_MISSING` 0 times; `diagnosis.txt`).

**Cause of the collapse:** a validator fetched every vote a manifest names before voting. A
manifest names all the votes its proposer holds, and with a Byzantine validator hundreds to
thousands of them are votes a follower never received (the equivocator's own final votes; other
validators' final votes dropped under load) and does not need. Fetching them took 9 to 35 s per
manifest against a round timeout of a few seconds, so validators abstained round after round.

One run. Two validators (PR8, PR9) still dropped 0.24 and 0.29 M votes.
