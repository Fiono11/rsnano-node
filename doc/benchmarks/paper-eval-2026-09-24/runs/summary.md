| Variant | Arm | Runs ok/total | Goodput (blocks/s) | p50 (ms) | p95 (ms) | p99 (ms) | Pooled p50/p95/p99 (ms) | Forks unresolved |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| nofork | develop | 1/1 | 1901 [1901-1901] | 189 [189-189] | 410 [410-410] | 622 [622-622] | 189/410/622 | 0 [0-0] |
| nofork | rai | 1/1 | 1930 [1930-1930] | 100 [100-100] | 169 [169-169] | 271 [271-271] | 100/169/271 | 0 [0-0] |
| nofork-offline1 | develop | 1/1 | 1907 [1907-1907] | 207 [207-207] | 238 [238-238] | 285 [285-285] | 207/238/285 | 0 [0-0] |
| nofork-offline1 | rai | 1/1 | 1958 [1958-1958] | 110 [110-110] | 141 [141-141] | 225 [225-225] | 110/141/225 | 0 [0-0] |
| nofork-byz1 | develop | 1/1 | 1921 [1921-1921] | 205 [205-205] | 234 [234-234] | 289 [289-289] | 205/234/289 | 0 [0-0] |
| nofork-byz1 | rai | 1/1 | 1963 [1963-1963] | 106 [106-106] | 138 [138-138] | 191 [191-191] | 106/138/191 | 0 [0-0] |
| fork5 | develop | 0/1 | n/a | n/a | n/a | n/a | None/None/None | n/a |
| fork5 | rai | 1/1 | 1579 [1579-1579] | 129 [129-129] | 763 [763-763] | 1198 [1198-1198] | 129/763/1198 | 3 [3-3] |
| fork10 | develop | 0/1 | n/a | n/a | n/a | n/a | None/None/None | n/a |
| fork10 | rai | 1/1 | 1372 [1372-1372] | 502 [502-502] | 4167 [4167-4167] | 6154 [6154-6154] | 502/4167/6154 | 692 [692-692] |
| fork5-offline1 | develop | 0/1 | n/a | n/a | n/a | n/a | None/None/None | n/a |
| fork5-offline1 | rai | 1/1 | 1829 [1829-1829] | 114 [114-114] | 307 [307-307] | 458 [458-458] | 114/307/458 | 721 [721-721] |
| fork10-offline1 | develop | 0/1 | n/a | n/a | n/a | n/a | None/None/None | n/a |
| fork10-offline1 | rai | 1/1 | 1200 [1200-1200] | 145 [145-145] | 698 [698-698] | 951 [951-951] | 145/698/951 | 209 [209-209] |
| fork5-byz1 | develop | 0/1 | n/a | n/a | n/a | n/a | None/None/None | n/a |
| fork5-byz1 | rai | 1/1 | 1820 [1820-1820] | 119 [119-119] | 459 [459-459] | 854 [854-854] | 119/459/854 | 691 [691-691] |
| fork10-byz1 | develop | 0/1 | n/a | n/a | n/a | n/a | None/None/None | n/a |
| fork10-byz1 | rai | 0/1 | n/a | n/a | n/a | n/a | None/None/None | n/a |

Runs that did not finish: all confirmed blocks (forks included) out of 45,000 at the timeout,
and the second after the first confirmation from which confirmations stayed below 200/s.

| Variant | Arm | Pair | Publishing timeout (s) | Confirmed | Share | Stall onset (s) |
|---|---|---:|---:|---:|---:|---:|
| fork5 | develop | 0 | 41 | 42,927 | 95.4% | 38 |
| fork10 | develop | 0 | 41 | 39,271 | 87.3% | none |
| fork5-offline1 | develop | 0 | 36 | 40,700 | 90.4% | 33 |
| fork10-offline1 | develop | 0 | 51 | 31,144 | 69.2% | 50 |
| fork5-byz1 | develop | 0 | 36 | 41,837 | 93.0% | 28 |
| fork10-byz1 | develop | 0 | 51 | 38,890 | 86.4% | none |
