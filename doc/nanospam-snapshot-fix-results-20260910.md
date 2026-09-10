# Nanospam snapshot fix ramp

Six PRs, 5% forks, no priority probes, accounts = requested blocks. Each successful run verifies exactly two epochs. Times: first publication to PR0 WebSocket receipt. Counts: election roots per epoch, not unique roots across epochs. Full tracing and audit disabled. Successful node data deleted; failed data preserved.

## 4,500 blocks at 90 blocks/s: PASS

Evidence: [b4500-r90-no-republish.json](/private/tmp/nanospam-fix-20260910/b4500-r90-no-republish.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 3486 | 61.4 / 90.0 / 189.5 | 3469 | 72.4 / 100.7 / 198.6 |
| 0 | Fork | 174 | 137.8 / 172.8 / 198.5 | 0 | — |
| 1 | Nonfork | 820 | 104.2 / 178.5 / 327.8 | 815 | 129.1 / 219.0 / 343.7 |
| 1 | Fork | 41 | 1498.0 / 6510.7 / 36262.9 | 2 | 249.3 / 260.6 / 260.6 |

## 4,500 blocks at 90 blocks/s: PASS

Evidence: [b4500-r90.json](/private/tmp/nanospam-fix-20260910/b4500-r90.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 3471 | 63.1 / 95.8 / 194.0 | 3451 | 77.2 / 104.9 / 172.0 |
| 0 | Fork | 194 | 139.2 / 180.0 / 254.2 | 0 | — |
| 1 | Nonfork | 819 | 116.5 / 191.4 / 478.3 | 818 | 146.2 / 242.1 / 518.5 |
| 1 | Fork | 101 | 13165.3 / 35693.9 / 46134.6 | 7 | 4655.1 / 5487.0 / 5487.0 |

## 4,500 blocks at 90 blocks/s: PASS

Evidence: [b4500-r90-latency.json](/private/tmp/nanospam-fix-20260910/b4500-r90-latency.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 3456 | 65.2 / 100.7 / 249.4 | 3436 | 75.0 / 107.5 / 179.4 |
| 0 | Fork | 201 | 141.5 / 190.8 / 223.6 | 1 | 202.7 / 202.7 / 202.7 |
| 1 | Nonfork | 817 | 107.9 / 203.3 / 575.1 | 814 | 127.0 / 238.5 / 637.7 |
| 1 | Fork | 108 | 11700.1 / 36272.4 / 38239.8 | 1 | 196.2 / 196.2 / 196.2 |

## 6,300 blocks at 90 blocks/s: PASS

Evidence: [b6300-r90-windows.json](/private/tmp/nanospam-fix-20260910/b6300-r90-windows.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 3464 | 63.7 / 95.2 / 134.5 | 3454 | 75.2 / 104.0 / 142.3 |
| 0 | Fork | 200 | 137.8 / 183.3 / 200.0 | 1 | 164.3 / 164.3 / 164.3 |
| 1 | Nonfork | 2518 | 106.2 / 182.8 / 433.5 | 2518 | 132.6 / 218.7 / 489.6 |
| 1 | Fork | 182 | 6355.5 / 35490.3 / 45007.4 | 3 | 334.2 / 407.6 / 407.6 |

## 6,300 blocks at 90 blocks/s: PASS

Evidence: [b6300-r90-independent-batches.json](/private/tmp/nanospam-fix-20260910/b6300-r90-independent-batches.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 3458 | 64.3 / 93.1 / 200.4 | 3438 | 74.9 / 102.7 / 122.3 |
| 0 | Fork | 201 | 136.6 / 183.5 / 204.9 | 0 | — |
| 1 | Nonfork | 2527 | 77.9 / 105.4 / 5222.4 | 2522 | 99.2 / 156.9 / 5230.9 |
| 1 | Fork | 179 | 5554.0 / 35775.1 / 40423.9 | 2 | 555.9 / 564.5 / 564.5 |

## 6,750 blocks at 135 blocks/s: PASS

Evidence: [b6750-r135.json](/private/tmp/nanospam-fix-20260910/b6750-r135.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 5187 | 66.5 / 100.1 / 142.4 | 5168 | 77.7 / 106.9 / 211.0 |
| 0 | Fork | 282 | 161.0 / 227.1 / 305.6 | 1 | 183.4 / 183.4 / 183.4 |
| 1 | Nonfork | 1251 | 115.0 / 192.1 / 435.2 | 1250 | 144.6 / 226.7 / 333.9 |
| 1 | Fork | 89 | 7666.6 / 26309.8 / 36156.6 | 16 | 14657.9 / 15737.0 / 15737.0 |

## 10,125 blocks at 203 blocks/s: PASS

Evidence: [b10125-r203.json](/private/tmp/nanospam-fix-20260910/b10125-r203.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 7844 | 65.6 / 92.9 / 143.4 | 7790 | 78.0 / 104.1 / 149.4 |
| 0 | Fork | 399 | 153.9 / 205.2 / 302.1 | 1 | 188.5 / 188.5 / 188.5 |
| 1 | Nonfork | 1846 | 105.0 / 175.8 / 482.3 | 1841 | 132.3 / 220.0 / 666.6 |
| 1 | Fork | 165 | 9226.0 / 35879.1 / 57074.4 | 2 | 627.1 / 661.6 / 661.6 |

## 15,188 blocks at 304 blocks/s: PASS

Evidence: [b15188-r304.json](/private/tmp/nanospam-fix-20260910/b15188-r304.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 12800 | 74.8 / 115.3 / 439.3 | 11526 | 79.7 / 108.1 / 172.7 |
| 0 | Fork | 670 | 198.8 / 284.1 / 5349.3 | 0 | — |
| 1 | Nonfork | 2896 | 361.6 / 1359.7 / 5536.1 | 2894 | 403.2 / 1438.9 / 5700.2 |
| 1 | Fork | 734 | 18064.4 / 40547.8 / 46078.9 | 68 | 5313.3 / 5446.5 / 5779.6 |

## 22,782 blocks at 456 blocks/s: PASS

Evidence: [b22782-r456.json](/private/tmp/nanospam-fix-20260910/b22782-r456.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 21662 | 173.0 / 1040.3 / 1771.2 | 17299 | 81.4 / 109.3 / 189.2 |
| 0 | Fork | 1120 | 353.8 / 1582.8 / 6637.7 | 0 | — |
| 1 | Nonfork | 4363 | 1510.9 / 6813.4 / 7390.9 | 4359 | 1641.9 / 7026.8 / 7635.9 |
| 1 | Fork | 1120 | 21157.4 / 42249.2 / 48259.7 | 15 | 1345.6 / 2074.2 / 2074.2 |

## 34,173 blocks at 684 blocks/s: PASS

Evidence: [b34173-r684.json](/private/tmp/nanospam-fix-20260910/b34173-r684.json).

| Epoch | Elections | Terminated | Mean / p95 / max ms | Finalized | Mean / p95 / max ms |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 32235 | 874.2 / 5714.7 / 15427.4 | 25123 | 87.5 / 119.7 / 279.7 |
| 0 | Fork | 1701 | 1321.4 / 7357.4 / 16143.4 | 3 | 235.3 / 258.3 / 258.3 |
| 1 | Nonfork | 7323 | 7679.5 / 15773.9 / 22643.7 | 7322 | 8545.7 / 17274.9 / 28972.9 |
| 1 | Fork | 1724 | 27180.6 / 50717.7 / 60349.9 | 0 | — |
