# RAI weighted election voting

The `rai_protocol` build uses a limited Kudzu-inspired adaptation inside the existing
`(qualified_root, epoch)` elections. It does not introduce global slots, leaders,
proposal-timeout timers, erasure coding, or committee transitions.
It supports notarized timeout outcomes within each root/epoch. The feature-disabled
build keeps the legacy election rules and wire format.

`W = max(online_weight, trended_or_min_weight)` is the same total used by the legacy
quorum calculation, before multiplying by its quorum percentage. Both `f` and `p`
are 19% of W. Thresholds use integer raw amounts without intermediate overflow:

| Evidence | Required weight |
| --- | --- |
| Block or timeout notarization certificate | ceil(62% W) |
| Finalization certificate | ceil(62% W) |
| Fast certificate | ceil(81% W) |
| Second look | floor(38% W) + 1 raw |

A zero W cannot produce a certificate. Nanospam fixes representative weights and W
for the entire run, including setup, using the explicit equal-weight committee
described below. RAI deployments without that test committee retain the legacy
weight-update behavior. This adaptation does not implement agreed committee
transitions or the full Kudzu safety/liveness protocol.

## Statements and confirmation

`First`, `Notarize`, `Final`, `FirstTimeout`, and `Timeout` are explicit signed vote kinds. A first vote also
counts as notarization for that candidate. The generator never emits a separate
notarization for its first candidate. A representative may first-vote only once
per election, and may notarize another available, validated candidate after the
second-look threshold. At most three distinct notarized candidates are retained
per representative. Separate phase records prevent double counting or timestamp
replacement across phases, including when final votes arrive before first votes.
A final vote also contributes notarization weight for its candidate; it does not
contribute first-vote weight.

A candidate with a notarization certificate becomes eligible for final voting.
A local representative may final-vote it only if that representative has not
notarized any other candidate. Confirmation requires either a fast certificate,
or both notarization and finalization certificates for the same candidate.
Certificates are locally collected sets of authenticated votes, with distinct
representatives counted once; there is no new certificate network message.
New certificate evidence is relayed as the original signed votes through the
existing broadcaster. Finalized election evidence remains available in memory.
Confirm requests and vote recovery retain their existing wire mechanisms.

Second-look candidates are submitted to the existing winner/fork processing and
vote generation paths. Generation requires the full block in the ledger with
confirmed dependencies. The existing candidate cap and replacement flow remain. Phase restrictions
survive candidate replacement so an evicted first vote cannot be changed.

## Requests, recovery, and epochs

`confirm_req` is unchanged: a batch epoch plus `(hash, root)` pairs. It does not
request a particular phase. The reply generators recover eligible first/
notarization and final statements, sending different kinds in separate
`confirm_ack` batches. The signed RAI vote includes one kind byte after the epoch.
Vote caches, local history, and rebroadcast history retain phases separately.
Request eligibility also uses those phase records: receiving a final statement
satisfies that representative’s notarization contribution. A late first vote
cannot downgrade the legacy final-vote summary. First recovery uses the normal
reply queue so it can proceed while the final generator waits for the ledger
writer. Eligible first votes are also recovered after local fast confirmation.
First and notarization statements use a stable zero timestamp (with the usual
duration bits): their signed kind, epoch and value identify the immutable
statement, and retransmissions can use ordinary duplicate filtering. An active
final-vote job avoids re-emitting an already-issued first vote.
A request also replays retained certificate-supporting votes and publishes their
candidate blocks, using the existing message formats. Relayed votes carry the
existing rebroadcast flag so representative discovery cannot mistake the relaying
channel for the original signer. Replies include evidence
from the requested epoch and the locally canonical epoch. Notarized elections
continue requesting evidence even when their current winner has enough votes,
so they can discover additional certified forks. A vote can attach a candidate
already known in another epoch to the requested election; the existing vote
cache replays statements when that candidate is added. Extra certificate replies
are deduplicated across batches/workers and retried at most once per peer every
five seconds; ordinary generated replies keep their existing scheduling. Full admission capacity
does not prevent repairing retained elections or opening another epoch for a
known root. Epochs of one root share an admission slot; notarization releases
that slot. All epochs remain available for voting and recovery, and their
certificate states remain independent.

Final choices are reserved in memory before writing the existing ledger lock;
the shared signing mutex is released before waiting for the ledger writer.

First-vote and notarization signing restrictions are held in memory for the node's
lifetime, shared by request and broadcast generators. No new ledger database is
created, and full votes/certificates are not persisted. This implementation assumes
nodes do not crash, restart, or go offline during the experiment.

The first participation epoch, first candidate and notarized candidates are retained
per representative and qualified root across election eviction and epochs. First
participation in another epoch emits a signed `FirstTimeout` vote, even for the
same candidate. Its block hash identifies the election; it contributes no block
notarization or fast-finalization weight, but contributes to timeout notarization.
This cross-epoch abstention is not a timer-driven epoch transition.
Second-look notarization remains allowed after timeout, but final voting in that
epoch is prohibited. Earlier termination evidence is still accepted.
Additional notarization in an epoch requires a first vote (including timeout).
Reissuing an authorized notarization/final statement uses its original epoch; it
cannot authorize that phase in another epoch. Final voting retains the existing
legacy `final_votes` persistence.

Election identities, local epoch advancement, canonical confirmation-epoch
assignment, and cementation remain as before. A timeout certificate terminates an
epoch without confirming a candidate, changing account frontiers, or advancing the
cemented-block-based epoch counter.

