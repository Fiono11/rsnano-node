# Local RAI epochs

Build both the node and its protocol clients with `--features rai_protocol`.
This experimental mode separates voting epochs from canonical ledger epochs.
Nano account-upgrade `Epoch` values are unrelated. Block elections use the
[weighted RAI voting rules](rai-voting.md).

```
cargo build --release -p rsnano_cli -p nanospam --features rai_protocol
PATH="$PWD/target/release:$PATH" target/release/nanospam \
  --data-dir /tmp/rai-nanospam --prs 6 --no-prio \
  --accounts 50000 --blocks 50000 --rate 2000 --epoch-length 25 --fork-percentage 5
```

RAI nanospam fixes the voting committee before setup using
`NANOSPAM_RAI_COMMITTEE` on the test network. Every PR has the same immutable
weight, `floor(Amount::MAX / prs)`, independently of ledger transfers, online
sampling, and block height. All representative keys are installed before funding.
For six PRs the certificate quorum requires four PRs and fast finalization requires
five. `RAI_QUORUM_SNAPSHOT` verifies the fixed weight base and discovered committee
weights at startup, workload start, and the end of observation.

`node.epoch_length` is a duration in seconds. Zero disables epoch advancement.
Nanospam writes one future Unix start timestamp atomically to `epoch-start-ms`
after setup. All local PRs read the same file through
`NANOSPAM_RAI_EPOCH_START_FILE` and map it onto their monotonic clocks. Scheduled
epoch deadlines are `start + (e + 1) * duration`; completing a close does not reset
or shift that schedule. Local drain and consensus completion times can differ.
Outside nanospam, the default epoch clock starts when the node is constructed.

When the scheduled deadline passes and the preceding epoch is closed, new
participation in epoch-e block elections uses
FIRST-timeout. Existing non-timeout FIRST votes retain their normal voting rules.
The PR waits until every epoch-e election in which it signed a non-timeout FIRST,
and every epoch-e election with more than f weight in distinct non-timeout FIRST
votes (including elections in which this PR first voted timeout), has a
notarization, finalization, or timeout certificate. FIRST weight is counted across
candidates, once per representative; with f = 19% and six equal PRs, two FIRST
voters suffice. It then starts Close(e)
and starts new block elections in e+1. Certificates in e+1 can form immediately,
including fast certificates, but their cementation waits for Close(e).

Close rounds use a rotating leader, selected from sorted committee public keys.
The leader proposes its latest valid epoch snapshot extending its retained
notarized parent. Replicas FIRST-vote that exact validated proposal, or
FIRST-timeout after the round deadline. Close-round timeouts grow from 3 seconds
to a maximum of 192 seconds on retries, and begin at local round entry. Second-look, notarization, final voting,
and timeout thresholds reuse the block-election tally rules. A notarized proposal
advances the round and becomes the retained parent; a timeout certificate permits
skipping the round. Per-round signing state and certificates are retained so
messages from earlier rounds can still establish a decision. Parent ancestry must
be notarized, valid, and extended by the child's snapshot.

A finalized descendant commits its ancestors. The oldest snapshot in the committed
close chain is the unique Close(e) decision; descendants cannot replace it with a
different epoch assignment. Snapshot hashes cover sorted block hashes eligible
through certificates from e or earlier, plus dependencies and prior closed state.
Snapshot pages and signed votes use a separate network message and timer. A
successfully validated immutable candidate is cached during that close election.

Voting certificates keep their original `(qualified root, voting epoch)` identity.
Cementing a dependency does not manufacture a final certificate for its own
root/epoch. Provisional dependency metadata can decrease when earlier evidence
arrives. Close(e) separately persists immutable canonical epoch assignments for
newly included blocks, together with the chosen digest. Blocks absent from the
chosen snapshot remain unassigned until a later close. Before committing a close,
the node asserts that no finalized epoch-e block or election was omitted. It then
discards omitted epoch-e elections under the vote-processing lock; late votes
cannot reopen these elections. Later-epoch elections remain available.

The `block_count` RPC exposes `current_epoch`, `draining_epoch`, and
`closed_epochs` without enumerating ledger block sets. The timer also does no
ledger scan. Ledger reads needed to construct and validate actual close snapshots
remain, including dependency validation and the omitted-finalization assertion.
No minimum block count is required, so underfilled and idle epochs can close.
Canonical assignments and close digests survive restart; close-round signing
state and certificate retransmission remain in memory, so voting currently assumes
continuous operation.

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

With timed epochs enabled, the benchmark observes the highest current epoch across
PRs at the end of the workload, then waits for every epoch through that target to
close with the same hash on all PRs. `EPOCH_CLOSE_RESULT` records this check. Newer
idle epochs do not extend the target. It does not scan ledger sets or require
agreement on discarded/open-epoch notarization sets. Ordinary block-tree and
termination checks remain available when timed epochs are disabled.

The initial workload funding send and receive are fork-free; later workload blocks
use the configured fork probability.


Close recovery uses signed receipts (message kind 6), separate from consensus
votes. Full snapshot manifests are replayed at most every two seconds, and the
previous close archive is released after every weighted committee member
acknowledges the same persisted close hash. Receipts alone cannot finalize a
close. FIRST-timeout participants may subsequently notarize under second-look
rules (up to the normal three-candidate limit), but may never issue FINAL in
that epoch. Locally generated votes use a bounded queue with backpressure and
reserved processing capacity, independent of droppable remote/cache traffic.
Optional RAI_CLOSE_TRACE_DIR tracing records each certificate transition once;
full election details are only dumped for an omitted-finalization assertion.
