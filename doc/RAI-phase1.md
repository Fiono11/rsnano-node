# RAI Phase 1

4 October 2026

## Current revision: v1.3 one-shot composition

The current PDF replaces the repeated per-rank protocol. Fast signatures and instance hashes now use v1.3 domains; signing and FC/RC verification reject nonzero fast attempts, and pools can only be created for attempt zero. The journal retains its original vote when later preferences change.

`fallback.rs` selects the smallest 3f+1 public keys and binds them to a fresh slow instance. Only those members may sign initial slow proposals, each backed by independently verified RC0; slow quorum is 2f+1. Admission needs no timeout. A regression admits fallback before fast quorum and then verifies a late FC0 with the existing pool.

The one-shot admission change passed twenty focused tests and 925 feature-on library tests. The next certificate-validation slice is described below. The recursive request verifier and proposer loop are now implemented as described below; mutable responders and proof relay are now integrated in a transport-independent replica. The synchronizer, wire envelopes, application adapter and node service remain outstanding. Disk persistence remains deferred. The records below describe historical commits; their per-rank v1.2 rules are superseded by this revision.

## Slow response certificates and phase transitions

`slow_certificates.rs` now signs phase-specific response snapshots and checks exact 2f+1 quorums in the selected slow committee. Signatures bind the slow instance, response rank, phase, triggering request, signer and each carried value/origin. Every carried origin needs its own 2f+1 distinct authenticated responses; f+1 weak admission witnesses do not make a response eligible. Duplicate, extra, missing or mismatched witness attachments are rejected. Witness responses attest receipt of their triggering broadcast and need not carry its input value.

R results compare (rank, value), preserving a selected rank above the triggering rank. A/B certificates use the existing evaluators. A verified result yields the next A, B, next-rank R or decision action; recovery is not an input to that transition. Rank overflow is rejected. These types have serde round trips but are not wire envelopes.

`SlowRequestEvidence` is the interface for complete authenticated request ancestry and W-admission checks. The initial certificate-component tests use an exact known-request fixture. The implementation described next replaces that fixture with recursive slow ancestry for the proposer tests; the synchronizer remains outstanding. A verified token under the original fixture is not a production checkpoint proof.

Validation: 26 focused checkpoint tests, 931 feature-on library tests (718 node, 82 messages, 131 types) and a feature-off node library build; see [the validation record](phase1-slow-certificates-validation.json). Gate B remains pending.

## Recursive slow requests and proposer loop

`slow_requests.rs` supplies signed, content-addressed requests and a recursive proof-store verifier. Initial R requests end at verified RC-backed proposals. Later R, A and B requests must match the preceding certificate's recomputed next action, including selected rank, value and flag. A request uses W complementing witnesses; an eligible response certificate uses Q. Weak validation never populates a strong-certificate cache. Missing dependencies are retryable; depth/object budgets and active-path cycle detection bound traversal. Request caches live only within a read-only verification session.

`slow_proposer.rs` implements the in-memory request/response loop. It retains authenticated historical snapshots, selects distinct eligible responders, assembles proof attachments, advances through adoption and emits a decision certificate on commit. R certificates may additionally carry a signed retained local maximum with its origin witnesses, so the first response quorum cannot erase local R state. This implementation waits for Q witnesses for that retained origin; eventual availability through relay remains a progress obligation. Unknown or invalid ancestry cannot advance the loop.

Tests now traverse actual slow request ancestry, rather than the known-request fixture: uniform R/A/B decision; conflicting A inputs followed by adoption and a decision at the next rank; weak-versus-strong validation; missing dependencies and retry; signed-field and transition forgery; traversal limits; retained R evidence; historical eligible responses. The initial `CheckpointEvidence` adapter is still a test fixture, so these are component simulations, not a complete RAI network execution.

At this slice, responder integration, relay/fetch scheduling, transport bounds and synchronization were outstanding; the next slice below implements the first two as in-memory components. The original report (pages 10 and 27) assumes round synchronization in its progress argument without giving a concrete timing protocol there. A verified consensus rank is not a synchronized communication round. The synchronizer must establish the request-before-response overlap from arbitrary prefixes and final return of temporary suspensions; a timer alone would not discharge that obligation. No synchronizer or end-to-end termination claim is added by this change.

Current validation: 33 checkpoint tests, 938 feature-on library tests and a passing feature-off library build. Results are recorded in [phase1-recursive-proposer-validation.json](phase1-recursive-proposer-validation.json). Disk persistence and Gate B remain deferred and pending, respectively. Earlier sections describe the preceding partial slices; the verifier and proposer omissions in them are superseded here.

## Mutable responders and independent-replica relay

