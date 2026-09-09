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
  --accounts 50000 --blocks 50000 --rate 2000 --epoch-length 25000 --fork-percentage 5
```

RAI nanospam fixes the voting committee before setup using
`NANOSPAM_RAI_COMMITTEE` on the test network. Every PR has the same immutable
weight, `floor(Amount::MAX / prs)`, independently of ledger transfers, online
sampling, and block height. All representative keys are installed before funding.
For six PRs the certificate quorum requires four PRs and fast finalization requires
five. `RAI_QUORUM_SNAPSHOT` verifies the fixed weight base and discovered committee
weights at startup, workload start, and the end of observation.

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
slot creates a separate election with its own tally. For a known active root, votes in a newer epoch can also create a separate
election so that later finalization can supersede earlier notarization. Existing matching elections continue accepting their
own votes. Admission is bounded by AEC capacity; peer messages never advance the
local epoch counter. Ordinary confirm requests recover votes for elections.

Tallies, replay checks, voting schedules, and request schedules are separate per
election. The vote cache retains statements separately by representative and
epoch and uses the strongest single epoch for scheduling hints. Final-vote locks
remain durable and keyed by qualified root across all epochs: a representative
cannot final-vote different forks by changing the epoch. Cementation records implicit finalization without discarding the independently
collected evidence of same-root elections. Earlier epochs can still establish an
earlier canonical assignment.

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
counts, elapsed workload time, average confirmation latency (overall, fork and
non-fork), and confirmed blocks per elapsed second. Latency measures publication
to the first winning-candidate WebSocket confirmation on PR0, counting each
workload root once. The workload observation window is at least 60 seconds and continues until all
requested blocks have been published, with a five-minute hard limit. Reported
throughput uses the actual full window and excludes node/wallet setup.

After the performance window, RAI allows up to 120 seconds of passive recovery
observation, using a common timestamp cutoff and incremental audits. The nodes
continue using ordinary votes and confirm requests. `CANONICAL_AGREEMENT_RESULT`
reports the additional observation interval separately from performance metrics.
This does not close epochs or prove that no future evidence can change the outcome.

`TERMINATION_RESULT` checks canonical termination/finalization outcomes as
specified in [RAI voting](rai-voting.md#nanospam-canonical-state-agreement).
Roots pending on every PR are reported separately and make the run fail. Every
requested root must have a canonical block or timeout certificate on every PR.
`ELECTION_RESULT` reports workload election counts per PR and epoch identity,
including `timeout_notarized`, fast, non-fast explicit, and implicit finalization.
These count `(qualified_root, epoch)` identities, so one workload root may appear
in several epochs. A certified timeout terminates an epoch without cementing a
block or advancing the local epoch counter. The optional audit output preserves
complete events for analysis. Active diagnostics default to 256 unfinished
elections; opt-in `NANOSPAM_DIAGNOSTIC_LIMIT` can raise the limit up to 10,000.
