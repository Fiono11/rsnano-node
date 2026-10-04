#!/usr/bin/env python3
"""Bounded interface checks for one initial fast attempt plus certified fallback.
This checks wrapper safety/recovery arithmetic, not the slow engine's proof.
"""
from itertools import combinations,product
from collections import Counter
import json
from check_batched_updates import candidates,a_result,b_result


def threshold_shortcuts(f,p=1):
    n=3*f+2*p+1;q=n-f-p;P=n-f;F=n-p
    correct=tuple(range(P));faulty=tuple(range(P,n))
    low=correct[:f+p+1];high=correct[f+p+1:]
    votes={i:0 if i in low else 1 for i in correct}
    assert candidates(votes,f,p)=={0}
    private={i:1 for i in high+faulty}
    private.update({i:0 for i in low[:p+1]})
    assert len(private)==P and candidates(private,f,p)=={1}
    # Full n-record collection can be prevented by f Byzantine withholders.
    assert P<n
    return {'n':n,'f':f,'p':p,'P':P,
            'public_P_snapshot':votes,'private_P_snapshot':private,
            'both_P_recoveries_valid':True,'requiring_all_n_can_wait_forever':True}


def hidden_fast_interface(f,p=1):
    n=3*f+2*p+1;q=n-f-p;F=n-p;faulty=set(range(f));count=0
    # For every possible hidden fast signer set, correct FC signers' votes
    # remain v. Other identities may report v,w,u, including equivocations by
    # Byzantine FC signers. Every valid resolved RC must still choose v.
    for fc_tuple in combinations(range(n),F):
        fc=set(fc_tuple);fixed=fc-faulty;free=sorted(set(range(n))-fixed)
        for assignment in product((0,1,2),repeat=len(free)):
            votes={i:0 for i in fixed};votes.update(zip(free,assignment))
            for m in range(q,n+1):
                for ids in combinations(range(n),m):
                    cand=candidates({i:votes[i] for i in ids},f,p)
                    assert 0 in cand
                    if len(cand)==1: assert cand=={0}
                    count+=1
    return count


def one_shot_trace(f,p=1):
    n=3*f+2*p+1;q=n-f-p;P=n-f
    correct=tuple(range(P));faulty=tuple(range(P,n));low=correct[:f+p+1];high=correct[f+p+1:]
    votes={i:0 if i in low else 1 for i in correct}
    assert candidates(votes,f,p)=={0}
    # Both 0 and 1 are allowed *initial* slow proposals, justified by RCs.
    # Reuse the previous counterexample's ordinary R return of 1. Now A must
    # take that certified R value, not run recovery again and revert to 0.
    r_return=1
    correct_a={i:r_return for i in correct}
    # Even if a Byzantine valid lower slow-R certificate authorizes A(0),
    # all clean correct A states include correct A(1). It cannot create a
    # singleton-0 A certificate from those correct snapshots.
    checks=0
    for mixed in product((False,True),repeat=P):
        states={i:({0,1} if mixed[i] else {1}) for i in correct}
        states.update({i:{0} for i in faulty})
        for ids in combinations(range(n),q):
            assert a_result([states[i] for i in ids])[1]==1
            checks+=1
    assert b_result([{(False,1),(True,1)}]*q)==('adopt',1)
    assert b_result([{(True,1)}]*q)==('commit',1)
    return {'n':n,'correct_slow_R_result':1,'correct_A_inputs':correct_a,
            'A_certificates_checked':checks,'lower_true_certificate_in_same_attack_impossible':True,
            'scope':'the existing attack is blocked; not an exhaustive slow-engine liveness proof'}


def unsafe_max_example():
    # n=6: all 5 correct first-vote v, forming FC(v). A later legal R(w) can
    # raise the ordinary R maximum, but cannot authorize overriding RC(v).
    f=p=1;votes={i:0 for i in range(5)}
    assert candidates(votes,f,p)=={0}
    assert len(votes)==5
    r_max=1
    illegal_a=a_result([{r_max}]*4)
    assert b_result([{illegal_a}]*4)==('commit',1)
    return {'hidden_fast_decision':0,'later_R_maximum':1,
            'naively_ignoring_recovery_would_slow_commit':1}

if __name__=='__main__':
    print(json.dumps({'wait_P_does_not_fix':[threshold_shortcuts(1),threshold_shortcuts(2)],
                     'unsafe_max':unsafe_max_example(),
                     'hidden_fast_interface_checks':{f:hidden_fast_interface(f) for f in (1,2)},
                     'one_shot_blocks_existing_attack':[one_shot_trace(1),one_shot_trace(2)]},indent=2))