`slow_responder.rs` connects the existing A/B state rules to authenticated, recursively verified requests. R retains the highest (rank, value); A retains conflict and its maximum; B retains true evidence and the permitted false evidence. Every response carries the exact originating request for each retained value. Invalid requests do not mutate state. Proposer-selected R evidence is merged back into the responder, keeping their local R views consistent.

`slow_replica.rs` combines proposer, responder and proof store behind typed message/effect interfaces. Replicas fetch missing requests and certificates by digest, retry pending requests when evidence arrives, relay new signed snapshots once and independently verify decision proofs. Explicit retries retransmit retained signed responses, the current request and known decision evidence. They continue serving requests and proofs after deciding. Fetch suppression spans incoming events; explicit retry reissues missing hashes after loss. This avoids a feedback loop where incoming fetch traffic itself causes more fetch broadcasts. Traversal-budget failures preserve pending work and already committed outgoing effects.

The deterministic simulator gives each of four slow replicas a separate store and exchanges actual signed requests, responses and certificates. Scenarios cover uniform inputs with certificate broadcasts withheld (forcing fetch/reply), split initial values, and one temporarily absent replica returning after the other three decide. Additional regressions cover total loss of initial outgoing messages, invalid-request state preservation, retry suppression and traversal-budget recovery. Initial payload/fast-R validation still uses the explicit application fixture. These scenarios exercise concrete executions; they do not establish arbitrary-schedule termination.

The typed messages are not wire encodings. That slice replayed the full retained response history; the bounded batching and admission checks described next supersede it. Retry timers, the finite initial-proposal rule, adversarial timing exploration, round synchronization and the node/application adapter remain. The slow replica is not yet the outer fast/slow `CheckpointElection` service. Disk persistence remains deferred. Validation: 945 feature-on library tests passed, including 40 checkpoint tests; the feature-off library build passed. Details are recorded in [phase1-replica-validation.json](phase1-replica-validation.json).

## Bounded admission and response replay

`slow_io.rs` adds configurable `SlowIoLimits`: a per-message accounting limit, a response count per retry, and a response-byte budget per retry. Defaults are 1 MiB per typed message, 64 historical responses per retry and 1 MiB of historical response data per retry. A counting serializer stops at the limit without allocating another encoded copy; incoming oversized messages are rejected before hashing, validation or proof-store insertion. This uses the local serde JSON representation for accounting, not the future transport frame format.

Retained response hashes form a rotating queue. New responses join its tail, and only emitted records move to the tail after a successful batch. Message and byte limits apply together; a failed batch leaves its cursor unchanged. A batch never emits a retained record twice. Policy changes reject invalid budgets or a cap that would strand retained responses, the current request, or a later legal two-value response shape. No vote or historical proof is evicted.

The current request, known decision evidence and newly generated effects remain outside the historical-response replay budget. These controls are not a bound on total event output, stored evidence, pending work or decoding memory. The wire decoder still needs frame/reassembly limits before allocation; relay batching does not establish round synchronization.

Regressions cover exact encoded-size boundaries, byte and message budgets, fair rotation under new arrivals, unchanged cursors after errors, oversized-message rejection without state insertion, and four-replica recovery from a lost response round with one-response retry batches. Validation: 950 feature-on library tests passed, including 45 checkpoint tests; the feature-off library build passed. Details are recorded in [phase1-io-validation.json](phase1-io-validation.json).

## Commit 6: equal-weight committees

The first Phase 1 change ports the membership and configuration portions of `79ece3fbc` and `e1f271c49`, adapted to Phase 0 and Fast Archipelago v1.2. It does not import the source branches' close or checkpoint-finalization changes.

`committee_model = "equal_weight"` with `committee_f` and `committee_p` selects the largest N = 3f + 2p + 1 positive delegated balances, breaking ties by public key. Each selected identity has weight one. Genesis and derived committees use this policy; finalized delegation changes retain the existing two-epoch lag. Setup before epoch activation remains stake weighted. The default stays `weighted`, and feature-off node behavior ignores the new policy.

Account/report thresholds are q = 2f+p+1, fast = N−p, recovery support = f+p+1 and reports = N−f. `CheckpointThresholds` separately supplies election Q, F_fast, P and W, validates N, and computes the v1.2 dynamic recovery support m−f−p only for Q ≤ m ≤ N. It does not implement recovery or elections yet.

Unlike the source port, an undersized equal-weight membership does not lower thresholds: it logs an error and produces an inert committee with no voting members and the configured thresholds. Direct equal-weight construction rejects incorrect distinct-member counts. Equal-weight committee digests bind the fault budgets and membership policy; historical weighted digests remain unchanged.

Node TOML rejects unknown model names. Nanospam exposes `--committee-model equal_weight --committee-f 1 --committee-p 1` and requires `--prs` to equal N. No checkpoint-election wire messages or services are introduced.

