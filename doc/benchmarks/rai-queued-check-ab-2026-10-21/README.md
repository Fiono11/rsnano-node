# A/B: a close round waits for its proposal from the moment it is queued

f = p = 1, fork10, one equivocating Byzantine node, 2000 blocks/s, 48k blocks, 8 s epochs.
A = c722b5b02 (the paper matrices' binary), B = c534df908 (the fix). Same seeds for both, order
alternating per seed. Stopped after 10 runs: B ahead on 4 of 5 seed pairs.

| seed | A goodput/s | A p95 s | B goodput/s | B p95 s |
|---|---|---|---|---|
| 2001090 | 1327 | 6.9 | 1443 | 4.7 |
| 2001091 | 1356 | 5.7 | 1319 | 7.6 |
| 2001092 | 1265 | 9.1 | 1493 | 3.6 |
| 2001093 | 1237 | 9.2 | 1497 | 3.5 |
| 2001094 | 825 | 21.8 | 1379 | 6.1 |
