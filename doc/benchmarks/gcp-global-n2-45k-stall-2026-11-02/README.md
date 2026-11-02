# 2000/s for 45,000 blocks across six regions: the closes fall behind (Nov 2 2026)

One run, `--rate 2000 --blocks 45000 --accounts 45000`, no forks, on the 2-vCPU
six-region fleet of `../gcp-global-n2-2026-11-02` (built at 93331b36b). It did
not settle: aborted by hand after ~15 min at 44,478 of 45,000 confirmed.

There is no `result.json`: the harness was stuck in its settle polls and
ignored SIGINT (a background job of a non-interactive shell inherits SIGINT
ignored; fixed in run_gate_b.py afterwards). The record was salvaged from the
controller while the nodes still ran:

- `run.log.gz` nanospam + node diagnostics as the harness saw them
- `pr<i>/diagnostics.log.gz`, `pr<i>/config-*` every node's own log and config
- `postmortem/` RPC snapshot ~10 min into the stall: `block_count`,
  `final_state`, `confirmation_active`, `stats counters` per node

## What happened

The first ~12 s ran at ~2000 cps, avg confirmation ~380-440 ms. Then
throughput fell to bursts and the average climbed to seconds, then past 90 s.
The 15k-block runs at 1500/2000 (7.5-10 s of load) ended before this point,
so their good tails are not representative of sustained load.

Each close took longer than the one before; from epoch 2 on, longer than the
8 s epoch (times in s after the load started, first / last node):

| Epoch | Ended | Closed | Close took |
|---|---|---|---|
| 1 | 6.3 / 6.8 | 9.2 / 9.5 | ~3 s |
| 2 | 6.9 / 11.9 | 16.2 / 17.7 | ~5-10 s |
| 3 | 14.3 / 15.2 | 25.0 / 41.3 | ~10-26 s |
| 4 | 22.3 / 23.2 | 138 / 166 | ~2 min |
| 5 | 30.4 / 38.7 | 672 / 686 | ~11 min |

At the snapshot the ledgers differed (blocks 45,040-45,065, cemented
44,100-44,534) and every node was overloaded with messages; the same counters
were zero in every 15k-block run:

| Counter (max over nodes) | 45k run | 15k runs |
|---|---|---|
| drop confirm_ack out | 13.6 M | 0 |
| request_aggregator overfill_hashes | 45 M | 0 |
| vote_processor overfill | 1.4 M | 0 |
| drop publish out | 439 k | 0.6-2.1 k |
| block_processor overfill (live) | 23 k | 0 |
| message_processor_overfill report / epoch_prop | 1,164 / 1,399 | 0 |

Reading (not yet verified): the closes stop keeping pace with the epochs;
blocks of unclosed epochs stay open, their elections keep soliciting votes,
the vote traffic overflows the queues, and the dropped close messages
(reports, proposals) slow the next close further. Some blocks are lost on
every path to a node (the differing ledgers), which an epoch close then
needs. What turns the first 3 s close into 10 s is the open question; the
node diagnostics around +6 to +17 s should show it.
