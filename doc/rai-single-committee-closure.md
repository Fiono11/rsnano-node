# RAI compact closure with a single committee

This implementation uses the existing single weighted Kudzu committee. It does
not implement joint committees or claim safe arbitrary committee replacement
while overlapping epochs are still open. Nanospam supplies a fixed committee.
The existing continuously-online signing-state assumption remains in effect.

An epoch's target is the sorted set of certificate-verified candidate block IDs
for that epoch, including notarized forks. Its root is
`Blake2b("RAI-CLOSE-STATE" || epoch_le_u64 || sorted_ids)`.
Finalization certificates update candidate status without changing membership.
Earlier epochs, timeouts, and uncertified ledger dependencies are not members.
An empty epoch has a valid, epoch-specific root.

The signed close header includes the epoch, close round, round parent,
previous finalized close identity, and target root. The epoch-close identity is
`Blake2b("RAI-CLOSE" || epoch_le_u64 || previous_close || target_root)`.
Each close round chooses the next representative in public-key order. Its
leader's authenticated FIRST statement proposes the target; followers first-vote
only after reconstructing it. A new round requires a timeout certificate for the
previous round. In particular, a notarized ancestor is not silently selected as
the finalized target when a descendant finalizes: doing so could omit candidates
added in the descendant. Reconstructed parent states must be subsets of children.
Advertised member counts do not gate progress.

At the epoch deadline, signing stops issuing new non-timeout FIRST votes in that
epoch. Old elections continue, with FIRST-timeout for new participants. D3 covers
local non-timeout first votes and all locally visible elections, including an
election with just one valid first vote. D4 requires the reconstructed target to
include every locally known notarized candidate. The signing reservation and
election read lock cover these checks and close signing, preventing concurrent
candidate ingestion from invalidating the checks mid-vote. D4 is not cached.

A drained replica announces its membership before anyone proposes (message kind
8, signed, flooded and retransmitted with the close history): the target root,
the member count and one digest per member bucket, where a bucket is the sorted
members sharing a first byte. Announcements never enter a tally. A replica whose
own root differs answers the announcer directly with the members of every bucket
whose digest differs (kind 9, at most 32 buckets per announcement, the rest on
its retransmissions), and answers every stored announcement again whenever its
own membership changes. Members received this way that carry no local
membership are solicited by hash through ordinary epoch-specific ConfirmReq
messages every two seconds until their certificate is verified locally; a zero
root makes the peer publish the block as well. Both directions are covered
because both replicas announce.

The round leader proposes only once every representative announced the root it
holds itself, or six seconds after its own drain once representatives holding
certificate weight have; a fresh snapshot that differs from the announced root
is announced instead of proposed. Round 0 is timed from the moment a proposal
became due on each replica rather than from the drain start, so followers do not
FIRST-timeout a round before its proposal can exist; later rounds are timed from
their timeout certificate as before. After the six-second wait the round timer
is armed even without certificate-weight agreement, so rounds keep rotating
while reconciliation continues. Close statements, announcements and pages use
their own outbound traffic class (`TrafficType::EpochClose`): on the shared
vote-reply queue a burst of recovery replies to one peer dropped an announcement
on that channel repeatedly, so the leader never saw agreement and round 0 was
lost even without forks. With six replicas and 5% forks at 2000 blocks/s
both epochs close in round 0 within about five seconds of the drain, where the
previous design needed two to three rounds and 14-27 seconds: memberships that
differed by 1-25 members at readiness could only converge through blind
solicitation, and a proposal is immutable, so every member learned after it made
the proposal unsignable for that replica.

Two subtleties observed on the way: a candidate with second-look FIRST weight is
not certain to be notarized, because representatives that already cast FINAL for
the other candidate are value-locked (about half of the 3-3 forks end with one
side locked), so readiness must not wait for such certificates; and late votes
of the draining epoch can open new local elections after readiness, which D3
then blocks until they terminate, so a replica may miss round 0 and reconstruct
the finalized target from the empty version instead.

Reconciliation requests name one target and up to eight candidate bases, the
requester's retained snapshot history newest first, with epoch and bounded page
routing metadata. The serving replica answers against the largest base it also
holds that is a subset of the target, and the requester pins that base for the
remaining pages. A live state that already exceeds the target can therefore
still be served through an earlier version. An unknown base or non-subset base
gets no delta. Responses are authenticated, contain
no block bodies or proofs, and are accepted only against a pinned immutable base
and a matching recomputed target root. Object and certificate recovery uses the
ordinary publish/confirm-ack path; locally verified certification is required
before close voting. Certificate collection is separate from election activation:
a block already notarized in an earlier epoch can acquire its independently
verified current-epoch certificate without reopening the slot or signing again.
Ordinary dissemination and refreshed local snapshots permit
retry and convergence without a second set-reconciliation protocol.

Each epoch retains its empty-state version. A finalized target is reconstructed
in an independent accumulator starting at that version, using the same additive
request/response mechanism. This does not delete live candidates or waive D4 for
voting. It allows a lagging replica to reconstruct a fixed finalized target even
when its live-state version was never held by the serving replica.

Learning a close certificate fences new signatures immediately, even while its
target is being retrieved. Existing signed votes remain available for recovery;
recovery does not require signing again after the fence. After reconstruction,
the close persists epoch membership and the previous-close link, preserves included
notarized forks, and discards omitted candidate state. Only omitted attempts
release their cross-epoch first-vote reservations. Included roots cannot receive a
new non-timeout first vote in another epoch. Voting entry checks require closes
through e-2, and sealed omissions are rejected as parents of later proposals.

The new root/header format is a protocol change. Benchmarks use fresh ledgers.

Epochs can alternatively advance by `epoch_terminated_elections` in node TOML,
or `--epoch-terminated-elections N` in nanospam. This is mutually exclusive with
nonzero timed `epoch_length`. Each distinct `(slot, epoch)` contributes once on
its first locally verified notarization or timeout; further notarized forks,
replayed certificates and later finalization do not increment it. Checkpoints
are cumulative: epoch e begins draining at `(e + 1) * N` terminations. Outcomes
arriving during a drain count toward the next checkpoint, avoiding an overshoot
that would strand a finite workload. Nanospam excludes pre-start setup outcomes.
This counter shares the continuously-online assumption of the voting state.

Drain recovery includes locally signed FIRST obligations even when no active
election remains. A reconstructed snapshot names exactly the members missing
locally, and those are requested directly; a request whose root is zero names a
block the requester does not hold, so the peer publishes it. Blind solicitation
of notarized elections and their forks runs only while a proposal has no delta
page flow, since memberships that differ in both directions share no base;
soliciting them on every drain tick would amplify replies and drops. Archived
signatures are replayed for explicitly requested IDs,
and certificate-only collection recognizes timeout as well as block outcomes.
A final value lock from another epoch cannot suppress a FIRST-timeout or eligible
timeout in this epoch; it still prohibits endorsing a conflicting block.
