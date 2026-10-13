# Reports laid over the inherited base, Oct 13 2026

Builds: 733abba3c (delta reports) and aef2f1468 (a live view without a base
is laid over it once it arrives); baseline 5b7710360 (report worker only).
All runs: RAI, 10% forks, 2,000 blocks/s, 45,000 blocks, seed 2001134.

- `check/`: aef2-equivalent build with `RAI_DELTA_CHECK=1`. 79 validations
  re-derived from flat copies of the selected reports: state and evidence
  claims agreed in every one. The run itself failed (pr3 fell behind at
  epoch 3); the check doubles derivation work under the exchange lock.
- `perf/`, `profile/` (sampled for 8 s at 10 s into the load): aef2, both
  failed the same way (one node behind).
- `ab/`: back-to-back A/B on the same host state. a-5b77 (baseline) failed
  (pr4 behind, closes up to 50 s, p95 8.9 s); b-aef2 settled (p95 3.8 s,
  closes up to 12 s, every fork resolved). The lagging-node failure occurs
  with both builds at this load and is not caused by the change; single
  runs at 2,000/s do not separate the two builds.

Scaling (release, `report_cost_against_history` in node/src/consensus/reports/mod.rs,
15,000 changes an epoch), flat -> over the base:

| history | build report | encode for a stream | persisted |
|---|---|---|---|
| 45,000 | 35 -> 19 ms | 48 -> 13 ms | 6.3 -> 1.6 MB |
| 450,000 | 244 -> 21 ms | 456 -> 13 ms | 49 -> 1.6 MB |
| 2,000,000 | 1,053 -> 20 ms | 2,405 -> 13 ms | 212 -> 1.6 MB |

Still linear in history per close: `build_state` clones the predecessor
`EpochLedger` and `ReportIndex::new` places all of it; the report base
(`report_ledger` + projection) and the decided-state record are built once
per epoch.

## 1df3a92b8: placement, installation and the next report base from the changes

`check-1df3/`: RAI, 10% forks, 1,000 blocks/s, `RAI_DELTA_CHECK=1`. 38
validations re-derived from flat reports with a full ReportIndex agreed in
state and claims; 42 incremental report bases (6 nodes x 7 epochs) equal
the base built afresh, projected flag included. Settled, p95 412 ms, every
fork resolved.

`checkpoint_cost_against_history` (release), at 2,000,000 finalized blocks:
placement 303 -> 11 ms, installation's new finals 205 -> 1 ms, next report
base 2,319 -> 179 ms, d_{e-1} per check 319 ms -> lookup. Still linear, once
per epoch: frontiers for the committee (314 ms), the state hash recorded at
installation (319 ms), copying the report base's maps (179 ms), the decided
record (48 ms, 208 MB written); per derivation: the predecessor clone in
build_state (69 ms).
