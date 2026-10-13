# A node falling behind at saturation, Oct 13 2026

RAI, 10% forks, 2,000 blocks/s, 45,000 blocks, `--fork-grace 120`, seeds
2001134, 2009053, 2016972 (rep 1-3). Arms:

- b2: fd419c436 (proposal checks off the exchange lock)
- c: b2 + evidence blocks in the inbound priority lane + finalized
  checkpoint blocks re-requested every second (no backoff)
- c2: c + a close-vote lane in the vote processor + close votes replayed
  from the vote cache when a committee is derived (`AecFact::CommitteeDerived`)

Mechanism (logs of the failing runs): with 10% forks every node lacks
about half of the fork blocks a checkpoint finalizes. Deriving the committee
from that checkpoint waits for them, and the close of epoch e counts in the
committees of e and e+1, so every close vote is `Indeterminate` meanwhile and
waits in the vote cache, where nothing replayed it when the committee came.
The node decides late, its next report waits for that decision, its live
view has no base and reconciles full sets (60k-symbol streams), and its
vote processor, saturated by account votes (250k drops), drops the close
votes of the next round too: each epoch it falls further behind.

| seed | b2 | c | c2 |
|---|---|---|---|
| 1 | failed, node 3 epochs behind, p95 5.8 s | failed, 1 epoch behind, p95 7.1 s | settled, p95 4.9 s |
| 2 | settled, p95 3.3 s | (Mac slept 74 s: `c-rep2.slept`) | settled, p95 3.0 s |
| 3 | settled, p95 3.0 s | - | settled, p95 2.9 s |

Longest committee wait for frontier blocks, seed 1: b2 16.2 s, c 7.4 s,
c2 8.1 s; in c2 the fetch drains steadily (910 -> 1 missing accounts in
7 s) instead of stalling on 4-8 s backoff tails, and a waiting node catches
up within the close. Byzantine-leader check (`fork*-byzlead/c2-rep1`, seeds
of round 2 rep 1 in rai-exchange-lock-2026-10-13): fork10 p95 3.4 s (a 4.8,
b2 3.6), fork5 2.3 s (a 1.9, b2 2.8); both settled, no late validations.
