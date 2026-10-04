#!/usr/bin/env python3
"""Abstract certificate-level checks for proposed batched updates.

One-shot variant: each phase state freezes after first Q distinct requests;
late replies certify receipt, not incorporation. All correct replicas reply
and all origins reach Q receipt witnesses. No signatures are forged (identity
sets abstract signature verification). Per-request partial ancestry is modeled
by the validated input maps; this is not the production recursive verifier.

Constructs a repeatable nondeciding rank for p=1, f=1 and f=2. The trace is
bounded-delay but NOT a clean request-before-state-update execution. This
separation is essential: waiting for Q requests alone does not ensure clean
round semantics. A second variant batches all correct requests before updating.
"""
from collections import Counter
from itertools import combinations
import json

V, W = 1, 2

def candidates(votes, f, p):
    threshold = len(votes) - f - p
    return {v for v,c in Counter(votes.values()).items() if c >= threshold}

def a_result(responses):
    values = set().union(*responses)
    return (len(values) == 1, max(values))

def b_result(responses):
    true_values = {v for s in responses for flag,v in s if flag}
    assert len(true_values) <= 1
    if true_values:
        v = next(iter(true_values))
        return ('commit' if all(s == {(True,v)} for s in responses) else 'adopt',v)
    return ('adopt',max(v for s in responses for flag,v in s))

def run(f, ranks=3):
    p=1; n=3*f+2*p+1; q=n-f-p; fast=n-p
    correct=tuple(range(1,n-f+1)); faulty=tuple(range(n-f+1,n+1))
    low=correct[:-2]; high=correct[-2:]; b=faulty[0]
    low_quorum=low+(b,)
    mixed_quorum=low[:q-2]+high
    assert len(low_quorum)==len(mixed_quorum)==q
    assert len(set(low_quorum))==len(set(mixed_quorum))==q
    carry={i:(V if i in low else W) for i in correct}
    initial=carry.copy(); trace=[]
    for rank in range(ranks):
        # Every introduction after rank zero is the requester's actual previous
        # valid B result, not an invented value or a reused certificate rank.
        assert rank == 0 or carry == trace[-1]['next_carry']
        r_requests={**carry,**{i:V for i in faulty}}
        first_batches={}
        for i in correct:
            ids=low_quorum if i in low else mixed_quorum
            assert i in ids and len(set(ids))==q
            first_batches[i]=ids
        r_state={i:max(r_requests[j] for j in first_batches[i]) for i in correct}
        r_state.update({i:V for i in faulty})
        assert all(r_state[i]==carry[i] for i in correct)
        # All correct origin receipt witness sets now contain every correct ID,
        # including later replies whose frozen state omits a received value.
        receipt_witnesses={origin:set(correct) for origin in r_requests}
        assert all(len(ids)>=q for ids in receipt_witnesses.values())
        r_returns={i:max(r_state[j] for j in (low_quorum if i in low else mixed_quorum)) for i in correct}
        assert r_returns == carry
        first_votes={i:r_state[i] for i in correct}
        # Byzantine first-vote equivocation is permitted; correct votes fixed.
        rc_v={i:first_votes[i] for i in low+(high[0],)}
        rc_w={i:first_votes[i] for i in low[:q-len(high)-f]+high}
        rc_w.update({i:W for i in faulty})
        assert len(rc_v)==len(rc_w)==q
        assert candidates(rc_v,f,p)=={V} and candidates(rc_w,f,p)=={W}
        assert all(Counter(first_votes.values())[v]+f < fast for v in (V,W))
        a_requests={**{i:next(iter(candidates(rc_v if i in low else rc_w,f,p))) for i in correct},**{i:V for i in faulty}}
        a_state={i:{a_requests[j] for j in first_batches[i]} for i in correct}
        a_state.update({i:{V} for i in faulty})
        assert a_result([a_state[i] for i in low_quorum])==(True,V)
        assert a_result([a_state[i] for i in mixed_quorum])==(False,W)
        # Low group deliberately receives the mixed response quorum first;
        # high group receives the singleton quorum first. All requests are valid.
        b_requests={i:a_result([a_state[j] for j in (mixed_quorum if i in low else low_quorum)]) for i in correct}
        b_requests.update({i:(False,W) for i in faulty})
        b_state={i:{b_requests[j] for j in first_batches[i]} for i in correct}
        b_state.update({i:{(False,W)} for i in faulty})
        # Receipt replies for every A/B origin are supplied by all correct IDs.
        for requests in (a_requests,b_requests):
            assert all(len(set(correct))>=q for _ in requests)
        # No possible Q certificate from these states commits. Byzantine nodes
        # in this execution choose these responses; they cannot be forced to help.
        assert all(b_result([b_state[j] for j in ids])[0]=='adopt' for ids in combinations(range(1,n+1),q))
        outcomes={i:b_result([b_state[j] for j in (mixed_quorum if i in low else low_quorum)]) for i in correct}
        assert all(tag=='adopt' for tag,_ in outcomes.values())
        carry={i:v for i,(_,v) in outcomes.items()}
        assert carry==initial
        trace.append({'rank':rank,'r_first_batches':first_batches,'r_first_votes':first_votes,
                      'recovery_v':rc_v,'recovery_w':rc_w,'A_requests':a_requests,
                      'A_state':{i:sorted(s) for i,s in a_state.items()},'B_requests':b_requests,
                      'B_state':{i:sorted(s) for i,s in b_state.items()},'next_carry':carry})
    # If every correct request is incorporated before the snapshot instead,
    # all correct A snapshots contain v and w. Every Q certificate includes a
    # correct response and thus returns false,w. Even Byzantine true,v cannot
    # be justified by Q singleton A responses from these snapshots.
    clean_a={V,W}
    assert a_result([clean_a]*q)==(False,W)
    assert b_result([{(False,W)}]*q)==('adopt',W)
    # With only w legal in the next rank of this particular fresh trace,
    # all correct first votes can provide Q singleton recovery even if f>p
    # prevents a fast certificate without Byzantine cooperation.
    assert candidates({i:W for i in correct},f,p)=={W}
    assert b_result([{(True,W)}]*q)==('commit',W)
    return {'n':n,'f':f,'p':p,'Q':q,'correct':correct,'byzantine':faulty,
            'semantics':'freeze after first Q requests; unlimited receipt replies',
            'repeatable_rank_cycle':True,'checked_ranks':trace,
            'clean_full_batch_same_schedule':'adopt w, then commit w (fresh ranks; not general liveness proof)'}

if __name__=='__main__':
    print(json.dumps([run(1),run(2)],indent=2))
