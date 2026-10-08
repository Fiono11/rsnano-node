# Vote generator delay A/B (2026-10-08)

`vote_generator_delay` (node default 100 ms) is how long a vote generator
collects hashes before it sends a vote that is not full (255 hashes). At
2,000 blocks/s a 100 ms window holds about 200 hashes, so first and final
votes each wait for the timer. Equal weight, nofork, 45,000 blocks at
2,000/s, 8 s epochs, same binary, arms interleaved:

```
python3 tools/rai/run_gate_b.py <dir> --vote-delay-ms 20|100 --wait-quiet 600 --timeout 150 --settle-timeout 90
```

| run | delay | cps | client p50/p95/p99 ms | node p50 ms | close median/max ms | confirm_ack out/in | msg overfill | rebroadcast overfill |
|---|---|---|---|---|---|---|---|---|
| d20-1 | 20 | 1871 | 50 / 390 / 556 | 36-38 | 1619 / 5204 | 107k / 66k | 1,359 | 184k |
| d100-1 | 100 | 1870 | 134 / 408 / 587 | 98-102 | 1544 / 3678 | 35k / 22k | 839 | 109k |
| d20-2 | 20 | 1824 | 138 / 879 / 1155 | 49-61 | 2467 / 7979 (epoch 1: round 1) | 98k / 60k | 2,353 | 250k |
| d100-2 | 100 | 1907 | 123 / 652 / 934 | 91-98 | 2353 / 2794 | 35k / 22k | 1,007 | 94k |

20 ms lowers node finalization p50 by about 50 ms in both pairs, but triples
the vote messages and raises inbound message drops; the client median
followed in the first pair only, and the second 20 ms run needed a second
close round in epoch 1. Not adopted: the message-thread blocking on the
report exchange lock and the rebroadcast load come first, then a retest.