## Validation

The commit is checked with formatting, feature-on and feature-off `cargo check --tests`, the feature-off workspace library suite, the feature-on node/messages/types library suites, and the nanospam CLI regression. Results are recorded in [phase1-commit6-validation.json](phase1-commit6-validation.json).

New tests exercise exact certificate formation, outsider exclusion, duplicate and undersized membership, deterministic balance/identity selection, fault-budget digest binding, both simulator populations, dynamic recovery thresholds, configuration round trips, feature-off isolation and committee delegation lag. Gate B remains pending commits 7–11; no performance claim is made for this change.

## Commit 7: first votes and recovery certificates

The feature-gated `consensus/checkpoint_election` module now provides pure logic for signed first votes, fast certificates, and both v1.2 recovery-certificate forms. Signatures bind a domain tag, the full instance, rank, signer, value and typed legal-introduction reference. Rank zero requires proposal ancestry; higher ranks require a previous B or fast certificate. Equal-weight membership and the committee digest are checked when creating the verification context.

A recovery snapshot retains one validated vote per signer. Fast detection separately retains signed votes per signer and value, so an equivocator's earlier vote cannot hide a valid fast certificate for another value. Missing application payloads or proof dependencies are parked and retried; invalid signatures are rejected before parking, and dependencies found invalid are discarded.

Recovery uses the changing threshold m−f−p for Q ≤ m ≤ N. A singleton may resolve at Q. An ambiguous snapshot waits, even if one value is larger. An empty snapshot needs independently verified ordinary R evidence and its recomputed maximum; it never substitutes the largest first-voted value. A resolved certificate can be verified by another node without changing that node's immutable first votes. Fast and recovery proof records have serde round trips; these are not the network envelopes planned for commit 9.

Two explicit interfaces remain unwired. `CheckpointEvidence` must validate RAI application values, complete legal-introduction ancestry and ordinary R certificates including complementing witnesses. There is no production implementation or permissive default; commit 8 supplies the R/A/B verifier and commit 10 connects application validation. `FirstVoteJournal` requires atomic insertion by (instance, rank, signer) before a vote is released; an existing slot returns the original vote without invoking the signing closure. Tests exercise the contract with an in-memory journal, including uncertain write outcomes; disk persistence is now deferred at the user's request. `VolatileFirstVoteJournal` retains locks for the running process only.

Eleven deterministic tests cover signed-field tampering, rank/instance replay, membership, ancestry binding, immutable signing, pending evidence, equivocation, both recovery forms, certificate verification/serialization, and the report's hidden-fast example. Bounded exhaustive tests cover n=6 and n=9: every fast-certificate identity set, recovery identity subset and permitted conflicting-vote subset for hidden-fast preservation; integer partitions of snapshot support for uniqueness at P; and explicit ambiguity at P−1. These are static recovery checks, not a full R/A/B model checker or proof of termination. See [the commit 7 validation record](phase1-commit7-validation.json).

## Commit 8 in progress: volatile state and A/B components

Disk persistence is deferred. `VolatileFirstVoteJournal` keeps per-instance/rank locks in memory for the process lifetime. Pure A/B state transitions and certificate-result evaluators are implemented, along with distinct-identity W admission and Q eligibility counters. These helpers require authenticated, validated inputs from the future recursive verifier; they do not replace that verifier.

The [conformance reproducer](RAI-archipelago-v1.2-conformance.md) retains an earlier B adoption certificate that still carries a different value after a later true-valued certificate. It exposed the overbroad adoption-persistence claim, not two conflicting decisions. The 4 October PDF correction replaces that claim with conditional decision-certificate persistence; global convergence and implementation conformance remain open. Eighteen focused tests pass, including the reproducer; the tests deliberately preserve and report the discrepancy rather than claiming the lemma has been proved.

The [partial-step validation record](phase1-step8-partial-validation.json) records passing formatting, both build configurations, 923 feature-on library tests and 1,702 feature-off library tests.

Commit 8 is incomplete. The full driver, recursive verifier, synchronizer, networking and Gate B are not implemented by this change. The current review finding must be reconciled before the plan's service-wiring gate can pass.

## Slow payload codec and current scope

Total retained-storage limits are deferred at the user's request. The current nanospam baseline assumes all nodes remain online. Existing temporary-absence unit regressions are retained; they are not a required benchmark workload.

`slow_wire.rs` adds a versioned, instance-bound slow payload envelope with an explicit body length. The header can be checked before allocating a body buffer. The codec rejects oversized bodies, unsupported versions, wrong instances, truncation and trailing bytes, and checks the instance inside requests, responses and certificates. Fetch and decision hints inherit the envelope's instance. Structural decoding still requires the replica's signature and recursive proof verification.

