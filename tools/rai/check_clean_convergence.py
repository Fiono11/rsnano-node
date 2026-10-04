#!/usr/bin/env python3
"""Bounded arithmetic/certificate checks for convergence to either value.
Not a synchronizer implementation or a full protocol termination proof.
"""
from itertools import combinations, product
import json
from check_batched_updates import candidates, b_result


def check(f,p):
    n=3*f+2*p+1;q=n-f-p;c=n-f;fast=n-p
    correct=set(range(c));universe=set(range(n))
    snapshots=[ids for m in range(q,n+1) for ids in combinations(range(n),m)]
    recovery_checks=0; b_checks=0
    for group_tuple in combinations(correct,q):
        group=set(group_tuple);outside=sorted(universe-group)
        for assignment in product((0,1,2),repeat=len(outside)):
            votes={i:0 for i in group};votes.update(zip(outside,assignment))
            for ids in snapshots:
                sample={i:votes[i] for i in ids}
                possible=candidates(sample,f,p)
                assert 0 in possible
                if len(possible)==1: assert possible=={0}
                if len(ids)>=c: assert possible=={0}
                recovery_checks+=1
        # Every current B certificate contains a responder from the correct
        # true-bearing Q set, even if outsiders report only larger false values.
        states={i:({(True,0),(False,1)} if i in group else {(False,1)}) for i in universe}
        for ids in combinations(range(n),q):
            assert group & set(ids)
            assert b_result([states[i] for i in ids])==('adopt',0)
            b_checks+=1
        # A competing fast certificate must include an immutable correct
        # first voter for value 0, so cannot certify a different value.
        assert q+fast-n>0
    # Once every legal A input is 0, source-valid B input can only be true,0;
    # Q awake correct responses suffice to commit (without fast cooperation).
    assert b_result([{(True,0)} for _ in range(q)])==('commit',0)
    return {'n':n,'f':f,'p':p,'Q':q,'recovery_snapshots_checked':recovery_checks,
            'fresh_B_certificates_checked':b_checks,
            'Q_correct_matching_first_votes_force_resolved_value':True,
            'fresh_B_true_dominance':True,'slow_commit_from_singleton_inputs':True}

if __name__=='__main__':
    print(json.dumps([check(1,1),check(2,1),check(0,1)],indent=2))
