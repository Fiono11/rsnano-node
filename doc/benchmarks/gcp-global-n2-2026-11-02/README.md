# RAI across six regions on 2-vCPU nodes (Google Cloud, Nov 2 2026)

Same six regions and committee as `../gcp-global-2026-11-02` (f = 1, p = 1,
6 seats, 8 s pipelined epochs, no forks), nodes resized from n1-standard-1 to
**n2-standard-2** (all on the same 2.80 GHz Xeon), controller n2-standard-4.
Built at 93331b36b: setup recovery paced by schedule; the first attempt at
750/s on this fleet failed in setup (one node kept a setup block uncemented,
recovery ran twice in 120 s) before that fix.

Round trips (ms) PR0..PR5 = Warsaw, Iowa, Montréal, Las Vegas, Taiwan, Melbourne:
126 / 106 / 160 / 278 / 318 from PR0; 28-221 between the others (`rtt_ms`).

## Sweep (no forks, 15,000 blocks, one run per rate)

| Rate | Goodput (cps) | p50 | p95 | p99 | Close median / max | 1 vCPU p95 (../gcp-global-2026-11-02) |
|------|---------------|-----|-----|-----|--------------------|------|
| 500 | 473 | 404 ms | 587 ms | 1.5 s | 2.4 / 4.4 s | 804 ms |
| 750 | 678 | 395 ms | 619 ms | 1.4 s | 2.9 / 3.6 s | 2.3 s |
| 1000 | 848 | 413 ms | 973 ms | 1.6 s | 2.0 / 3.0 s | 1.3 s |
| 1500 | 1197 | 389 ms | 463 ms | 500 ms | 1.7 / 3.3 s | - |
| 2000 | 1427 | 384 ms | 541 ms | 597 ms | 3.2 / 4.6 s | - |

Every run settled. Goodput is confirmed blocks over the run's duration, so it
falls short of the rate by the time the last blocks take to confirm; it is not
a measure of falling behind.

- p50 is 385-415 ms at every rate: the links, not the load.
- With 2 vCPUs the 1-vCPU tail at 750/s (p95 2.3 s) is gone: that one was CPU.
- No saturation up to 2000/s. At 500/s the nodes used 31-50 % of one core of
  their two (`cpu-samples.txt`; the samples at higher rates missed the short
  load window and read ~1 %, they are not valid).
- The tail does not follow the load: 1500 and 2000 have the best p99. A 15k
  run at 2000/s lasts 7.5 s, about one epoch; at 500/s it lasts 30 s, about
  four epoch transitions. Hypothesis, not tested: the p99 tail is blocks
  caught at an epoch transition while the global close takes 2-3 s. Fair
  high-rate numbers need runs that span several epochs (45k blocks).
- nanospam shows ~100 % of a core on the controller: `enqueue_blocks` spins
  on `yield_now()` while ahead of the rate; signing with stub work is cheap,
  so this is not the client's limit.
