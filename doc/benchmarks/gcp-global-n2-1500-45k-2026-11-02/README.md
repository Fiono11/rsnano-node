# 1500/s for 45,000 blocks across six regions (Nov 2 2026)

One run, `--rate 1500 --blocks 45000 --accounts 45000`, no forks, 8 s
pipelined epochs, on the 2-vCPU six-region fleet of `../gcp-global-n2-2026-11-02`
(built at 85c062774). It **settled**: all six nodes cemented the same 45,065
blocks, all 45,000 load blocks confirmed.

| | 1500/s x 45k | 1500/s x 15k (`../gcp-global-n2-2026-11-02`) | 2000/s x 45k (`../gcp-global-n2-45k-stall-2026-11-02`) |
|---|---|---|---|
| Settled | yes | yes | no (closes fell behind, aborted) |
| p50 | 473 ms | 389 ms | - |
| p95 | 2.78 s | 463 ms | - |
| p99 | 3.98 s | 500 ms | - |
| Close median / max | 3.2 / 7.2 s | 1.7 / 3.3 s | 3 s, then 10 s, 26 s, 2 min, 11 min |
| Goodput | 1302 cps | 1197 cps | - |

- Closes keep pace: every epoch closes 1-10 s after it ends and the durations
  do not grow over the run (at 2000/s they grew without bound).
- The sustained tail is 6-8x the 15k-block run's: more blocks pass through
  the 3-9 s closes. The average confirmation time kept rising during the load
  (469 -> 717 -> 960 ms).
- CPU under load (`cpu-samples.txt`, % of one core, 200 = both cores):
  Warsaw 130, **Iowa 167**, Montréal 123, **Las Vegas 165**, Taiwan 76,
  Melbourne 72. The two central US nodes are near their limit; at 500/s every
  node was at 31-50. At 2000/s they likely saturated (that sample missed the
  load), which would explain closes falling behind there.
- First overload signs: vote send drops up to 3.6 k, vote processor overfill
  7.2 k on Iowa only, block sends dropped up to 15 k; no aggregator,
  block processor or close-message overfill (all in the millions or
  thousands at 2000/s, zero in the 15k runs).
- Side observation: in epochs 2, 5 and 8 one node reports EPOCH_ENDED ~5 s
  before the others (right after the previous epoch's end); the 2000/s run
  shows it in epoch 2 too. Not investigated.

Takeaway: on 2-vCPU nodes with global links, 1500/s is sustainable with
little headroom; 2000/s is not. 4-vCPU nodes (quota 32) are the obvious
next test for 2000/s.