Every delivery in the independent-replica simulator now passes through this byte codec. This is a payload format, not a Nano network message: transport message registration and node handlers remain outstanding; the next slice implements payload fragmentation/reassembly. No all-online nanospam checkpoint run is claimed yet. Validation: 951 feature-on library tests passed (738 node, 82 messages, 131 types), and the feature-off library build passed; see [the codec validation record](phase1-wire-validation.json).

## Slow payload fragmentation and reassembly

`slow_frames.rs` splits encoded payloads into at most 65,527-byte chunks including their chunk header, reserving eight bytes for a future Nano message header within the 65,535-byte frame limit. Each chunk names a content digest, total size and canonical offset. The assembler checks version, lengths, configured per-object limits and chunk boundaries before retaining body bytes. It allocates the complete payload only after every chunk is present, then verifies its digest. Decoding and recursive signature/proof checks still follow reassembly.

Partial assemblies are scoped to a caller-supplied connection identity. Duplicate chunks are idempotent, conflicting duplicates are rejected, reordered chunks are accepted, and completed payloads can be retransmitted. A completed corrupt assembly is removed so a later retry can succeed. The caller can discard partial assemblies on disconnect. Aggregate storage limits and expiration remain deferred; this is a per-object bound.

Independent-replica simulations now exchange chunked payloads in reverse chunk order before decoding. Focused regressions cover multi-frame proofs, duplicates, retransmission, peer isolation, corruption, disconnect cleanup, malformed headers and noncanonical sizes/offsets. These are transport components; no Nano message type or live checkpoint handler is installed yet. Validation: 954 feature-on library tests passed (741 node, 82 messages, 131 types); the feature-off library build and diff whitespace check passed. See [the frame validation record](phase1-frames-validation.json).

## Nano slow-frame registration

The feature-gated `CheckpointFrame` message uses type 0x1a and the full 16-bit extensions field for payload length. Its private payload can only be constructed after structural validation of the chunk version, total, offset and canonical body size. Header validation rejects packets exceeding 65,535 bytes including the Nano header before waiting for their bodies. Message names, statistics and message-type array capacity include the new type.

Replica simulations now pass each chunk through the real Nano serializer and streaming deserializer. Network-level tests cover maximum-size frames, partial reads, repeated frames, unsupported chunk versions, inconsistent extension lengths and oversized headers. The node currently drops and counts checkpoint frames because no application-backed checkpoint service is installed; this change enables transport representation, not live elections. Fast-proof envelopes, the synchronizer and application/service wiring remain outstanding. Validation: 956 feature-on library tests, 77 feature-off message tests and the feature-off node build passed; see [the registration validation record](phase1-registration-validation.json).

## Fast payload envelopes

`fast_wire.rs` adds a separate `RAIFAST` payload namespace for FIRST, FC0 and RC0. The header binds the checkpoint instance digest and body length; the tagged body distinguishes the three types. Encoding and decoding enforce attempt zero and exact instance matching for the outer proof and every nested vote. Signature, quorum, legal-introduction and recovery-candidate validation remain mandatory after decoding.

Fast payloads reuse the existing content-addressed chunk carrier and Nano message type 0x1a. The existing `slow_frames` API names and chunk digest domain remain unchanged; the complete payload bytes, including their fast/slow namespace, are hashed. No separate fast Nano message identifier is needed. End-to-end component tests serialize all three kinds through Nano frames, reassemble, decode and independently verify them. Negative tests cover truncation, extra bytes, size limits, instance replay, cross-namespace decoding, nested-instance mismatch, nonzero attempts and unauthenticated signatures.

Validation: 958 feature-on library tests and the feature-off node build passed; see [the fast-envelope validation record](phase1-fast-wire-validation.json).

The live node still drops and counts incoming checkpoint frames pending service installation. Routing to fast versus slow instances, application evidence retrieval, synchronization and decision installation remain; no all-online nanospam checkpoint run has been performed.

## Per-instance transport adapter

`CheckpointTransport` derives both fast and slow codec contexts from the active checkpoint context, converts typed messages to Nano checkpoint frames, and routes reassembled messages by their explicit namespace. It rejects unknown namespaces and foreign instances, uses checked size arithmetic and retains incomplete objects per connection. Callers clear a disconnected connection explicitly and create a new adapter when replacing the checkpoint instance.

The independent-replica simulator now owns a persistent adapter for each replica. Its message deliveries pass through Nano stream decoding and the adapter before reaching the recursive slow verifier. New tests cover fast/slow routing, retransmission, foreign instances, unknown namespaces, oversized configuration, interleaved large and small payloads, connection isolation and disconnect cleanup. Structurally valid but invalid-quorum certificates can be transported and remain subject to consensus verification.

