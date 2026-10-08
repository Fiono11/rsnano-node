# Bounded-weight committees: smoke runs (2026-10-08)

Committee model `bounded_weight` (f = p = 1, drift 100 permille): the six
genesis members stay for the whole run, and each epoch's committee re-weights
them by the balance delegated to each, held within 1000 ± 100 units per
member. Thresholds discount the heaviest members, so any 4 members certify and
any 5 carry a report, as with equal weights.

Workload as run7 of `rai-paper-2026-10-07`: 45,000 blocks at 2,000/s, 45,000
accounts, 8 s epochs, quiet host.

```
python3 tools/rai/run_gate_b.py <dir> --bounded-drift 100 --wait-quiet 600 --timeout 150 --settle-timeout 90 [--offline 1]
python3 tools/rai/check_committees.py <dir>/result.json
```

| variant | status | committees | cps | node p50 ms | client p50/p95/p99 ms | close median/max ms |
|---|---|---|---|---|---|---|
| nofork (bounded) | settled | consistent, 4 digests | 1903 | 95-99 | 121 / 616 / 1044 | 2147 / 4904 |
| nofork (run7, equal) | settled | consistent, 1 digest | 1945 | 88-89 | 104 / 305 / 519 | 1475 / 2650 |
| nofork-offline1 (bounded) | settled | consistent, unchanged | 1951 | 101-103 | 112 / 265 / 380 | 1772 / 3611 |
| nofork-offline1 (run7, equal) | settled | consistent | 1969 | 100-102 | 110 / 176 / 270 | 1432 / 3858 |

Weights per epoch (nofork): genesis all 1000; C(0) one member 999; C(1) and
C(2) one member 1001. In nofork-offline1 every committee stayed at 1000.

**The workload barely moves weight.** Each PR wallet is funded with about
56.5M nano delegated to itself, while the spam circulates only nanospam's
1M nano `INITIAL_AMOUNT`, so at most about 0.3% of the weight can move: one unit
of 1000. The runs show that derivation, digest agreement and joint closes work
with bounded weights. A run that changes weights visibly needs a workload that
moves weight between the representatives.

One run per variant; the tail latency and close-time differences against run7
are within what single runs have varied by before and are not attributed.
