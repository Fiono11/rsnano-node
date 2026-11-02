# RAI across six regions (Google Cloud, Nov 2 2026)

First runs with real link latency: every representative on its own VM,
`run_gate_b.py --hosts` (branch `rai_multihost`, built at 2a30967c9, node
sha256 `c3f93b31…`). Equal-weight committee f = 1, p = 1 (6 seats),
pipelined epochs of 8 s, no forks, `--network-delay-ms 0`.

## Fleet

| PR | Zone | Type | CPU |
|----|------|------|-----|
| controller (nanospam, harness) | europe-central2-b (Warsaw) | n2-standard-2 | Xeon 2.80 GHz |
| PR0 | europe-central2-c (Warsaw) | n1-standard-1 | Xeon 2.20 GHz |
| PR1 | us-central1-c (Iowa) | n1-standard-1 | Xeon 2.30 GHz |
| PR2 | northamerica-northeast1-b (Montréal) | n1-standard-1 | Xeon 2.00 GHz |
| PR3 | us-west4-b (Las Vegas) | n1-standard-1 | Xeon 2.00 GHz |
| PR4 | asia-east1-b (Taiwan) | n1-standard-1 | Xeon 2.20 GHz |
| PR5 | australia-southeast2-c (Melbourne) | n1-standard-1 | Xeon 2.20 GHz |

The N1 CPU platform differs by zone (Haswell / Broadwell / Skylake); it was not pinned.
Latency is measured by nanospam on the controller through PR0's websocket.

Round trips between the node machines (ms, `rtt_ms` in every `result.json`):

| | PR0 | PR1 | PR2 | PR3 | PR4 | PR5 |
|---|---|---|---|---|---|---|
| PR0 | – | 130 | 107 | 158 | 278 | 312 |
| PR1 | 130 | – | 32 | 49 | 173 | 224 |
| PR2 | 107 | 33 | – | 179 | 205 | 225 |
| PR3 | 158 | 49 | 179 | – | 132 | 181 |
| PR4 | 278 | 174 | 205 | 132 | – | 132 |
| PR5 | 334 | 225 | 225 | 180 | 132 | – |

## Saturation sweep (no forks, 15,000 blocks, one run per rate)

| Rate | Goodput (cps) | p50 | p95 | p99 | Close median / max | Settled |
|------|---------------|-----|-----|-----|--------------------|---------|
| 250 | 245 | 426 ms | 560 ms | 810 ms | 2.1 / 5.9 s | yes |
| 500 | 471 | 401 ms | 804 ms | 2.0 s | 2.7 / 6.0 s | yes |
| 750 | 678 | 450 ms | 2.3 s | 3.1 s | 3.6 / 7.5 s | yes |
| 1000 | 834 | 439 ms | 1.3 s | 1.9 s | 2.9 / 3.5 s | yes |

`smoke-250` (5,000 blocks): p50 422, p95 552, p99 712 ms, close median 1.6 s.

- The median stays at 400-450 ms at every rate: the cost of the global
  committee (about 4x the single-machine p50 of ~90-100 ms).
- The tail bends between 500 and 750 blocks/s and goodput falls behind the
  offered rate from 750 on: the 1-vCPU nodes saturate there. 750 and 1000 are
  single runs and not monotonic, so above the knee they are noisy.
- Matrix rate proposed: 400 blocks/s.

Reproduce: `tools/rai/gcp/README.md`; the hosts file was PR0..PR5 above, in order.
