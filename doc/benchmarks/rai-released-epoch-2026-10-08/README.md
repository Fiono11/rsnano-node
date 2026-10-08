# Released epoch brought back by repeated reports (2026-10-08)

**Symptom.** After the checkpoint of epoch 1 installed, the client saw a
0.5-1.5 s dip (PR0 cementing down to ~200 blocks/s, then a burst; PR0's AEC
fact queue spiking to ~4.7k events), and the tail latencies of nofork runs
came from that window.

**Cause.** `ReportExchange::handle_report` and `refresh_live` create an
epoch's entry on demand (`entry().or_default()`). Replicas that have not
released an epoch yet keep repeating their reports of it every second, so on
a node that had already released the epoch a repeated report recreated it,
with a report never reconstructed. From then on every 300 ms report tick
treated a reporter as unusable: it rebuilt the epoch's live certified state
(`epoch_certified`, ~31k cumulative entries, 50-290 ms under the AEC read
lock), reconciled and verified - for an epoch decided, installed and
released here. Vote application takes the AEC write lock, so it stalled
behind those holds. Timing diagnostics (logged at >= 50 ms) counted 29-87
such holds after the node's own release, 3.4-9.3 s of read lock per run.

**Fix.** The exchange remembers the epochs it released (the newest 64) and
ignores reports and live refreshes for them, as `a_released_epoch_holds_no_reports`
already states ("nothing of the epoch is usable or served afterwards").

**A/B** (nofork, equal weight, 45,000 blocks at 2,000/s, 8 s epochs,
interleaved; both arms carried the temporary timing diagnostics, which only
log calls over 50 ms; `fix-*` = the fix, `instr-*` = HEAD 16f67221c):

| run | cps | client p50/p95/p99 ms | node p50 ms | close median/max ms | msg overfill | epoch_certified holds after own release |
|---|---|---|---|---|---|---|
| fix-1 | 1949 | 101 / 210 / 307 | 85-87 | 1038 / 2436 | 7 | 0 |
| instr-1 | 1933 | 110 / 377 / 721 | 91-93 | 1423 / 3585 | 647 | 33 (3.4 s) |
| fix-2 | 1950 | 100 / 271 / 453 | 89-91 | 1116 / 2222 | 19 | 0 |
| instr-2 | 1933 | 111 / 383 / 542 | 95-97 | 1372 / 3607 | 152 | 87 (9.3 s) |

`result.json` records the HEAD revision for both arms; the fix arm was the
working tree with this change.
