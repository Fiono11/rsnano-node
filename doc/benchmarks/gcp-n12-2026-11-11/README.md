# Twelve seats across six regions, profiling and lock fixes (Nov 11 2026)

Branch `rai_dsn_merge`. Fourteen `n2-standard-4` Spot VMs: the six paper regions twice
(Warsaw, Iowa, Montréal, Las Vegas, Taiwan, Melbourne) and two Toronto standbys
(`hosts-n14.txt`); twelve seats at f = p = 2 (certificate 8, fast path 10, reports 10).
Every run: no forks, 45,000 blocks at 2,000 blocks/s, seed 2000655 (the matrix's `nofork`
seed), `run_gate_b.py --prs 12 --committee-f 2 --committee-p 2 --committee-seats 12`.

| Run | Build | Status | Blocks/s | p50 | p95 | p99 | Close median / max |
|---|---|---|---|---|---|---|---|
| matrix `nofork/r2000/rai` | c4390d33f | settled | 475 | 813 ms | 31.7 s | 37.8 s | 9.1 / 107 s |
| matrix `nofork/r2000/develop` (Nano) | develop-fork-memo | settled | 1,937 | 523 ms | 579 ms | 879 ms | – |
| knee `nofork-1000` (1,000/s) | c4390d33f | settled | 984 | 428 ms | 593 ms | 708 ms | 3.2 / 4.9 s |
| `lock1` | 829e33964 (lock profiler, not forwarded) | failed* | 1,552 | 1,035 ms | 4.3 s | 6.3 s | 17.7 / 59 s |
| `lock2` | 13828d8c8 (lock profile on) | settled | 434 | 906 ms | 20.9 s | 27.4 s | 9.7 / 101 s |
| `opt1` | f1cf174de (chunked solicitor scan, per-request lookups) | settled | 1,904 | 524 ms | 1,088 ms | 1,271 ms | 4.1 / 5.1 s |
| `opt2` | 1b8e3e391 (+ reconciliation streams start at 64 and double) | settled | 1,912 | 455 ms | 979 ms | 1,111 ms | 3.5 / 4.7 s |
| `origin1` (Nov 12) | 51e4683fc (+ every node starts the epochs from one wall-clock origin) | settled | 1,946 | 425 ms | 764 ms | 949 ms | 2.3 / 3.8 s |

\* `lock1` failed the harness's old check only: validators finalized epoch 28's checkpoint in
round 0 and its round-1 child, one state under two value hashes; the harness now compares
decided states (13828d8c8). `opt1b` (a repeat of opt1 without the profiler) failed in setup:
the Spot VM of the second Montréal node was preempted at 20:53:51 UTC.

**What limited RAI (lock2, lagging Iowa validator):** the elections lock was held about 175 s
of the run while the CPU was about 60 % busy: vote application (223,000 times, 99 s), the
solicitor's scan of every open election in one read hold (25 s, holds up to 269 ms), the voter
scan (25 s), and 5.6 M per-hash lock takes answering vote requests. Validators fell behind,
dropped up to 1.2 M votes, and closes took up to 101 s. After the fixes vote applications fell
to about 11,000 per validator (less solicitation, fewer repeated votes) and almost no vote was
dropped.

**Common epoch origin (origin1, Nov 12):** the client used to start the epochs one PR after the other, each PR setting its origin on arrival, so the last PR's boundaries lagged by the round trips before it: validators ended each epoch 0.6 to 0.7 s apart. With one origin they end it within 0.1 s, the close from first leave to last decision falls from 3.5 to 2.3 s at the median, and the 95th percentile from 979 to 764 ms (Nano 579). This makes the benchmark match the protocol's synchronized boundaries; a deployment would configure its genesis time.

**What remains (opt2):** between epoch boundaries RAI's median is 384 to 400 ms, below Nano's
515 to 533 ms; for 3 to 4 s after each boundary it rises to 700 to 1,000 ms until the checkpoint
is installed, which sets the 95th percentile. Close phases at the median: reconcile 1.5 s (2.0 s
before the stream change), agree 1.2 s, check 0.16 s.

Files: `*/result.json.gz` per run, `pr*-aec-lock.txt.gz` the `AEC_LOCK` lines (lock profile:
`line:r|w:count:hold_ms:max_hold_ms:wait_ms:max_wait_ms` of `aec_service.rs`, plus timed
`av_*` sections of `apply_vote`), `runs/perf-<run>-<ip>.txt` 10 s per-thread CPU profiles of
Warsaw (10.186.0.3), Iowa (10.128.0.3) and Melbourne (10.192.0.2) taken 12 s into the load,
`runs/close-<run>.dat` close breakdowns (`RAI_LaTeX/tools/close_breakdown.py --runs`).
One run per row: the effect of each fix has not been repeated, and the fork and Byzantine cells
were not run on the fixed builds.