Validation: 960 feature-on library tests and the feature-off node build passed; see [the transport validation record](phase1-transport-validation.json).

This adapter does not install the live node handler or manage peer lifetimes itself. Application evidence, the combined fast/slow election service, scheduling/synchronization and decision installation remain outstanding. Total storage limits remain deferred and the nanospam baseline remains all-online.

## Selected-member composition driver

`CheckpointParticipant` joins the initial fast pool and ordinary slow replica for a selected slow-committee member. Validated FIRST votes can form a fast certificate or resolve initial recovery. A verified imported or local RC0 starts one slow proposal without waiting for a timeout; later recovery preferences do not restart that proposal. The fast listener remains active after fallback begins. Fast and slow results share an immutable decision value, with one outer `Decided` notification; path-specific slow effects still carry their proof information. Contradictory verified outcomes are rejected.

Explicit retries recheck parked FIRST votes, retransmit known FC0/RC0 and keep the slow replica serving proofs after decision. Empty-candidate recovery accepts an explicit initial-R evidence reference through the existing application verifier. FIRST signing remains outside the driver behind the process-lifetime journal.

This is a component for selected slow members, not the complete node service. The constructor rejects other members. The caller must retain slow messages arriving before RC admission, preserve connection identity for replies, and schedule retries. Missing imported fast-certificate/recovery dependencies must also be retried by the caller. Observer decision verification, application adapters, synchronizer and live node installation remain outstanding; aggregate storage remains deferred and nanospam remains all-online.

Regressions cover RC-driven fallback before a late FC, one-time outer decision notification, continued slow service after fast decision, a four-participant slow decision, and post-decision proof retransmission. Validation: 962 feature-on library tests and the feature-off node build passed; see [the participant validation record](phase1-participant-validation.json).

## Keyless slow-decision observer

`CheckpointObserver` supplies the slow-decision verification role needed by fast members outside the selected slow committee. It owns no signing key, proposer or responder. Decision hints remain pending until a fresh recursive verification session validates the complete certificate ancestry and obtains a B commit. A hint, a decoded certificate or an adoption result cannot directly install a decision.

Missing objects trigger deduplicated fetches; explicit retry reopens lost fetches. Verification-budget failures retain pending work and the caller can raise the budget. Authenticated requests and instance-checked certificate objects are cached without treating insertion as proof validation. After deciding, the observer serves retained requests/certificates and retransmits the decision hint. It emits its decision notification once and rejects conflicting verified outcomes. Incoming typed messages retain the existing per-message byte cap; aggregate storage remains deferred.

A regression starts four slow replicas, then gives a fresh observer only their decision hint. It drops the initial fetch, retries, obtains the entire ancestry through replica replies and checks a matching decision without any observer vote. It also checks invalid-quorum rejection, duplicate hints and proof service after decision.

Validation: 963 feature-on library tests and the feature-off node build passed. The observer regression also passed after adding the invalid-quorum assertion; see [the observer validation record](phase1-observer-validation.json).

This is a standalone observer component. Role selection, fast-certificate decisions for observers, early-message buffering, application evidence and live service wiring still need integration; it does not add a synchronizer or a nanospam completion claim.

## Committee-wide driver and early slow traffic

`CheckpointDriver` selects a participant for the deterministic slow committee and a keyless slow observer plus fast pool for other fast-committee members. Nonmembers cannot construct the driver. Both roles can accept fast certificates; observer FIRST reception can also assemble one. The driver emits one decision notification across both paths and translates protocol effects into broadcasts or replies with explicit connection destinations.

Selected members retain slow traffic received before RC admission. Signed requests/responses are authenticated before retention, certificate instances and per-message sizes are checked, and duplicate messages from the same connection are coalesced. Admission drains the queue in order, preserving each original sender for replies; malformed queued proofs yield rejection effects without discarding other queued traffic. Disconnect cleanup discards queued traffic for that connection. Aggregate queue/storage bounds remain deferred.

A six-member test exercises four slow participants and two observers, duplicate early-fetch buffering, the eventual reply to the original sender, a slow decision at all six members, and deduplication of a later matching fast decision. This is an all-online component execution, not a live nanospam run or a proof of arbitrary-schedule termination.

Validation: 964 feature-on library tests and the feature-off node build passed; see [the driver validation record](phase1-driver-validation.json).

The driver still needs application evidence retrieval, retry scheduling/synchronization, journal-backed local FIRST release and live node installation. Imported fast proofs with unavailable application dependencies remain caller-retryable; initial-R evidence injection for empty recovery must also be exposed through the service. Transport and driver are separate components awaiting node integration.

## Application admission and local FIRST release

The driver now releases local FIRST through the caller's process-lifetime journal, reuses the original vote when candidate preferences change, and retransmits that vote on retry. It also exposes explicit initial-R evidence injection for empty recovery.

