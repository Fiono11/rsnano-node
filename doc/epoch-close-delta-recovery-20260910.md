# Epoch-close delta recovery and candidate admission

## Protocol and transport

Epoch-close proposals and votes carry a signed epoch digest, not snapshot membership. A separate signed recovery request advertises known base digests and the target digest. A peer with both a target snapshot and a shared nonempty base returns only additions and removals, in bounded unicast pages. There is no full-list fallback. Without a shared base, recovery retries as ordinary block and vote recovery progresses. A receiver pins the selected base during assembly, verifies the reconstructed target digest, and validates membership before using the candidate. Certified membership, assertions, vote thresholds, and the 40-second epoch schedule are unchanged.

Recent local membership changes update recovery bases without generating additional unsigned close candidates. Flooding serializes each message once for all destinations; hexadecimal encoding uses the existing hex library instead of formatting each byte separately.

## Concrete high-load failure

The first delta-only 51,260/1,026 run left PR1 and PR5 in epoch 0 round 16 with 64/65 retained candidates, all complete and valid. Four other PRs closed epochs 0 and 1. The code admitted new candidate headers only while the total candidate map contained fewer than 64 entries. Completed historical snapshots consumed this limit, preventing lagging PRs from learning the round-17 decision. The run was deliberately stopped after capturing this state, rather than increasing load. Its node data remains in `target/nanospam-debug-20260910/b51260-r1026-delta-data/`.

`epoch_close_recovery_limit_does_not_drop_authenticated_decision_headers` reproduces the failure deterministically: populate 64 retained candidates, then deliver a valid next-round FIRST. The original admission check fails; the correction passes.

## Correction

Candidate metadata is admitted after the vote tally accepts an authenticated vote. Rejected equivocating FIRST votes cannot allocate candidate headers. The existing 64 limit applies to concurrent incomplete delta assemblies, not retained completed snapshots or decision headers.

Advancing an epoch merges unacknowledged compact close evidence and recovery snapshots into the archive instead of replacing them. Older receipts remain admissible while their local close receipts are retained. The archive is released only after every retained close has matching receipts from all weighted PRs. A regression advances twice while one peer lags and verifies that epoch-0 votes, snapshots, and receipts remain available.

Round timeout backoff remains 3, 6, 12, 24, 48, 96, then 192 seconds. Notarization certificates can advance rounds immediately; the timeout is not an obligatory delay before every round transition.

## Validation

All 16 close-related node tests passed, including the candidate admission, rejected equivocation, two-transition archive, shared-base-only recovery, reordered delta pages, immutable digest verification, split-snapshot convergence, and finalized membership checks. The failure-before log and passing test log are under `target/nanospam-debug-20260910/`.

The earlier delta-only 6,300/90 control passed with six PRs agreeing on exactly two epochs; successful node data was deleted. Fresh post-close nonfork FIRST-to-termination means were 42.7–45.4 ms across the five-second publication windows starting at 45–65 seconds. Full primary latency rows and maxima are preserved in `b6300-r90-delta.json`.

The header-corrected run `b51260-r1026-delta-headers` failed the existing epoch-1 convergence deadline after 478.77 seconds. All six PRs closed epoch 0 with digest `BE680ECE6C59039663905BBE33F6FEF33B2AF1AC55BFF904E6BF53DB12711EB4` (39,793 members); none closed epoch 1. The final largest drain had 272 pending elections. Shutdown was clean and failed node data is preserved. No complete epoch performance result was emitted, so no successful latency table is claimed.

## Subsequent workload timeout recovery defect

The captured pending roots include two FIRST votes and three FIRST-timeouts. Timeout is eligible, but the three timeout votes are below the unchanged certificate threshold. `KudzuVotes::needs_vote` considered a representative's FIRST sufficient because it already contributes notarization weight; with no block quorum it returned false. `ConfirmationSolicitor` consequently stopped requesting that representative's later TIMEOUT, even though this is the missing vote needed for termination.

