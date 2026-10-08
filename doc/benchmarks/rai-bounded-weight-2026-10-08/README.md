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

## With weight shifts

nanospam `--weight-shift-percent 5` (run_gate_b.py `--weight-shift 5`): at the
start of the spam and every epoch after, PR(k mod 6) sends 5% of its balance
(2.83M nano) to a fresh account delegating to PR(k+1 mod 6). The holder
account belongs to no wallet, so no node receives into it on its own. The
target's own balance does not grow, so each source sends about the same
amount, and the source of shift 0 (PR0) stays low until the shifts wrap
around to it.

```
python3 tools/rai/run_gate_b.py <dir> --bounded-drift 100 --weight-shift 5 --wait-quiet 600 --timeout 150 --settle-timeout 90 [--offline 1]
```

Committees (nofork; offline1 the same within one unit), by PR:

| derived by | PR0 | PR1 | PR2 | PR3 | PR4 | PR5 |
|---|---|---|---|---|---|---|
| genesis | 1000 | 1000 | 1000 | 1000 | 1000 | 1000 |
| 0 | 951 | 1050 | 1000 | 1000 | 1000 | 1000 |
| 1 | 951 | 1000 | 1050 | 1000 | 1000 | 1000 |
| 2 | 951 | 1000 | 1000 | 1050 | 1000 | 1000 |

Every node held the same digest for every epoch (`check_committees.py`).

## Performance, same binary and day

| variant | cps | node p50 ms | client p50/p95/p99 ms | close median/max ms |
|---|---|---|---|---|
| nofork, equal weight (today) | 1905 | 95-99 | 124 / 530 / 802 | 2388 / 2580 |
| nofork, bounded | 1903 | 95-99 | 121 / 616 / 1044 | 2147 / 4904 |
| nofork, bounded + shift 5 | 1923 | 97-101 | 123 / 712 / 1305 | 1427 / 4647 |
| nofork-offline1, bounded | 1951 | 101-103 | 112 / 265 / 380 | 1772 / 3611 |
| nofork-offline1, bounded + shift 5 | 1960 | 99-101 | 112 / 218 / 337 | 1671 / 3843 |

Most of the gap to run7 (p95 305 ms) is also in today's equal-weight run, so it
comes from the host or the day, not the model. The bounded nofork runs have
somewhat higher p95/p99 and a slower worst close (4.6-4.9 s vs 2.6 s) than the
equal-weight run; with one run each this is not attributed.
