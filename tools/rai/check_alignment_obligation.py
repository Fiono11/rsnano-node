#!/usr/bin/env python3
"""Check the gap between common R return and common immutable first votes.
Fresh clean example with selective Byzantine R request, followed by a clean
mixed A rank that restricts all legal next-rank inputs to the same value.
Assumes authenticated two-value ancestry and correct complete validation.
Not a synchronizer or an exhaustive execution model.
"""
from itertools import combinations
from collections import Counter
import json
from check_batched_updates import candidates,a_result,b_result


def run(f):
    p=1;n=3*f+3;q=2*f+2;c=n-f;fast=n-p
    correct=tuple(range(c));byzantine=tuple(range(c,n));v,w=0,1
    # All correct R requests carry v. A valid Byzantine w request reaches
    # Q-f correct responders before their first response, others afterwards.
    high=correct[:q-f];low=correct[q-f:]
    votes={i:w if i in high else v for i in correct}
    assert len(high)<q and len(low)<q
    responses={**votes,**{i:v for i in byzantine}}
    witnessed_w=set(high)|set(byzantine)
    assert len(witnessed_w)==q
    checked=0
    for ids in combinations(range(n),q):
        assert max(responses[i] for i in ids)==w
        checked+=1
    # Byzantine identities can separately send v first votes; both recoveries
    # are valid without any correct identity changing its first vote.
    rc_w={i:votes[i] for i in high+low[:q-len(high)]}
    rc_v={i:votes[i] for i in low+(high[0],)}
    rc_v.update({i:v for i in byzantine})
    assert len(rc_w)==len(rc_v)==q
    assert candidates(rc_w,f,p)=={w}
    assert candidates(rc_v,f,p)=={v}
    assert max(Counter(votes.values()).values())+f<fast

    # Q correct fresh A responders each incorporate both correct A values
    # before their first-ever response. Outsiders may respond with a justified
    # singleton v, singleton w, or a mixed set. Every Q certificate contains
    # an H response and therefore evaluates false,w.
    h=set(correct[:q]);a_checked=0
    for outsider_state in ({v},{w},{v,w}):
        states={i:({v,w} if i in h else outsider_state) for i in range(n)}
        for ids in combinations(range(n),q):
            assert a_result([states[i] for i in ids])==(False,w)
            a_checked+=1
    # With these first-ever A snapshots, no historical A-true certificate can
    # exist either: every Q identity set intersects H. Only false,w B input
    # can be justified in this two-value example. Next-rank ancestry is w-only.
    assert b_result([{(False,w)}]*q)==('adopt',w)
    assert candidates({i:w for i in correct[:q]},f,p)=={w}
    assert b_result([{(True,w)}]*q)==('commit',w)
    return {'n':n,'f':f,'p':p,'Q':q,'correct_first_votes':votes,
            'all_R_response_quorums_return_w':checked,
            'no_Q_correct_matching_first_votes':True,
            'recovery_v':rc_v,'recovery_w':rc_w,
            'fresh_mixed_A_certificates_checked':a_checked,
            'result':'clean R agreement does not imply first-vote agreement; fresh mixed A eliminates v; next rank commits w'}

if __name__=='__main__':
    print(json.dumps([run(1),run(2)],indent=2))