For example, root `FA09909A134ABA87764153284E00A389E543A0C37EFE630B9959E298BFD25710FA09909A134ABA87764153284E00A389E543A0C37EFE630B9959E298BFD25710`, winner `2F7E405CA1459B62FA03115F313C08D4D27C2E77AA48C9329541353FEDC9E3E7`, had FIRST from PR0/PR1 and FIRST-timeout from PR2/PR4/PR5. Read-only LMDB inspection found no cemented membership, canonical membership, or final-vote lock for this winner/root on any PR. This rules out a conflicting final lock for this particular pending root.

`timeout_recovery_requests_upgrade_from_existing_first_voter` reproduces this exact five-vote pattern. Before the fix it fails because recovery does not request PR1's later timeout. The correction requests missing timeout votes whenever timeout is eligible and there is no block quorum. It changes request selection only, preserving FIRST, signing restrictions, thresholds, and membership assertions. After receiving the additional timeout, the regression checks the timeout certificate and that further requests to that voter are unnecessary.

Evidence: `headers-drain-latest.json`, `headers-pending-ledger.json`, the read-only `read-key.c` utility, and `timeout-recovery-before.log` in the new artifact directory. The subsequent same-load rerun passed; results follow. Full tracing and validation diagnostics remain disabled during benchmark runs. No compilation runs concurrently with nanospam.


## Same-load rerun after both recovery corrections: PASS

`b51260-r1026-timeout-recovery` completed successfully in 404.64 seconds, with six PRs agreeing on exactly two closed epochs. Six fresh PRs, 40-second epochs, 51,260 blocks/accounts, 1,026 blocks/s, 5% forks, `--no-prio`, compact PR0 outcomes, tracing/audit disabled. No extra epoch closed. Shutdown was clean; successful node data was deleted automatically. No higher load was started.

- Epoch 0: `5A1DF93D623AED993ED126EDE72C8505918C22BDE9BDBD0E8B6AFCBD3E757A6C`.
- Epoch 1: `CFC31D76B69080772C6A54796BFBD3B4F481C3D115FAD363BEE338A1615A4B69`.
- Base revision: `267f1a623b7ed9dbca3402be285a7f5e54ab559a`; working-tree diff SHA-256 at run start: `c8725df04d05988ec415a933915f8aa41fb0c916edfd868843e6d386a5c2ce71`.

Primary timing: first publication to PR0 WebSocket outcome receipt. Counts are election roots per epoch, so a root can occur in both epochs. All latency values below are milliseconds.

| Epoch | Roots | Terminated | Mean / p95 / max | Finalized | Mean / p95 / max |
|---|---|---:|---:|---:|---:|
| 0 | Nonfork | 42,536 | 3,344.470 / 18,935.001 / 136,459.053 | 36,092 | 103.778 / 172.616 / 277.102 |
| 0 | Fork | 2,208 | 5,283.572 / 22,273.488 / 167,488.967 | 7 | 181.156 / 365.285 / 365.285 |
| 1 | Nonfork | 12,613 | 54,930.938 / 213,937.795 / 283,216.148 | 12,232 | 60,802.942 / 209,626.142 / 279,770.084 |
| 1 | Fork | 2,541 | 57,270.972 / 217,634.988 / 312,365.407 | 156 | 197,000.568 / 260,004.943 / 267,013.509 |

This demonstrates convergence, not normalized latency at this load. PR0 polling first observed epoch 0 closed at 194.542 seconds after the schedule anchor (500 ms polling, subject to RPC delay). Publication had already ended, so there are no fresh post-close publication windows for assessing epoch-1 normalization. Aggregate epoch-1 publication latencies include old roots and boundary/recovery delays. The 6,300/90 control remains the available fresh-window measurement for the delta implementation.

The result JSON preserves maxima, FIRST-to-outcome summaries, prior-epoch-outcome cohorts, and five-second publication windows. Logs and PR0 count samples remain alongside it. The candidate-admission/archive regressions passed (16 close-related tests), the timeout-request correction passed all 21 Kudzu tests, and both RAI release rebuilds passed. No compilation overlapped either benchmark.

Final cleanup verification found no benchmark/node processes. Both failed datasets remain available for debugging (3.7 GB and 4.9 GB). Successful-run node data is absent. Disposable incremental build cache was removed, leaving about 9.5 GiB free after validation. The pre-existing `kudzu.pdf` is untouched. Historical `/private/tmp/nanospam-*` evidence was deleted by the user; new artifacts are in `target/nanospam-debug-20260910/`.
