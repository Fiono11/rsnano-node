# RAI weighted election voting

The `rai_protocol` build uses a limited Kudzu-inspired adaptation inside the existing
`(qualified_root, epoch)` elections. It does not introduce global slots, leaders,
timeout certificates, erasure coding, or committee transitions. The feature-disabled
build keeps the legacy election rules and wire format.

`W = max(online_weight, trended_or_min_weight)` is the same total used by the legacy
quorum calculation, before multiplying by its quorum percentage. Both `f` and `p`
are 19% of W. Thresholds use integer raw amounts without intermediate overflow:

| Evidence | Required weight |
| --- | --- |
| Notarization certificate | ceil(62% W) |
| Finalization certificate | ceil(62% W) |
| Fast certificate | ceil(81% W) |
| Second look | floor(38% W) + 1 raw |

A zero W cannot produce a certificate. Representative weights and W retain the
legacy update behavior; they are not a new agreed committee snapshot. The full
Kudzu safety/liveness proof therefore does not establish this adaptation's security.

## Statements and confirmation

`First`, `Notarize`, and `Final` are explicit signed vote kinds. A first vote also
counts as notarization for that candidate. The generator never emits a separate
notarization for its first candidate. A representative may first-vote only once
per election, and may notarize another available, validated candidate after the
second-look threshold. At most three distinct notarized candidates are retained
per representative. Separate phase records prevent double counting or timestamp
replacement across phases, including when final votes arrive before first votes.

A candidate with a notarization certificate becomes eligible for final voting.
A local representative may final-vote it only if that representative has not
notarized any other candidate. Confirmation requires either a fast certificate,
or both notarization and finalization certificates for the same candidate.
Certificates are locally collected sets of authenticated votes, with distinct
representatives counted once; there is no new certificate network message.

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
never suppresses a request for missing notarization weight. A late first vote
cannot downgrade the legacy final-vote summary. First recovery uses the normal
reply queue so it can proceed while the final generator waits for the ledger
writer. Eligible first votes are also recovered after local fast confirmation.
First and notarization statements use a stable zero timestamp (with the usual
duration bits): their signed kind, epoch and value identify the immutable
statement, and retransmissions can use ordinary duplicate filtering. An active
final-vote job avoids re-emitting an already-issued first vote.
Final choices are reserved in memory before writing the existing ledger lock;
the shared signing mutex is released before waiting for the ledger writer.

First-vote and notarization signing restrictions are held in memory for the node's
lifetime, shared by request and broadcast generators. No new ledger database is
created, and full votes/certificates are not persisted. This implementation assumes
nodes do not crash, restart, or go offline during the experiment.

The first candidate and notarized candidates are retained per representative and
qualified root across election eviction and epochs. This conservative cross-epoch
restriction prevents an epoch change from enabling a conflicting fast vote.
Additional notarization in an epoch requires having first-voted in that epoch.
Reissuing an authorized notarization/final statement uses its original epoch; it
cannot authorize that phase in another epoch. Final voting retains the existing
legacy `final_votes` persistence.

Election identities, local epoch advancement, canonical confirmation-epoch
assignment, and cementation remain as before. No timeout/retry protocol is added:
split votes or the conservative signing locks can leave an election unconfirmed.

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
