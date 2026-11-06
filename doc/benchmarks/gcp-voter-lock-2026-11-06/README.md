# Voter scan and the elections lock, Nov 6 2026

Hardest cell of the six-region matrix: 10% forks, Byzantine member, handover (PR2 to PR6), 2000/s x 45k, 8 s epochs, seven GCP regions.
Same five seeds on both builds, which share the equal-weight client change (node sha db1c1b4c before, 8a7c1763 after).
"before" holds the elections read lock for the whole voter scan; "after" holds it for 64 elections at a time.
A run's `status` is what the harness recorded; "before" seed 2001092 did not settle.

| build | seed | status | goodput (blocks/s) | p50 (ms) | p95 (ms) | slowest close (ms) | peak requests |
|---|---|---|---|---|---|---|---|
| before | 2001090 | settled | 1716.0 | 415 | 646 | 42282 | 259764 |
| before | 2001091 | settled | 1715.3 | 425 | 640 | 44508 | 333456 |
| before | 2001092 | failed | - | - | - | 96395 | 3155883 |
| before | 2001093 | settled | 1568.8 | 411 | 953 | 157772 | 1279654 |
| before | 2001094 | settled | 1581.3 | 419 | 905 | 124726 | 2444936 |
| after | 2001090 | settled | 1743.2 | 404 | 821 | 10692 | 57590 |
| after | 2001091 | settled | 1754.9 | 455 | 1537 | 7784 | 55024 |
| after | 2001092 | settled | 1741.6 | 404 | 775 | 9161 | 56366 |
| after | 2001093 | settled | 1737.2 | 400 | 702 | 8653 | 51465 |
| after | 2001094 | settled | 1730.6 | 404 | 906 | 15422 | 64624 |