`CheckpointCandidate` signs the complete instance, proposer and epoch-value hash. Validation checks exactly N−f report references, committee membership/weights, roots, predecessor and recomputed BuildState. `EpochDecisionService::validate_candidate` connects this to the node's reconstructed report exchange and configured session. Candidate derivation and its report source now reject reports signed against a different committee digest; previously those paths checked predecessor and roots without this committee check.

`CandidateEndorsementJournal` locks one candidate per (instance, proposer, endorser). `CandidateAdmission` requires Q distinct authenticated endorsements. Two certificates for one proposer intersect in at least f+1 identities, so a correct lock holder would need to sign both: at most one candidate per proposer can be admitted, bounding the initial universe by N. The evidence cache accepts only application-validated candidates with a verified admission. Admission adds an exchange before FIRST; future latency measurements must include that cost. Admission locks, like FIRST locks, remain volatile.

`InitialRRequest`, `InitialRResponder` and `InitialRCertificate` implement an independent initial max-register exchange. Responses bind the request and an admitted candidate; a responder retains its maximum across requests. The verifier requires Q distinct signatures, validates admitted origins and recomputes the maximum. Candidate admission certificates witness origin availability here, rather than reusing later RC-backed slow-response certificates. Only verified initial-R certificates populate `CandidateEvidence::r_maximum`; a caller-supplied maximum or FIRST snapshot is not accepted. The PDF now specifies this initial-R refinement and finite admission rule.

Validation covers real BuildState reconstruction, signed-field tampering, missing reports, wrong committee context, immutable local FIRST release, endorsement locks, quorum duplication, initial-R signatures and monotonic maxima. The complete feature-on library run passed 967 tests (752 node, 84 messages, 131 types); all 683 feature-off node library tests passed. A focused initial-R test was extended afterward to check retention of a higher value across a lower request. That focused test passed; see [the gate-prerequisite validation record](phase1-gate-prerequisites-validation.json).

## Gate B status: blocked before live service installation

Phase 1 is not complete and Gate B has not been run. The all-online restriction removes the offline benchmark workload; it does not supply synchronized request-before-response rounds. The local source paper assumes that ordering on page 8 and invokes round synchronization in its termination argument on pages 26–27. The current replica processes requests immediately and its retry method only retransmits evidence. No implemented synchronizer establishes the source's ordering and progress premises after an arbitrary prefix. This is a missing implementation/proof obligation, not a new nontermination counterexample for v1.3.

The production handler therefore still drops checkpoint frames and the optional `CheckpointElection` interface remains uninstalled. Connecting admission/candidate/initial-R dissemination, bounded verification retry policy, candidate timing, node lifecycle/decision installation and benchmark instrumentation also remains. The existing `tools/rai/run_matrix.sh` is explicitly a Phase 0 single-epoch runner; its result would not establish Gate B. The required all-online gate must exercise three real checkpoint decisions and record fast frequency, recovery wait/size, slow ranks, close duration and the Gate A goodput/latency comparison.

The next critical step is a concrete synchronizer governing request collection and response generation, with a checked arbitrary-prefix progress argument. Enabling the service or reporting a fast-only benchmark as Gate B before that step would bypass the plan's logic exit criteria. Aggregate storage and disk persistence remain deferred.

## Application evidence distribution

`ApplicationWireCodec` introduces a separate `RAIAPPL` payload namespace on the existing checkpoint frames. It carries candidate proposals, endorsements, candidate/admission bundles, initial-R requests/responses/certificates and digest fetches. The codec enforces instance binding, including nested endorsements/responses, and bounded exact payload lengths. Structural decoding remains separate from signature and application validation.

`ApplicationExchange` owns a per-instance evidence cache and pending messages. The caller supplies the report/BuildState validator and a process-lifetime endorsement journal. Endorsements arriving before candidates trigger fetches. Once Q valid distinct endorsements are available, the exchange independently validates and admits the candidate, then relays its complete bundle. Missing report reconstruction remains pending and is retried through the caller's validator. Initial-R traffic uses the same dependency mechanism; distinct valid responses form certificates that are independently checked before cache insertion and relay.

Fetch replies are immediate, preserving their caller's reply destination; pending work cannot produce a reply to a stale peer. Explicit retries reopen missing fetches and retransmit retained local candidates/endorsements/initial requests, admitted bundles and known initial-R certificates. Value-hash fetches prefer admitted candidates. Aggregate retention remains unbounded by the user's deferred storage policy.

The expanded application regression exchanges actual payloads through Nano serialization, chunk reassembly and namespace routing between six independent caches. It covers out-of-order endorsements, admission at all six members, a lost initial-R transmission recovered by retry and independent initial-R proof availability. A separate regression checks truncated/oversized envelopes, namespace separation and nested foreign-instance rejection.

