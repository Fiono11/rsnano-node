#!/usr/bin/env python3
"""Bounded checks for an explicitly hypothetical immutable phase snapshot rule.
Not a full protocol simulator, signature verifier, or liveness proof.
"""
from itertools import combinations

def check(f, p):
    n = 3 * f + 2 * p + 1
    q = n - f - p
    replicas = set(range(n))
    faulty = set(range(f))
    quorums = [set(xs) for xs in combinations(range(n), q)]
    checked = 0
    for true_certificate in quorums:
        correct_singletons = true_certificate - faulty
        # A correct singleton-v signer cannot witness processing a w request
        # in its sole phase snapshot. Byzantine witnesses may equivocate.
        possible_witnesses = replicas - correct_singletons
        assert len(possible_witnesses) <= q - 1
        for origin_witnesses in quorums:
            assert correct_singletons & origin_witnesses
            assert not origin_witnesses <= possible_witnesses
            checked += 1
    return dict(n=n, f=f, p=p, q=q, quorum_pairs_checked=checked)

def barrier_only_trace():
    # IDs 1..5 correct, 6 Byzantine. All correct requesters send once per phase.
    # A valid recovery for either value is a premise, not proved by this model.
    a_requests = {1:'v', 2:'v', 3:'v', 4:'w', 5:'w', 6:'v'}
    received = {i: {1,2,3,6} for i in (1,2,3,6)}
    received.update({4:{1,2,4,5}, 5:{1,2,4,5}})
    a = {i: {a_requests[j] for j in ids} for i,ids in received.items()}
    assert all(len(ids) == 4 for ids in received.values())
    assert all(a[i] == {'v'} for i in (1,2,3,6))
    assert set.union(*(a[i] for i in (1,2,4,5))) == {'v','w'}
    # Both w origins lack Q processing witnesses. Thus this is NOT a valid
    # certificate trace when origin eligibility is tied to these snapshots.
    for origin in (4,5):
        witnesses = {i for i,ids in received.items() if origin in ids}
        witnesses.add(6)  # allow every Byzantine identity to claim receipt
        assert len(witnesses) == 3 < 4

if __name__ == '__main__':
    barrier_only_trace()
    for budgets in ((1,1), (2,1)):
        print(check(*budgets))
    print('Barrier-only example rejected by snapshot-bound origin eligibility.')
