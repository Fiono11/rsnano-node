# epoch_certified built outside the AEC lock (2026-10-08)

`AecService::epoch_certified` held the AEC read lock while it cloned the
epoch's inherited base, certified every finalized instance and every live
certificate (a Blake2 digest and a tree insert per entry, ~31k cumulative
entries by epoch 1) and projected the final prefixes: 50-220 ms per call on
every report tick while a reporter is unusable. Vote application takes the
write lock and waited behind it.

Now `epoch_certified_parts` collects the inputs under the lock (the base
`Arc`, the finalized instances with their epoch flag, the live certificates,
in the order they are applied) and `EpochCertifiedParts::build` does the
rest after the lock is released. The result is identical: the scan yields one
instance per hash, so "an older instance only if the state holds its hash"
is "only if the inherited base holds it".

Nofork, equal weight, interleaved (`ec-*` = the change on 3a2314d1d, `h2-*` =
3a2314d1d):

| pair | arm | cps | client p50/p95/p99 ms | close median/max ms | inbound overfill |
|---|---|---|---|---|---|
| 1 | ec | 1932 | 102 / 243 / 490 | 1254 / 2405 | 9 |
| 1 | h2 | 1941 | 105 / 308 / 860 | 1132 / 2085 | 74 |
| 2 | ec | 1942 | 103 / 523 / 1036 | 1586 / 2976 | 44 |
| 2 | h2 | 1941 | 104 / 687 / 1538 | 1045 / 2981 | 1487 |

The second pair ran slower for both arms (host); within each pair the tails
and message drops are lower with the change.