Validation: 968 feature-on library tests and all 683 feature-off node tests passed; see [the application-exchange validation record](phase1-application-exchange-validation.json).

This is an evidence-distribution component, not the live node handler. Joining exchange notifications to driver retries and local FIRST release, report reconstruction wakeups, initial-R selection, node lifecycle and the synchronizer remains. Gate B remains blocked and no live nanospam gate was run.

## Application-to-election session integration

`CheckpointSession` now joins the application exchange and committee driver for one instance and signing identity. It authenticates and validates one locally proposed candidate, waits for quorum admission, releases FIRST through the process journal, and starts the independent initial-R exchange. The session retains the greatest verified initial-R result it has learned and supplies its certificate to empty recovery. The process-lifetime journal bundle also retains the original signed candidate across session recreation, so a new preference cannot silently replace it.

FIRST votes and imported fast/recovery certificates with missing application evidence remain pending with their original connection. Digest fetches query the application cache; slow proof fetches are mirrored there because their missing references may name admitted candidates or initial-R evidence. New evidence wakes pending verification automatically. An authenticated initial slow request can supply its attached RC0 for admission before its slow request is delivered. Replies preserve their original destinations, and disconnect cleanup clears connection-specific pending traffic.

Evidence arrival now uses `evidence_updated` on the replica, participant, observer and driver instead of a timer retry. This rechecks pending work without retransmitting historical response batches or reopening unrelated fetches. Explicit retries still perform retransmission. Observer-side recovery certificate generation is deduplicated between retries to prevent certificate feedback when initial-R evidence arrives repeatedly.

The integrated test runs six independent sessions with actual application validation, journals and evidence stores, serializing every hop through Nano frames. It discards all initial outputs and recovers by explicit retry. Identical inputs produce a fast certificate; split inputs decide through the slow path without a fast certificate. All six members notify once. Additional coverage parks a fast decision proof before its application evidence and verifies automatic completion after a fetched admission arrives, and checks candidate-lock preservation across session recreation.

Validation: 968 feature-on library tests and all 683 feature-off node tests passed; see [the session validation record](phase1-session-validation.json).

This is an integrated in-memory protocol session, not a live node service or synchronizer. Candidate timing, report reconstruction wakeups, transport/node lifecycle, proof-backed checkpoint installation, synchronization/progress and the nanospam gate remain. The all-online benchmark assumption and deferred disk/aggregate storage scope are unchanged.

## Portable decision-proof handoff

`CheckpointDecisionProof` encodes a fast certificate or a slow commit proof under a versioned decision-proof envelope. The slow proof collector follows the commit's triggering request, response origins and preceding-certificate references, retaining each required request/certificate once. It intentionally does not recurse into the values carried by complementing witness responses: those signatures witness receipt, as in the recursive verifier. Unrelated retained store objects are excluded.

Import builds a fresh proof store and reruns the existing strong commit verification, including weak ancestry admission where appropriate. It rejects duplicate or unrelated attachments, missing dependencies, noncommit roots, invalid signatures/instances and exceeded traversal budgets. The envelope also enforces an explicit serialized byte budget. Application candidate/admission/initial-R evidence remains external and must already have passed the receiver's application validator; the consensus bundle does not silently mark those objects valid.

Participants, observers and the committee driver now expose retained decision proofs. `CheckpointSession::checkpoint_decision` independently verifies the proof before exporting the decided EpochValue and encoded proof through the existing `CheckpointDecision` type. `verify_decision` checks an imported proof independently and binds its result to the exact epoch-value hash. This supplies the handoff needed by the node election interface without installing a live service.

The six-session integration regression now exports fast and slow decisions and verifies each across every session. It also rejects altered values, unsupported proof versions, size-limit violations, incomplete/duplicate slow ancestry, noncommit roots and insufficient verification budgets.

Validation (2026-10-05): 968 feature-on library tests passed (753 node, 84 messages, 131 types), together with 683 feature-off node tests. Results are recorded in `doc/phase1-decision-proof-validation.json`.

Live node installation, candidate timing, reconstruction wakeups, round synchronization/progress and Gate B execution remain outstanding. No checkpoint is installed by this slice.

## Candidate submission timing

The node's `EpochDecisionService` now waits for a usable report quorum and either all committee members' usable reports or 500 ms after first observing that quorum. Reports must bind the expected committee and predecessor. The deadline resets on a context change or quorum loss, and decided epochs discard their timing state. Repeated ticks do not extend the deadline. This delay governs candidate submission only; it does not gate FIRST votes or implement the slow synchronizer. The 500 ms value remains provisional pending Gate B measurement.

