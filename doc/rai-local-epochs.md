# Local RAI epochs

Build both the node and its protocol clients with `--features rai_protocol`.
This experimental mode adds local consensus epochs; it does not implement epoch
transition consensus. Nano account-upgrade `Epoch` values are unrelated.

Election voting now uses the [weighted RAI voting rules](rai-voting.md), including
in-memory first-vote restrictions across epochs. The epoch metadata behavior below
is unchanged.

```
cargo build --release -p rsnano_cli -p nanospam --features rai_protocol
PATH="$PWD/target/release:$PATH" target/release/nanospam \
  --data-dir /tmp/rai-nanospam --prs 6 --no-prio \
  --accounts 50000 --blocks 50000 --rate 2000 --epoch-length 25000
```

`node.epoch_length` configures newly cemented blocks per local epoch. Zero disables
advancement. Genesis is epoch 0 and excluded from the counter; representative
setup blocks and implicitly cemented dependencies count. The persistent counter
and threshold determine the current epoch. Changing the threshold after counting
blocks requires a fresh ledger, so restarting cannot reinterpret past progress.
The `block_count` RPC includes `current_epoch` in a RAI build.

New elections use the local current epoch. Their identity is `ElectionId`, a
qualified root and consensus epoch. Existing elections keep their epoch across
advancement. Votes bind their epoch into their signature; votes and confirm
requests contain one epoch for the entire batch. Queues and solicitation batch
only entries with the same epoch. A valid vote for an earlier epoch of a known
slot creates a separate election with its own tally. A vote in a newer epoch does
not create another election. Existing matching elections continue accepting their
own votes. Admission is bounded by AEC capacity; peer messages never advance the
local epoch counter. Ordinary confirm requests recover votes for elections.

Tallies, replay checks, voting schedules, and request schedules are separate per
election. The vote cache retains statements separately by representative and
epoch and uses the strongest single epoch for scheduling hints. Final-vote locks
remain durable and keyed by qualified root across all epochs: a representative
cannot final-vote different forks by changing the epoch. Cementation ends
same-root dependency elections in the confirming epoch; elections in other
epochs retain their own state and can establish an earlier canonical assignment.

The ledger stores `block hash -> confirmation epoch` in the `consensus_epochs`
LMDB database, atomically with confirmation height and the counter. Block bodies,
block hashes, and account-upgrade metadata do not change. The cementing queue
retains the original election epoch, including through deferral. Dependencies
cemented through that election inherit its epoch. The canonical epoch is the
minimum confirmed epoch: confirmation in epoch 2 followed by confirmation in
epoch 1 changes the record to 1. Earlier confirmation also lowers dependencies
whose assignments are later. Neither lowering nor duplicate confirmation
increments the cemented-block counter or advances the local epoch. Existing confirmed blocks without the new metadata are read as
epoch 0. `Ledger::confirmation_epoch` returns `None` for unconfirmed blocks.

There are no epoch advertisements or epoch-transition consensus. Earlier votes
are processed even after the block has been cemented: they create an earlier
election, which must independently reach quorum before lowering the canonical
assignment. Neither one vote nor a lower epoch number alone changes the ledger.

PRs converge when they receive the relevant earlier votes and confirm the earlier
elections. They can temporarily differ during propagation. There is no epoch
closure: a canonical assignment can decrease after a later-arriving earlier
confirmation. Bootstrap transfers ordinary blocks, and confirmations are locally
validated.

The RAI `block_count` response includes `confirmation_epochs`, with a count and
Blake2 digest of each epoch's sorted block hashes. This checks equal block sets,
not merely equal block counts. Existing ledgers' implicit epoch-0 records are
readable through `confirmation_epoch`; set digests enumerate explicit records,
so the benchmark uses fresh ledgers.

RAI changes the network header identifier, preventing connections to the ordinary
protocol. Every node and binary protocol client in a test network must use the
same build mode. The feature-disabled build retains ordinary vote/request wire
formats and uses epoch 0.

Nanospam prints a `BENCHMARK_RESULT` JSON record with created/confirmed block
counts, elapsed workload time, average confirmation latency, and confirmed blocks
per elapsed second. It also logs every PR's final ledger counts. Latency is the
existing nanospam publication-to-WebSocket-confirmation measurement on PR0;
throughput includes workload injection and final draining, but excludes node and
wallet setup. RAI then passively checks for matching per-epoch block-set digests
on every PR for three stable polls, and prints `EPOCH_SET_AGREEMENT_SECONDS`.
This check does not initiate protocol work; its additional observation time is
excluded from the first-confirmation throughput measurement. Runs with a fixed publication target measure achieved throughput
at that target, rather than maximum node capacity.
