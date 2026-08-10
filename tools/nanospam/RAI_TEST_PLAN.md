# RAI protocol nanospam test plan

This plan exercises the RAI implementation against the safety and liveness
properties in `rai_spec.tex`. It deliberately separates tests that nanospam can
assert today from tests that need more observability or fault injection. A
successful load run is evidence for the exercised execution; it is not a proof
of the specification's universal safety claims.

## Build and isolation

Build both programs with the same source revision and the `rai_protocol`
feature:

```sh
cargo build --release --features rai_protocol \
  -p rsnano_cli \
  -p nanospam
export PATH="$PWD/target/release:$PATH"
```

`rsnano_node` is a library package. Nanospam launches the `rsnano` executable
from `rsnano_cli`, so building only `rsnano_node` can silently leave an older
node executable in `target/release`.

Use a fresh, unique `--data-dir` for every test. Do not run two nanospam
networks concurrently: their peer, RPC, and websocket ports are fixed. Preserve
the node logs and final nanospam output for every run.

The RAI-specific end-to-end oracle is enabled only when
`--rai-epoch-duration-ms` is supplied. For the tests below, nanospam must exit
successfully and its final validation must establish:

- every PR observed the same epoch-0 close-cut hash;
- every PR installed the same epoch-0 close-record hash;
- epoch 0 closed, epoch 1 is open, and no old epoch is still closing;
- epoch-0 cut obligations reached their expected terminal state;
- all PRs report identical per-epoch finalized workload counts;
- the workload contains blocks finalized in both epoch 0 and epoch 1; and
- the total finalized workload equals `--blocks`.

Unless a case says otherwise, use 5-second epochs and a 100-ms close-loop tick.
Run every CI case at least three times because block generation and epoch
boundaries are timing-sensitive.

## Tier 0: configuration and smoke tests

| ID | Command | Purpose and required result |
| --- | --- | --- |
| C0 | `cargo test -p nanospam --features rai_protocol` | CLI parsing, RAI timing validation, generated config, status validation helpers, and process cleanup pass. |
| C1 | `nanospam setup --data-dir /tmp/rai-c1 --prs 1 --accounts 32 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` then `nanospam run --data-dir /tmp/rai-c1 --prs 1 --blocks 200 --rate 50 --accounts 32 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Single-member baseline. Setup funds the ledger and stops its temporary node; run restarts RAI at epoch 0. The full RAI oracle passes. |
| C2 | `nanospam setup --data-dir /tmp/rai-c2 --prs 6 --accounts 128 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` then `nanospam run --data-dir /tmp/rai-c2 --prs 6 --blocks 800 --rate 200 --accounts 128 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Multi-replica agreement, gossip, and deterministic cut/record construction pass. |

Run C1 before every fault-oriented batch. If C1 fails, later results are not
diagnostic.

## Tier 1: input and workload matrix

