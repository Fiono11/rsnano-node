# Timeout notarization fix and validation

The missing timeout-certificate path was implemented and the previously failing workload now passes. The user explicitly selected a verified timeout certificate as epoch termination, consistent with the paper’s slot exit. A timeout does not confirm or cement a candidate block.

## Validation run

6 PRs, no priority traffic, 15,000 accounts, 15,000 blocks, 1,500 blocks/s, epoch length 7,500, fork probability 5%. Fresh generated data, local TCP/RPC/WebSocket permissions, full audit; generated accounts and node data deleted afterward. All 15,000 blocks were generated and published.

| Canonical outcome | Epoch 0 | Epoch 1 | Total |
|---|---:|---:|---:|
| Finalized block | 8,920 | 5,227 | 14,147 |
| Block notarization only | 564 | 278 | 842 |
| Timeout notarization | 11 | 0 | 11 |
| Terminated roots | 9,495 | 5,505 | 15,000 |
| Pending roots | 0 | 0 | 0 |

Every PR agrees on these canonical outcomes. No conflicting finalized hashes or same-epoch timeout/finalization conflicts were observed. Exit code 0; no extra recovery window was necessary. All six ledgers also agree on confirmation-epoch digests. The number of retained root/epoch certificates differs among PRs because noncanonical recovery instances need not be identical; the table counts each workload root once.

The 60.028-second performance window reported 235.68 confirmed blocks/s, including the idle tail. Mean publication-to-PR0-WebSocket confirmation latency was 1,941.40 ms; 14,142 non-forks averaged 1,940.45 ms and 5 finalized forks averaged 4,638.45 ms. This average excludes notarization-only and timeout outcomes. No throughput improvement is claimed: the input is randomized and this is a liveness validation at the same configured load, not a replay of the identical earlier roots.

## Audit latency by epoch

Milliseconds from local election insertion to the canonical outcome observed on PR0. These exclude publication transit before insertion and WebSocket delivery after finalization.

| Epoch / outcome | Samples | Mean | p50 | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|---:|
| 0/finalized | 8920 | 639.98 | 494.44 | 1458.03 | 3003.13 | 5371.60 |
| 0/block_notarized_only | 564 | 1251.84 | 1129.00 | 2846.58 | 4080.83 | 4434.23 |
| 1/finalized | 5227 | 2211.34 | 2108.99 | 3478.64 | 4219.22 | 7826.88 |
| 1/block_notarized_only | 278 | 2987.92 | 2890.80 | 4058.53 | 4690.87 | 6831.18 |
| 0/timeout_notarized | 11 | 3428.81 | 3127.34 | 4472.79 | 4472.79 | 4472.79 |

## Implemented changes

- Wire kind 3 is explicitly `FirstTimeout`, retaining its existing first-abstention meaning. New wire kind 4 is `Timeout`, a later timeout notarization. Both signed kinds round-trip and bind kind/epoch/hash in the signature. All local RAI peers were rebuilt together.
- FIRST-timeouts occupy the one-FIRST-per-signer slot and contribute timeout notarization weight. Later timeouts only contribute timeout notarization weight, preserving FIRST even if packets arrive out of order. Both types share a signer-deduplicated timeout tally across candidate hashes for the same qualified root and epoch.
- The weighted `all FIRST weight - maximum non-timeout FIRST tally` trigger follows Protocol 1 lines 32–35. A signer must already have a FIRST record for that epoch. A timeout prevents final voting in that epoch; an in-memory final reservation or durable final lock prevents a new later-timeout signature.
- A timeout certificate uses the same 62% quorum as block notarization (4 of 6 equal PRs). It releases admission capacity and retains authenticated votes for ordinary Publish/ConfirmReq/ConfirmAck recovery, including requests from a later epoch. It produces no block confirmation callback, cementation or WebSocket block-confirmation event.
- Audit event 7 records a verified timeout certificate with zero candidate hash. The checker can use it as a canonical timeout outcome, requires that certificate on every PR, still rejects pending roots, and rejects same-epoch timeout/finalization conflicts. Finalized blocks take precedence over block notarizations, which take precedence over timeout-only outcomes; earliest epoch breaks ties within each class.
- `ELECTION_RESULT.timeout_notarized` reports root/epoch timeout certificates, and `TERMINATION_RESULT.timeout_roots` reports unique canonical timeout roots. Documentation now distinguishes these outcomes and the strict all-roots requirement.

## Why this fixes the reproduced failure

The earlier failure had all six representatives’ participation but candidate FIRST support split across epochs below the three-vote second-look threshold. For the recorded example, epoch 0 had candidate FIRST tallies 1/1 plus four first-timeouts; epoch 1 had tallies 2/2 plus two first-timeouts. The old implementation neither tallied those timeout notarizations nor emitted later timeout notarizations, so it could never produce the paper’s timeout certificate.

With the fix, epoch 0 already has four timeout notarizations. In epoch 1, six total FIRST participations minus a maximum candidate tally of two exceeds the timeout-trigger threshold. Later timeout votes complete its certificate. The deterministic reproduction now asserts both epochs time out while neither candidate is marked confirmed. The live run independently validates timeout outcomes on 11 canonical roots; those are newly randomized roots, not the earlier audit’s identical root set.

## Tests and limits

RAI core tests passed: nanospam 33 (1 ignored), node 630, types 128, plus the added timeout recovery test. Legacy core tests passed: nanospam 33 (1 ignored), node 597, types 126. Message/network/RPC protocol tests passed: 79 + 40 + 28 + 316. Existing RAI recovery integration tests passed: 3. A focused signing-order test verifies timeout/final exclusion in both directions. Formatting and whitespace checks passed.

This remains the repository’s experimental root/local-epoch adaptation. The change does not add global slots, leaders, proposal-timeout timers, committee transitions or crash recovery, nor does this workload establish the full Kudzu safety/liveness proof. Timeout termination does not advance the cemented-block epoch counter or make an account frontier spendable.

## Evidence

Live run: `../rai-ramp/b15000-r1500-timeout-fix/` contains exact command and binary SHA-256 hashes, `run.log`, `summary.json`, `analysis.json`, `epoch-results.json`, `audit.json.gz`, telemetry and `cleanup.json`. This directory contains build, core/protocol/integration test and formatting logs. The earlier failing ramp and detailed paper comparison remain in `../rai-ramp/report.md`.
