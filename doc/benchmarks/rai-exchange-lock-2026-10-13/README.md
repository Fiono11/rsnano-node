# Proposal checks off the report exchange lock, Oct 13 2026

Arms (RAI, Byzantine validator leading every close, 2,000 blocks/s, 45,000
blocks, `--fork-grace 120`, seeds 2007924/2015843 (5%) and 2007929/2015848
(10%), arms alternating):

- a: 1df3a92b8 (checks hold the report exchange for their whole run)
- b: checks copy the selected reports out (`OwnedReports`) and release the
  exchange; a `checking` mutex keeps them one at a time, in arrival order
- b2: b, plus proposals checked on their own thread ("Epoch proposals"),
  queued once per value; network threads no longer wait for a check

Round 1 (a vs b, top level) and round 2 (a vs b2, `round2/`): all 16 runs
settled, closes needed at most 2 rounds, every fork resolved; the Oct 8
failure of releasing the lock (0952a6f7a: one check at a time with dropped
copies, 117 close rounds) does not recur.

| variant | arm | p95 s | overflow | late validations | close max s |
|---|---|---|---|---|---|
| fork10-byzlead | a (r2) | 4.8, 3.8 | 4,078, 4,374 | 5, 5 | 11.7, 12.4 |
| fork10-byzlead | b2 | 3.6, 3.2 | 3,740, 2,299 | 0, 0 | 14.2, 11.6 |
| fork5-byzlead | a (r2) | 1.9, 1.8 | 1,332, 2,479 | 4, 4 | 9.7, 11.0 |
| fork5-byzlead | b2 | 2.8, 2.3 | 1,374, 1,662 | 0, 4 | 10.5, 10.0 |

b vs a (round 1) was within run-to-run noise. a's own fork5 p95 ranged
1.8-2.5 s across the rounds, so two seeds per arm do not resolve the fork5
difference. `profile-b/`: one sampled b run; network-thread lock waits fell
to about a quarter of an earlier (non-Byzantine) profile, the rest being
proposal copies queued on the check, which b2 removes.