| ID | Command | Spec behavior exercised | Required result |
| --- | --- | --- | --- |
| W1 | `nanospam --data-dir /tmp/rai-w1 -- prs 6 --blocks 1200 --rate 300 --accounts 1 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Adjacent slot elections on one account; the next slot must not overtake its unresolved predecessor. | Full oracle passes; no divergent close hashes. |
| W2 | `nanospam --data-dir /tmp/rai-w2 -- prs 6 --blocks 1200 --rate 300 --accounts 1000 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Many disjoint account elections and canonical frontier-map construction. | Full oracle passes with equal per-epoch counts on every PR. |
| W3 | `nanospam --data-dir /tmp/rai-w3 -- prs 6 --blocks 3000 --rate 50+100@1s --accounts 512 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 50` | Load crosses several epoch boundaries while the offered rate changes. | Full oracle passes; extend the oracle to require all traversed epochs to close for a long-running version of this test. |
| W4 | `nanospam --data-dir /tmp/rai-w4 -- prs 6 --blocks 2000 --rate 1000 --accounts 512 --cps-limit 100 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Backlog at close-cut time and draining a non-empty frozen cut. | Full oracle passes; epoch 0 closes despite the CPS bottleneck. |
| W5a | `nanospam --data-dir /tmp/rai-w5 -- prs 6 --blocks 1000 --rate 250 --accounts 128 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Creates funded/open account chains for representative changes. | Passes. Preserve this data directory. |
| W5b | `nanospam --data-dir /tmp/rai-w5 -- prs 6 --blocks 1000 --rate 250 --accounts 128 --sync --change --unconfirmed --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Representative updates, stake snapshotting, and committee derivation. | Full oracle passes and all PRs install the same close hash. |
| W6 | `nanospam --data-dir /tmp/rai-w6 -- prs 6 --blocks 1500 --rate 500 --accounts 256 --unconfirmed --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Many simultaneously unresolved elections and out-of-order arrival without waiting for predecessor confirmation. | Full oracle passes. |
| W7 | `nanospam --data-dir /tmp/rai-w7 -- prs 6 --blocks 1000 --rate 250 --accounts 128 --drop-percentage 10 --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Eventual delivery under transient publish loss; delayed blocks are republished. | Full oracle passes. Compare with W8 to ensure recovery depends on retransmission. |
| W8 | `nanospam --data-dir /tmp/rai-w8 -- prs 6 --blocks 1000 --rate 250 --accounts 128 --drop-percentage 10 --no-republish --rai-epoch-duration-ms 5000 --rai-tick-interval-ms 100` | Negative control: violates reliable eventual dissemination. | A timeout/failure is expected, but PRs must never report conflicting installed cut or close hashes. |
| W9 | `nanospam --data-dir /tmp/rai-w9 -- prs 6 --blocks 2500 --rate 2000 --accounts 2048 --unconfirmed --cps-limit 50 --rai-epoch-duration-ms 3000 --rai-tick-interval-ms 3000` | Boundary timing with the slowest valid tick and a heavy drain. | The protocol must still close safely; allow a larger external test timeout. |

W5b uses `--sync`, so nanospam attaches to the persisted ledger state but starts
new node processes. Do not combine `--attach` with W5b unless the four nodes
were started separately.

## Tier 2: forks and release

Run these cases after adding the observability described below. The current
nanospam oracle requires `drain_finalized == drain_obligations`, whereas the
specification also permits an obligation to terminate by certified timeout or
conflict release. Consequently a correct release can currently be reported as
a test failure.

| ID | Input | Property to assert |
| --- | --- | --- |
| F1 | C2 plus `--fork-percentage 1` | Every conflicting slot has at most one finalized block; all PRs choose the same finalized hash or the same certified release. |
| F2 | C2 plus `--fork-percentage 100 --accounts 1 --unconfirmed` | Repeated conflicts cannot advance to a later account slot until the earlier slot is finalized or has certified release. |
| F3 | C2 plus `--fork-percentage 20 --drop-percentage 10` | Conflicting observations and transient loss converge without different correct replicas finalizing different tips. |
| F4 | Deliver opposite fork sides initially to two PR subsets, then heal delivery | A conflicting retry is accepted only after durable timeout/conflict release evidence; all replicas converge after healing. |

Required RPC additions for these cases:

- expose `drain_released_timeout` and `drain_released_conflict` counts;
- expose `(epoch, account, start_slot, outcome_hash/outcome_kind)` terminal slot
  outcomes, or provide a test-only event stream;
- expose current and maximum finalized slot per account; and
- make nanospam validate
  `finalized + released_timeout + released_conflict == obligations` rather than
  requiring every obligation to finalize.

## Tier 3: close rounds, durability, and reconstruction

These are necessary to cover the specification beyond the all-online happy
path. They require a controllable proxy/test transport or nanospam lifecycle
hooks; `--drop-percentage` only drops workload publishes and cannot partition
vote/report/close traffic.

| ID | Scenario | Required invariant/progress result |
| --- | --- | --- |
| R1 | Partition PRs after close-cut first votes so different cut hashes are preferred; heal later. | No conflicting close decision. The round either dies with durable evidence or carries one supported hash, and all PRs eventually install the same cut. |
| R2 | Force a non-timeout notarized close hash without a fast/final decision, then permit the next round. | The exact hash is carried into the successor round and eventually decided; fresh preference must not replace it. |
| R3 | Produce timeout/conflicting committee outcomes for a close round. | A later round starts only after persistent signed death evidence exists; a local wall-clock timeout alone is insufficient. |
| R4 | Restart one PR after it votes for a close hash but before decision. | Retained vote/preimage state is replayed; restart cannot cause a conflicting first vote or prevent closure. |
| R5 | Keep one PR from receiving the decided cut/frontier preimage while allowing it to receive the hash; then restore reconciliation traffic. | It reconstructs the canonical version, recomputes the hash, validates it, and installs the same close state. A corrupt delta is rejected. |
| R6 | Stop one PR for a complete epoch, let the others close it, then restart it. | The joining replica validates blocks, reports, vote batches, cut, and record from the certified history and reaches the same close hash/frontiers. |
| R7 | Run through at least three closures while changing representative delegation in each epoch. | Elections in epoch `e` use the specified frozen committees/governing close state; all PRs derive identical later committees. |

For R1--R7, capture at least these per-round fields: election kind, epoch,
round, preferred hash, carried hash, outcome, death-proof kind, first/notar/final
signer weights per effective committee, and installed close hash. Assertions must
compare durable hashes and outcomes, not log strings alone.

## Safety checks to run after every case

Query every PR before deleting its data directory and compare:

1. No pair of PRs has different non-empty `close_hashes[e]` or `cut_hashes[e]`
   for the same epoch.
2. `closed_through` never decreases and no epoch closes before its predecessor.
3. Finalized counts never decrease.
4. For each account slot, no two different hashes are finalized.
5. A finalized segment is contiguous from a certified base and its blocks are
   applied ancestor-to-descendant atomically.
6. Each installed close record's frontier map is reproducible from retained
   authenticated blocks, reports, and vote batches.
7. Ledger replay preserves signatures, balances, send/receive uniqueness,
   representative state, and total supply.

Checks 1--3 are mostly available from `rai_status`. Checks 4--7 require a
test-only audit RPC or an offline ledger/audit command and should be treated as
coverage gaps until implemented.

## Recommended automation order

Use C0, C1, C2, W1, W2, W4, W6, and W7 as the initial pull-request suite. Keep
W3, W5a/W5b, W8, and W9 as scheduled or nightly tests. Enable F1--F4 only after
release outcomes are observable. R1--R7 belong in a dedicated fault-injection
job because they need deterministic process/network control.

Each runner should impose an external timeout, terminate spawned nodes on
failure, archive each PR's logs and final `rai_status`, and retry only the whole
case. Never retry an individual protocol assertion inside a failed run, because
that can hide a genuine non-convergent execution.