`FirstTimeout` (wire kind 3) occupies the signer's FIRST position and also contributes
a timeout notarization. `Timeout` (wire kind 4) contributes only timeout notarization,
so it can arrive after, or before, the signer's original FIRST without replacing it.
Timeout weight is deduplicated by representative across both kinds and candidates.
The signed candidate hash identifies the root/epoch; it does not endorse that block.
As in Protocol 1, lines 32–35 of `kudzu.pdf`, a replica that has already first-voted
can issue a later timeout when total FIRST participation minus the largest
non-timeout FIRST tally reaches `floor(38% W) + 1 raw`. FIRST-timeouts count in the
total but not in the maximum. Later timeouts count in neither FIRST quantity.
A representative that signed a timeout in an epoch cannot final-vote in that epoch;
a final reservation or durable final lock also prevents a new timeout signature.
Timeout certificates use the notarization threshold, `ceil(62% W)`, release admission
capacity, and retain signed evidence for recovery. A certificate does not become a
block confirmation. The local-epoch adaptation still does not implement the paper's
full leader/slot/timer protocol.

The RAI network identifier and vote signature domain have changed to separate
these votes from the previous RAI encoding. All peers in a test network must use
the updated build. Use a fresh test ledger: the old RAI vote encoding and voting rules are incompatible with this experiment.

## Performance counters

The RAI stats response includes `election/kudzu_fast_confirmed` and
`election/kudzu_slow_confirmed`. These count election confirmations, including
setup and earlier-epoch recovery; they are not distinct workload-block counts.
Eligibility and active-hash checks are batched under the election lock. A bounded
confirmed-epoch cache avoids repeated ledger reads for known late votes while
preserving earlier-epoch recovery. These optimizations do not change thresholds,
request intervals, or epoch advancement.

## Nanospam canonical-state agreement

RAI nanospam supplies an explicit, equally weighted committee through
`NANOSPAM_RAI_COMMITTEE`. The test-network-only override uses the existing bootstrap
weight cache for the entire node lifetime and sets the quorum base to the sum of
those fixed weights. Ledger transfers, account balances, and peer discovery do
not change vote weights or thresholds. All committee keys are installed before
funding, so this also applies to setup elections. With six PRs, certificates
require four PRs, fast finalization requires five, and second look requires three
first votes. `RAI_COMMITTEE` records the identities and exact weights in the run log.
Dynamic committee membership and epoch stake changes are outside this experiment.

For each workload qualified root, finalization supersedes notarization in any
epoch. The canonical epoch is the earliest finalized epoch observed on any PR
before the cutoff, or the earliest block-notarized epoch if no epoch finalized,
or the earliest timeout-certified epoch if no block certificate exists. Every PR must have the same outcome in that epoch. If any PR finalized,
all must finalize the same value; finalization supersedes notarization-set
comparison. Otherwise every PR must possess the same set of notarized candidate
hashes. Different valid signer subsets are equivalent certificates. Later-epoch
outcomes cannot satisfy missing canonical-epoch evidence. Conflicting finalized
values are rejected across all epochs. A timeout certificate and block finalization
in the same epoch also fail the check. When timeout is canonical, every PR must
possess its certificate; the candidate hashes used to route timeout votes do not
create distinct timeout outcomes. Roots without a block or timeout certificate are
reported as `pending_roots` and make both `success` and `all_workload_terminated`
false. Pending roots keep passive recovery running and cause exit 1 if unresolved.
`timeout_roots` counts canonical timeout outcomes. Audit event kind 7 records a
verified timeout certificate with a zero candidate hash; individual timeout votes
never count as termination.

The check is an observed-run invariant, not an epoch-close protocol. Earlier evidence or a later finalization arriving after the cutoff may still
change the canonical outcome. The experiment assumes continuous operation and retains election evidence
in memory; it does not provide crash recovery or bounded long-term archival.

### Latency profiling

The opt-in nanospam termination audit also records the first accepted FIRST or
FIRST-timeout vote (kind 8), second-look eligibility (9), timeout eligibility
(10), and first accepted final, timeout, and notarization votes (11, 12, 13).
These are diagnostic events, not certificates. Existing termination event kinds
and the strict all-root, all-PR checker are unchanged. Events are deduplicated
per kind, root, candidate, and epoch; the audit retains its existing size bound.

For latency optimization, measure nonfork roots to finalization and fork roots
to their first verified block or timeout notarization. Start at the earliest
local election insertion across epochs, and report missing finalizations as
censored roots alongside the distributions. A timeout alone does not count as
nonfork finalization. Publication-to-WebSocket confirmation includes additional
transport, admission, cementation, and notification time.

The voting scheduler gives a newly eligible timeout its own retry record, so it
does not wait for the FIRST retry interval. Repeated timeout votes remain rate
limited. Second-look notifications load representative keys once per batch,
then apply the existing signing restrictions to each candidate. Vote replays
skip certificate and audit processing because they cannot change the tallies.

Nanospam accepts `--vote-generator-delay-ms <milliseconds>` to override the
node's batching delay in a controlled run; omitting it uses the node default.
`workload-results/rai-latency/run.py` runs the six-PR workload with fresh data and
cleanup, and `analyze.py` writes per-PR and per-epoch latency distributions from
the saved audit. The runner accepts blocks, rate, output label, and an optional
batching delay, in that order. Keep performance-cutoff agreement and later
recovery results separate when comparing settings.
