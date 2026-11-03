# 45,000 blocks across six regions on 4-vCPU nodes (Nov 3 2026)

Two runs, `--rate 2000` and `--rate 1500`, each `--blocks 45000 --accounts 45000
--timeout 600`, no forks, 8 s pipelined epochs, equal-weight committee f=1 p=1.
Same six regions and binary (built at 85c062774) as `../gcp-global-n2-2026-11-02`,
but every node resized from n2-standard-2 to **n2-standard-4** (Xeon 2.80 GHz;
CPU quota raised to 32). Controller n2-standard-4 in europe-central2-b.

Both runs **settled**: all six nodes cemented the same 45,065 blocks, all
45,000 load blocks confirmed, correctness checks passed, every epoch closed in
round 0.

| | 2000/s, 4 vCPU | 1500/s, 4 vCPU | 1500/s, 2 vCPU (`../gcp-global-n2-1500-45k-2026-11-02`) | 2000/s, 2 vCPU (`../gcp-global-n2-45k-stall-2026-11-02`) |
|---|---|---|---|---|
| Settled | yes | yes | yes | no (closes fell behind) |
| Goodput | 1775 cps | 1318 cps | 1302 cps | - |
| p50 | 405 ms | 405 ms | 473 ms | - |
| p95 | 2.41 s | 972 ms | 2.78 s | - |
| p99 | 3.32 s | 1.85 s | 3.98 s | - |
| Close median / max | 3.5 / 6.6 s | 2.4 / 5.6 s | 3.2 / 7.2 s | 3 s, then 10 s, 26 s, 2 min, 11 min |
| Settle wait after the client | ~4 s | ~4 s | ~9 s | - |

- 2000/s × 45k, which collapsed on 2 vCPUs, now settles with closes inside the
  8 s epoch (epochs 0-5: 1.4, 3.4, 3.7, 1.4, 3.7, 6.6 s). Its tail is one
  stretch: seconds 18-19 of the load (95 then 8,072 confirmations, p95 3.3 s),
  i.e. confirmations held for about a second, then released at once.
- At 1500/s the 4-vCPU tail is ~3x smaller than on 2 vCPUs (p95 972 ms vs
  2.78 s, p99 1.85 s vs 3.98 s); p50 drops from 473 to 405 ms. Per-second p95
  goes above 1 s only at seconds 12, 17 and 29 of the load (1.0-1.9 s), and
  after the load ends.
- CPU under load at 1500/s (`cpu-samples.txt`, % of one core, 400 = all four):
  Warsaw 118, Iowa 107, Montréal 82, Las Vegas 65, Taiwan 39, Melbourne 36, so
  roughly a quarter to a third of the node at most. The 2000/s sample was taken
  during setup (7-13 %) and missed the load.
- No RPC stats post-mortem: the fleet powered off after the pair, so the
  overload counters of the 2-vCPU READMEs (vote drops, aggregator overfill) were
  not collected.
- `settled_after_s` in both `result.json` files reads 514: the harness then
  computed it as `timeout - settle_timeout + settle wait` (600 - 90 + ~4). Fixed
  in the same commit; the real settle wait is in the table. The 2-vCPU value
  219.1 is 300 - 90 + 9.1 by the same rule.

Takeaway: with 4 vCPUs per node, 2000/s is sustainable over 45k blocks across
six continents; 1500/s has a sub-second p95. Since only the core count
changed, the 2-vCPU collapse at 2000/s points to CPU, not the WAN links.
