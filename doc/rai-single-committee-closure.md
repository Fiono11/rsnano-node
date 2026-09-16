# RAI compact closure with a single committee

This implementation uses the existing single weighted Kudzu committee. It does
not implement joint committees or claim safe arbitrary committee replacement
while overlapping epochs are still open. Nanospam supplies a fixed committee.
The existing continuously-online signing-state assumption remains in effect.

An epoch's target is the sorted set of certificate-verified candidate block IDs
for that epoch, including notarized forks. Its root is the root of a
three-level radix digest tree over the sorted members (`MembershipTrie` in the
ledger crate): 256 level-1 buckets by first byte, each a digest over the
non-empty leaves of its bucket (second byte), each leaf a
`Blake2b("RAI-CLOSE-LEAF" || epoch || prefix || members)`. The ledger keeps an
append-only log of recorded candidates per epoch, so the closer follows the
membership incrementally and recomputes only the digests on the changed path;
nothing copies the whole membership except the close itself.
Finalization certificates update candidate status without changing membership.
Earlier epochs, timeouts, and uncertified ledger dependencies are not members.
An empty epoch has a valid, epoch-specific root.

The signed close header includes the epoch, close round, round parent,
previous finalized close identity, and target root. The epoch-close identity is
`Blake2b("RAI-CLOSE" || epoch_le_u64 || previous_close || target_root)`.
Close rounds have no proposer. A drained replica announces the parent and
membership root it would close on; once certificate weight announced the same
pair, every draining replica that holds that membership FIRST-votes the value
it names, whether or not the pair is its own (rule C2 of `rai_protocol.tex`).
Replicas holding the same pair create the same candidate, so their votes tally
without a proposer. A new round requires a timeout certificate for the
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
8, signed, flooded and retransmitted with the close history): the tree root, the
member count and the 256 level-1 digests. Announcements never enter a tally. A
replica whose own root differs treats the announced root as a view and pulls its
pages from the announcer (kind 9 requests, answered by kind 5 level-2 pages and
kind 7 leaves): one level-2 page per differing bucket, one leaf per differing
prefix, at most 32 requests per tick and one request per page per two seconds.
Members of a received leaf that carry no local membership are solicited by hash
through ordinary epoch-specific ConfirmReq messages every two seconds until their
certificate is verified locally; a zero root makes the peer publish the block as
well. Both directions are covered because both replicas announce. Traffic is
therefore proportional to the difference: a differing member costs about three
pages, however large the membership.

A replica that holds a finalized close whose root it never held (it missed the
round, or held extra members) reconstructs the closed membership the same way:
peers keep the closed membership as a view and announce its root with the vote
archive until every representative acknowledged the close; the lagging replica
fetches the differing pages, solicits the members it lacks, excludes the members
absent from the fetched leaves, and applies the close once the assembled root
matches. There is no full-list transfer and no delta against a shared base.

Signing needs no membership scan: a candidate is signable when its root equals
the live root, or when the announced view with that root decoded against the
live membership names nothing this replica lacks, so every entry verifies
locally; a child candidate is signable when this replica held its notarized
parent's root, since members are only added. D3 and D4 are the announcers'
judgment: an announcement is made only when drained and names the full live
membership, and a quorum of them authorizes the value for the round. A voter
does not re-run them (rule C3): a FINAL vote follows the FIRST vote even when a
member arrived since, because a member the announcing quorum never saw cannot
be finalized once that quorum closed without it. The drain check reads a
per-epoch index of undecided elections instead of scanning every election of
the epoch.

The announced value is voted only once every representative announced the same
pair, or six seconds after this replica's own drain once certificate weight
has: the announcement phase of a round, which gives late certificates a chance
to enter the close instead of being discarded with it. A fresh snapshot that
differs from the announced root is announced instead of voted. Round 0 is timed
from the moment the vote became due on each replica rather than from the drain
start, so nobody FIRST-timeouts a round before its value can exist; later
rounds are timed from their timeout certificate as before. After the six-second wait the round timer
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

Drain stragglers observed with the termination audit come in one shape: the
two candidates of a fork reach the replicas around the drain boundary, one
replica lacks the second one, and every FIRST-timeout and TIMEOUT its peers
route through that hash is indeterminate there, so the election cannot
terminate until the block arrives. Under saturation each node drops two to
four thousand of the 45,000 published blocks at its block-processor queue,
and the drain solicitation, rotating through hundreds of pending elections at
two-second cadence, recovered such a block in about five seconds. The
minute-long stragglers were the same shape with the network duplicate filter
in the way: the first copy of the block had been received and lost, and
every copy a peer republished on request was a byte-identical duplicate until
the filter's 60-second cutoff. Three repairs: the hinted scheduler, which
already scans the vote cache every second for hashes with representative
weight and no block, requests a block neither the ledger nor an election
holds from the principal representatives with a zero-root ConfirmReq
(five-second cooldown per hash, independent of container vacancy); a
zero-root request makes the aggregator publish a block it holds even without
a certificate for it, since votes alone cannot help a requester that lacks the
block; and a duplicate publish is suppressed only for two filter epochs (five
to ten seconds), which covers the fan-in of one flood, since a received block
is never flooded on. With those, the largest gap between a replica's first
sight of one fork candidate and of the other in a run is about ten seconds
where it was over sixty.
