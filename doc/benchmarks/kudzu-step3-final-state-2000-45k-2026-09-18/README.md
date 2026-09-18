# Kudzu step 3: final state hash convergence (2026-09-18)

Same node as `../kudzu-step3-no-eviction-2000-45k-2026-09-18` (`208258a9f`) plus the
`final_state` RPC; same command (6 PRs, 2000 blocks/s, 45k blocks, 5 % forks).

## Termination condition

`final_state` returns, per PR, an order-independent hash of the final ledger state: one
entry `(account, height, block)` per account, the settled single notarization certificate if
there is one, else the cemented frontier. Timeout certificates and conflicting notarization
certificates contribute nothing; a finalized block is included whether the PR finalized it
by certificate or cemented it as a dependency. The response also says whether every
election is terminated / settled and lists the conflicting roots.

`final_state_check.py` polls all PRs and stops when every PR has settled every election and
all six hashes are equal. The settled gate is needed because a single notarization
certificate is not monotonic: a late second-look certificate turns it into a conflicting
pair. After convergence the blocks of the conflicting roots are the ones a PR discards; the
check asserts that none of them is cemented on any PR (a finalization certificate for X and
a notarization certificate for a sibling cannot coexist with honest representatives and
equal `n`). The per-root `settle_check.py` runs afterwards as a cross-check.

## Results (run 2; `run1/` is the first run with the per-hash `block_info` assertion)

| | no eviction | run 1 | **run 2** |
|---|---|---|---|
| Confirmation rate | 1817 cps | 1775 cps | 1840 cps |
| Average confirmation time, cps-weighted | 159 ms | 121 ms | 129 ms |
| Median of the per-second averages | 117 ms | 103 ms | 111 ms |
| Worst second | 473 ms | 264 ms | 265 ms |
| Non-fork finalization p50 / p95 (healthy PRs) | 95–101 / 203–238 ms | 91 / 131–149 ms | 93–95 / 149–160 ms |
| Evictions | 0 | 0 | 0 |
| Check time after convergence | – | 44 s | **< 1 s** |

Both runs converged on the first poll at which every PR was settled: run 1 hash
`5ADA3D1F…` (15,332 accounts, 660 single-notarized, 1,479 conflicting roots, 2,958 blocks
discarded), run 2 hash `CB620224…` (14,645 accounts, 904 single-notarized, 1,242 conflicting
roots, 2,484 blocks discarded), 0 discard violations in both. The per-root cross-check
reported `SETTLED_CONSISTENT` on its first snapshot both times.

Run 1 spent 44 s in the check itself: one `confirmation_info` per conflicting root and one
`block_info` per discarded block per PR, ~27k sequential HTTP requests. Since then
`final_state` returns each conflicting root's candidate hashes and the assertion uses
`blocks_info` (`include_not_found`, 1,000 hashes per request): 6 + 18 requests, under a
second. The old `compare_certs.py` diagnostic was dropped from the run script: its ~13k
extra connections after the stats snapshot ran macOS out of ephemeral ports (connect
timeout) in run 2; nothing of value was lost, it duplicated the cross-check.

Physical rollback of the discarded blocks is not performed: a settled root with conflicting
certificates is a dead slot either way.
