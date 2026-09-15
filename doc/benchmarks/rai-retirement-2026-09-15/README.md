# RAI election retirement, passive certificate collection, multi-base reconciliation

Branch `rai_close_epochs` on top of 3629c711d, 2026-09-15. Six PRs, `--no-prio`,
5% forks, 50k blocks and accounts, count-based epochs
(`--epoch-terminated-elections 25000 --closed-epochs 2 --close-timeout 240`).
`run_one.sh <bindir> <name> <rate>` runs one benchmark; `table.py` and
`summarize.py` read the saved logs and PR0 counters. The baseline binaries were
built from a worktree at 3629c711d.

## Changes measured

1. Unfinalizable elections (timeout certificate, or two notarized candidates) are
   retired from the AEC two base latencies after becoming unfinalizable. Their
   tallies move to the certificate recovery map and candidate bodies are kept, so
   late notarizations still extend epoch membership. Retired roots cannot be
   re-activated in their epoch, and roots decided at a close are remembered in the
   recently confirmed cache so the backlog scan skips them cheaply.
2. Unfinalizable elections are not solicited while their epoch is open; the closer
   reconciles what is still missing at the drain. Without epochs, one bounded batch
   of retired roots is requested every 30 s.
3. Blind drain-time solicitation of notarized, fork and retired elections runs only
   while a proposal has no delta page flow; a reconstructed snapshot is reconciled
   by naming exactly the missing members. A zero-root reconciliation request now
   makes the peer publish the block, since the requester does not hold it.
4. Reconciliation requests advertise up to eight snapshot digests (the local
   history, newest first) and the peer serves the largest shared subset base.

Two variants were rejected on the way: a one-shot request per retired election and
unconditional solicitation of retired roots during the drain. Both amplified
replies and publish drops (`requests_generated_hashes` up to 5x, drops up to 4x)
and made epoch 1 slower at 2000 blocks/s.

## Results (non-fork finalization, ms, mean/p95 per epoch; PR0 counters)

`close_after_workload_s` is the time from BENCHMARK_RESULT to EPOCH_CLOSE_RESULT;
`None` means the close did not converge within 240 s (exit 1).

    run          exit cps  | e0 fin mean/p95 | e1 fin mean/p95 | close e0 rnd | close e1 rnd | close_after_workload_s | pr0 started retired gen_hashes conf_req drop_pub drop_ack
    base-1500    0     792 |   107/  187 |   154/  430 |   1 |   1 |      4 |  50200     0   26442   2572   2978   3054
    base2-1500   0     790 |   103/  136 |   140/  226 |   2 |   2 |      4 |  50223     0   30412   2679   5555   5189
    new7a-1500   0     792 |   100/  132 |   132/  253 |   1 |   2 |      0 |  50210  1329   18846   2412   4452   3452
    new8a-1500   0     789 |   107/  187 |   130/  290 |   1 |   1 |      0 |  50411  1510   19312   2686   1858   2408
    base-2000    1     791 |   203/  434 |   890/ 1823 |   2 |  -1 |   None |  50318     0  252409  13686  61737  41605
    base3-2000   0     790 |   172/  387 |   786/ 1357 |   2 |   2 |      0 |  50270     0   31318   2800   5070   3540
    prof-base-2000 0     791 |   332/  687 |  1031/ 2233 |   2 |   2 |      0 |  50319     0   22588   2765   3172   1934
    base4-2000   0     791 |   193/  365 |  1488/ 2682 |   1 |   3 |      0 |  50125     0   35181   2822   5515   5097
    base5-2000   0     790 |   187/  392 |   876/ 1629 |   2 |   1 |      0 |  50377     0   20552   2964   2592    982
    new8a-2000   0     790 |   210/  509 |  1610/ 3268 |   1 |   2 |     23 |  51000  2156   27019   4452   5772   2424
    new8b-2000   0     788 |   253/  479 |   912/ 1827 |   2 |   1 |      0 |  50954  1962   23578   2614   5400   2473
    new8c-2000   0     792 |   266/  524 |  1457/ 2430 |   1 |   2 |      0 |  50616  1543   25493   2512   5894   2242
    new8d-2000   0     791 |   183/  346 |   865/ 1297 |   2 |   0 |      0 |  50865  1887   24111   2502   9227   2512

Summary: at 1500 blocks/s epoch 0 is unchanged and epoch 1 improves (130-132 vs
140-154 ms) with fewer generated reply hashes (19k vs 26-30k) and fewer publish
drops. At 2000 blocks/s the host is saturated; epoch-1 means are 1014 ms
(baseline, five runs) vs 1211 ms (new, four runs) with overlapping ranges
(786-1488 vs 865-1610). The new build closed both epochs in every run; the
baseline failed to converge once in five at 2000.

## Isolation: reply fix + changes 3 and 4 only (no retirement, no passive collection)

Built from 3629c711d with only `messages/src/epoch_close.rs`, `epoch_closer.rs` and the
zero-root line in `vote_generators.rs` applied (`variant.patch` in the session scratchpad).

    vara-1500    0     790 |   108/  190 |   161/  266 |   3 |   4 |     27 |  50194     0   65868   4756  13220  17496
    varb-1500    0     790 |   217/  565 |   128/  230 |   1 |   2 |      0 |  50153     0   23611   2653   3188   4388
    vara-2000    0     791 |   132/  244 |   608/ 1299 |   2 |   1 |      0 |  50201     0   20744   2638   2710   1955
    varb-2000    0     791 |   158/  279 |   816/ 1403 |   1 |   3 |     28 |  50453     0   40232   3214   7884   5626

Epoch-1 means: 1500/s baseline 147, full build 131, variant 145; 2000/s baseline 1014,
full build 1211, variant 712. Epoch-0 means at 2000/s: baseline 217, full 228, variant 145.
Retirement and passive collection add nothing the variant does not already deliver,
and cost at 2000/s. Committed: the reply fix, 3 and 4. Changes 1 and 2 were dropped;
the description above is kept as the record of what was measured.
