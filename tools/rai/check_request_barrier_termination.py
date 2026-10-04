#!/usr/bin/env python3
"""Finite deadlock witness for the proposed immutable phase-response rule.

Each sole signed response binds the origin requests processed before signing.
Only these responses count as Q processing witnesses; later receipt cannot
issue another response or extend the signed origin set. This is NOT a model
of Algorithm 5's original mutable per-request response service.

All initial A requests have explicit singleton-recovery witnesses. Signatures
and application validity are abstracted as authenticated rank-zero records.
No Byzantine identity forges a correct record or contributes to the trace.
"""
from collections import Counter
from dataclasses import dataclass
import json

@dataclass(frozen=True)
class Snapshot:
    signer: int
    origins: frozenset
    values: frozenset


def run(f):
    p = 1
    n = 3*f + 2*p + 1
    q = n-f-p
    correct = tuple(range(1, n-f+1))
    byzantine = tuple(range(n-f+1, n+1))
    # f+1 votes for v, f+1 for w, one for u; all rank-zero proposals legal.
    votes = dict(zip(correct, ['v']*(f+1) + ['w']*(f+1) + ['u']))
    recovery = {}
    for value, excluded_value in [('v','w'), ('w','v')]:
        excluded = next(i for i in correct if votes[i] == excluded_value)
        snapshot = {i:votes[i] for i in correct if i != excluded}
        threshold = len(snapshot)-f-p
        candidates = {v for v,c in Counter(snapshot.values()).items() if c >= threshold}
        assert len(snapshot) == q and candidates == {value}
        recovery[value] = snapshot
    assert max(Counter(votes.values()).values()) < n-p

    # Q-1 correct requesters use the valid v recovery; the other two use w.
    common = frozenset(correct[:-2])
    w1, w2 = correct[-2:]
    requests = {i: ('v' if i in common else 'w') for i in correct}
    responses = {}
    received = {i:set() for i in correct}
    # Split which w origin is processed before freezing. Include own request.
    group1 = set(correct[:f+1]) | {w1}
    for i in correct:
        origins = common | {w1 if i in group1 else w2}
        assert i in origins and len(origins) == q
        received[i].update(origins)
        responses[i] = Snapshot(i, origins, frozenset(requests[j] for j in origins))
        assert responses[i].values == {'v','w'}
    witness_sets = {j:{i for i,s in responses.items() if j in s.origins} for j in correct}
    eligible_origins = {j for j,ids in witness_sets.items() if len(ids) >= q}
    assert eligible_origins == common
    # Even allowing a snapshot to use ANY eligible origin for a value (more
    # permissive than fixed provenance), no eligible origin supplies maximum w.
    eligible_values = {requests[j] for j in eligible_origins}
    eligible_responses = [i for i,s in responses.items() if s.values <= eligible_values]
    assert eligible_responses == []

    # Deliver every remaining request and relay every signed response to everyone.
    # No further phase snapshots or first votes may be emitted; Byzantine nodes
    # remain silent. Additional deliveries/relays cannot change any witness set.
    frozen = tuple(responses.items())
    for i in correct:
        received[i].update(correct)
        assert received[i] == set(correct)
    assert tuple(responses.items()) == frozen
    assert all(len(witness_sets[j]) < q for j in (w1,w2))
    # Everyone is in A, so no correct B request/proof or next-rank predecessor
    # can be produced. There is no FC. Fair retransmission repeats fixed records.
    return {
        'n':n,'f':f,'p':p,'Q':q,'correct':correct,'silent_byzantine':byzantine,
        'first_votes':votes,'singleton_recoveries':recovery,'A_requests':requests,
        'A_responses':{i:{'origins':sorted(s.origins),'values':sorted(s.values)} for i,s in responses.items()},
        'origin_witness_counts':{j:len(ids) for j,ids in witness_sets.items()},
        'eligible_A_responses':eligible_responses,
        'all_correct_requests_eventually_delivered':True,
        'result':'A blocked under immutable snapshot-bound witnesses; no reset/skip rule',
    }

if __name__ == '__main__':
    print(json.dumps([run(1),run(2)], indent=2))
