# RAI epochs, step 1: epoch-indexed votes and elections, single epoch (2026-09-18)

Same build flags, command and machine as `../kudzu-step3-no-eviction-2000-45k-2026-09-18`
(`--features rai_protocol`, 6 PRs, 2000 blocks/s, 45k blocks, 5 % forks, `run_fork_settle.sh`
until `SETTLED_CONSISTENT`, `caffeinate -i`, idle machine).

Changes on top of that run (commits `da2f44a69`, `4a1989c47`), all behind `rai_protocol`:

1. Votes carry a signed `ConsensusEpoch` (8 bytes after the timestamp, part of the signed
   payload, so a vote cannot be replayed into another epoch's election).
2. Elections are identified by `ElectionId` (root, epoch): the AEC holds one election per root
   and epoch, the vote router one route per (block, epoch), the Kudzu local slot state is per
   epoch. New blocks and fork candidates join the AEC's current epoch; a vote only counts in
   the election of its own epoch.
3. `ConfirmReq` carries the epoch (8 bytes before the roots); the solicitor bundles per channel
   and epoch, the aggregator replies with votes and certificate evidence of the requested epoch,
   the vote generators batch candidates per epoch, the local vote history keeps one vote per
   (hash, kind, epoch). `confirmation_info` takes and reports the epoch.

Nothing advances the epoch yet: this run has the single epoch 0 and only checks that the
addressing change and the two extra 8-byte fields cost nothing. The first attempt of the run
stalled at 0 blocks (nanospam forked the funded account's first block, the known pitfall) and
the script restarted; the numbers are from the second attempt.

## Results

| Metric (nanospam, publishing phase = status lines with ≥ 1000 cps) | no-eviction run | **this run** |
|---|---|---|
| Non-fork blocks confirmed | 42,818 | 42,778 |
| Confirmation rate | 1817 cps | 1851 cps |
| Average confirmation time, cps-weighted | 159 ms | **111 ms** |
| Median of the per-second averages | 117 ms | 108 ms |
| Worst second | 473 ms | **157 ms** |
| Settle phase | 11 s | **3 s** |
| Cemented on every PR | 42,981 | 42,855 |
| Forked roots settled identically on all PRs | 2,072 | 2,198 (1,646 with two certificates, 1,382 with a timeout certificate) |
| Inconsistencies | 0 | 0 |

Node side (`kudzu-node-latency.txt`, from election start on each PR):

| PR | non-fork finalization p50 / p95 / p99 | non-fork termination p50 / p95 | fork termination p50 / p95 / p99 | fork termination max |
|---|---|---|---|---|
| PR0 | 92 / 133 / 193 ms | 75 / 114 ms | 142 / 216 / 295 ms | 413 ms |
| PR1 | 91 / 139 / 243 ms | 76 / 117 ms | 141 / 224 / 295 ms | 10395 ms |
| PR2 | 91 / 130 / 157 ms | 75 / 113 ms | 141 / 221 / 281 ms | 10250 ms |
| PR3 | 92 / 146 / 239 ms | 76 / 121 ms | 141 / 216 / 323 ms | 10292 ms |
| PR4 | 91 / 132 / 191 ms | 75 / 116 ms | 143 / 215 / 293 ms | 424 ms |
| PR5 | 91 / 133 / 204 ms | 75 / 116 ms | 142 / 214 / 291 ms | 10478 ms |

Performance is maintained; the run is in fact the best so far (no starved PR, p95 under
150 ms on every PR, fork termination p95 ≈ 220 ms). The gain over the no-eviction run is
within the run-to-run variance of this machine (one PR out of six is usually starved by the
machine for a few seconds; here none was), not attributable to the epoch change. The four
~10 s fork termination maxima are single forks recovered through two 5 s solicitation rounds,
as before. `evicted` and `over_cap` are 0 on every PR.

Message counts per PR: ~10k `confirm_ack` in, ~580 `confirm_req` in, ~3.1k fork candidate
replies and ~2.2k certificate evidence replies sent.