Controlled-clock regressions cover the quorum boundary, 499/500 ms boundary, immediate all-report release, context changes and quorum loss. All 8 feature-on report-module tests passed (2026-10-05); see `doc/phase1-candidate-timing-validation.json`. Live session installation, reconstruction wakeups, synchronization/progress and Gate B execution remain outstanding.

## Report reconstruction wakeups

`ApplicationExchange::evidence_updated` rechecks parked candidate/admission validation after local report reconstruction. `CheckpointSession::evidence_updated` propagates new admissions into pending FIRST/certificate processing and local proposal progress. These entry points preserve fetch suppression and do not replay retained message history; timer retransmission remains a separate operation.

`CheckpointElection` now requires an evidence-update hook. `EpochDecisionService` calls it after the report ticker refreshes usability, before candidate submission or decision draining, including ticks without an eligible local candidate. The concrete live adapter still needs to connect this hook to its sessions.

The session regression receives a fast certificate and its admission while report validation is unavailable. It remains undecided until a reconstruction-only wakeup succeeds, emits one decision, and produces no output on a repeated wakeup. Validation: all 755 feature-on node library tests passed (2026-10-05); see `doc/phase1-reconstruction-wakeup-validation.json`. Live session/transport installation, synchronizer/progress and Gate B remain pending.

## Concrete election-interface adapter

`CheckpointElectionAdapter` implements the node's `CheckpointElection` interface for one configured checkpoint and signing identity. It owns the protocol session, transport reassembly, process-lifetime journals and report-validation callback. It accepts frames, exposes encoded network effects, handles reconstruction wakeups and explicit retransmission, and exports an independently verified decision once. Repeated candidate ticks do not rebroadcast or replace the original proposal. Disconnect cleanup removes pending replies and connection reassembly; replacement returns the signing journals for reuse.

The adapter regression connects six independent adapters through checkpoint frames, verifies each exported decision, checks repeated candidate/decision polling and wakeups, and recreates an adapter with its retained journal to check that a different candidate cannot replace the original proposal.

Validation: all 755 feature-on node library tests passed (2026-10-05); see `doc/phase1-election-adapter-validation.json`.

Node-wide instance routing, wallet identity/session configuration, shared network ownership, retry scheduling and installation into `EpochDecisionService` remain outstanding. This component is not yet enabled by `node.rs`; the network handler still drops checkpoint frames. The synchronizer/progress obligation and all-online Gate B measurements remain open.

## Minimum all-online benchmark wiring

At the user's request, the synchronizer/progress proof is deferred for the minimum live benchmark. Set `RAI_CHECKPOINT_BENCHMARK=1` in a `rai_protocol` node process to install the experimental checkpoint service. The binding uses the network genesis hash as its session and derives each epoch's committee/predecessor locally. The service selects one local committee signing identity, validates candidates through the node's reconstructed reports, routes frames by registered fast/slow instance, retries once per second, and exports verified decisions to the existing checkpoint installer. It retains prior instances for proof service and clears connection state on disconnect. This mode targets fresh all-online nanospam nodes with one representative key each; it is not a restart or changing-identity service.

`tools/rai/run_checkpoint_smoke.py OUTPUT` runs six equal-weight nodes (f=1,p=1), requires three matching checkpoint decisions across all six and at least 18 installation diagnostics, and saves the command, binary hashes, logs, proofs, final states and counters. Default workload: 360 blocks at 10/s, 5-second epochs. Build with `cargo build --release -p rsnano_cli -p nanospam --features rsnano_cli/rai_protocol --bin rsnano --bin nanospam` first. The runner rejects occupied RPC ports, preserves run data, and stops only its own process group.

Validation (2026-10-05): 755 feature-on node tests and 683 feature-off node tests passed; the release build passed. The live six-node smoke run passed with matching installed checkpoints for epochs 0–5 on all six nodes, and five deliveries of epoch 6 at capture. There were 41 installed decisions: 36 fast proofs and 5 slow proofs; epoch 4 demonstrates cross-path agreement (one fast, five slow). Observed account finalization p50 was 98–100 ms at the offered 10 blocks/s; this is not a saturation comparison. One late epoch-4 InvalidEvidence rejection and four uncemented blocks on one node at capture remain in the evidence for follow-up. Results and logs: `doc/benchmarks/phase1-checkpoint-smoke-2026-10-05/`; validation summary: `doc/phase1-benchmark-wiring-validation.json`.

This smoke benchmark is not Gate B performance qualification or a general termination proof. Disk persistence and aggregate storage limits remain deferred.

## Next

Investigate the retained late rejection, add full-drain acceptance and the remaining performance metrics/comparison. The synchronizer/progress proof is explicitly deferred rather than a prerequisite for this experimental run.
