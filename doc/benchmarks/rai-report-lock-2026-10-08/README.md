# Report verification off the exchange lock (2026-10-08)

`ReportService::verify_reports` held the `exchange` mutex while it checked
every entry of every reconstructed report against the AEC's certificates.
Report and symbol messages are handled inline on the network message
threads and need the same mutex, so votes and blocks queued behind them
(profile: ~30k lock-wait samples per 30 s on the message threads).

Change (all behavior-preserving):

- `ReportExchange::verify` becomes `verification_jobs` (under the lock),
  `VerifyJob::check` (without it) and `apply_verifications` (under the lock,
  dropping a job whose report was reconstructed again or checked meanwhile);
  reconstructed states and residuals are `Arc`s so a job holds no copy.
- `unjustified` looks up certificates only for entries the predecessor
  checkpoint does not justify (an inherited entry is justified whatever its
  certificates); one lookup still yields None while the committee is unknown.
- `only` and the listed missing hashes are hash sets instead of linear scans.

## before-fix (on 16f67221c, before the released-epoch fix 02f4a2c05)

Client p95 was worse in all three runs with the change (783-956 ms vs
297-652 ms); the low-rate profile showed message-thread lock waits halved
(410 vs 892 samples at 10 ms) and AEC ticker CPU -45%. The tail regression
came from the post-install dip, which turned out to be a released epoch
brought back by repeated reports (fixed in 02f4a2c05).

## on-fix (on 02f4a2c05), nofork, interleaved

`pair` = item2/head x2 (item2-1 failed in nanospam's setup, before any epoch:
one PR never cemented the 65th setup block); `split` = cpu-only/head/item2 x2,
`cpu` = the `unjustified` part alone.

| arm | runs | client p95 ms | client p99 ms | close max ms | inbound overfill |
|---|---|---|---|---|---|
| head | 4 | 311, 359, 351, 384 | 500, 484, 560, 597 | 2667, 3538, 2363, 3571 | 430, 60, 656, 112 |
| item2 | 3 | 480, 260, 309 | 956, 426, 624 | 2422, 2447, 2432 | 48, 85, 41 |
| cpu | 2 | 260, 360 | 512, 706 | 2916, 2345 | 218, 37 |

Latency neutral within run-to-run noise; fewer inbound message drops and a
lower worst close.
