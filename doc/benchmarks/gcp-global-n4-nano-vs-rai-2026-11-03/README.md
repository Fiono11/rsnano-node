# Nano vs RAI, 45,000 blocks across six regions on 4-vCPU nodes (Nov 3 2026)

Same six-region n2-standard-4 fleet as `../gcp-global-n4-45k-2026-11-03`, no
forks, `--blocks 45000 --accounts 45000 --timeout 600`.

- **Nano** = the unmodified develop node (`develop-fork-memo` afe63ab1c, node
  sha256 478d1750…), `--no-epochs --bin-dir ~/bin-develop`, the same
  nanospam as the RAI runs.
- **RAI fixed** = rai_multihost a5e3c7cfe (epoch boundary fix), 8 s epochs,
  equal-weight committee f=1 p=1.

| | Nano 2000/s | Nano 1500/s | RAI fixed 2000/s | RAI before the fix 2000/s (old client) |
|---|---|---|---|---|
| Run | `n4c-nano-nofork-2000-45k` | `n4c-nano-nofork-1500-45k` | `n4d-rai-nofork-2000-45k` | `../gcp-global-n4-45k-2026-11-03/n4-nofork-2000-45k` |
| Settled | yes | yes | yes | yes |
| Goodput | 1632 cps | 1288 cps | 1789 cps | 1775 cps |
| p50 | 526 ms | 527 ms | **373 ms** | 405 ms |
| p95 | 587 ms | 583 ms | **450 ms** | 2.41 s |
| p99 | 605 ms | 605 ms | **530 ms** | 3.32 s |
| Close median / max | - | - | 3.2 / 3.9 s | 3.5 / 6.6 s |

With the fix RAI is faster than Nano at every percentile at 2000/s.

## Why RAI's tail was worse (fixed in a5e3c7cfe)

Each replica's epoch origin is the moment the start reached it; across six
regions the origins differ by up to 0.9 s. A replica behind sees members
holding more than f voting in the next epoch and ends its epoch just before
its own boundary (`follow_ahead`). The next epoch's first election then set
its end to `next_boundary(now)` - the same boundary - and the epoch ended
~140 ms after it began (pr3: FOLLOW_AHEAD epoch 1 at 7.07 s, ADVANCED 2 at
7.10 s, ENDED 2 at 7.24 s). The other replicas followed, every later epoch
moved one boundary early, the epoch after was only ~2 s long and ended while
the one before was still closing, and for 2-3 s no epoch took new blocks
(epoch 3 ended at 15 s, epoch 4 started at 17.8 s, two seconds after epoch 2
was installed). Blocks published then waited up to 3.5 s. The same happened in
epochs 2 and 5 at 1500/s (gaps 15.1-17.3 s and 31.1-33.6 s).

Fix: an epoch ended before its boundary still owns it; the next epoch ends at
the first boundary after the one the ended epoch was due at. Regression test
`the_epoch_after_a_followed_one_ends_at_the_next_boundary` fails without it.
After the fix every epoch ends on all six nodes within 0.8 s of 7, 15, 23 and
31 s and no publish second has a p95 above 1 s.

## Client fix (1ad24eb72)

`n4-nano-nofork-2000-45k` (old client) stalled at 44,999 of 45,000 confirmed
while all six nodes had cemented the same 45,065 blocks. nanospam recorded a
block as published only after its writes to all six nodes finished; a write to
a distant node can block on TCP backpressure while PR0 (next to the client)
already confirms the block, and that confirmation found no publish time and
was dropped. The block is now marked published before it is sent, so latency
counts from the start of the publish. The Nano and fixed-RAI runs above use
this client; the "before the fix" column used the old one (its latencies
exclude the write time).

## Notes

- CPU samples (`*/cpu-samples.txt`) mostly caught the setup, not the load; the
  Nano 1500/s one shows 47-63 % of 400 on every node.
- The aborted runs (Nano 1500/s with the old client, RAI 2000/s with the new
  client but the old node) are not kept.
