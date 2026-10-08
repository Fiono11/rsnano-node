# Certificate lookups of a report check in chunks (2026-10-08)

Timing every AEC lock hold of 50 ms or more (temporary instrumentation, two
nofork runs on ce2444644) showed the largest remaining holder:
`AecService::certificate_kinds`, the report check's lookup of every
non-inherited entry of an epoch (~16k), one read-lock hold of up to 217 ms,
18-34 holds per run, all during epoch 1's close (16.6-17.8 s), right before
the slowest client second of the close. Other holders: `apply_vote` write
holds of 80-143 ms when a vote triggers the epoch switch or the install, and
`evidence_manifest` reads of 78-106 ms.

The lookups now take the lock per chunk of 1,024 hashes. A later chunk can
only see more evidence: a committee once known stays known, certificates are
never withdrawn.

Nofork, interleaved (`ck-*` = the change, `h3-*` = ce2444644):

| run | cps | client p50/p95/p99 ms | close median/max ms | slow seconds (avg > 150 ms) |
|---|---|---|---|---|
| ck-1 | 1957 | 103 / 251 / 312 | 1480 / 2207 | 2s:197 10s:153 19s:177 |
| h3-1 | 1948 | 101 / 317 / 531 | 1330 / 2483 | 2s:248 17s:328 19s:167 |
| ck-2 | 1956 | 100 / 366 / 1159 | 1096 / 2231 | 2s:191 20s:483 |
| h3-2 | 1940 | 102 / 259 / 539 | 1857 / 2771 | 9s:198 10s:152 17s:223 19s:337 |

p95/p99 differ either way between pairs. The slow second inside epoch 1's
close (17 s) was absent in both `ck` runs above, but two later runs of the
same build (`rerun/`, the baseline arm of the next A/B) show it again:

| run | cps | client p50/p95/p99 ms | close median/max ms | slow seconds (avg > 150 ms) |
|---|---|---|---|---|
| rerun h4-1 | 1950 | 100 / 229 / 406 | 1632 / 2582 | 2s:153 17s:221 19s:237 |
| rerun h4-2 | 1947 | 100 / 232 / 362 | 1483 / 2625 | 2s:167 17s:251 19s:168 |

So the change shortens the lock holds as measured, but no latency effect is
established; its absence in the first pair was chance.
