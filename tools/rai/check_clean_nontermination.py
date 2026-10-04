#!/usr/bin/env python3
"""Constructive symbolic clean-round nontermination witness.

All correct requests precede every phase's correct responses. Correct state
updates once per batch, late requests are processed in batch 2, and replies
are bound to processed triggering requests. Distinct authenticated identities
abstract unforgeable signatures. Q origin witnesses are individual replies.
Byzantine signatures on its private certificates are withheld from the public.
First votes are immutable. No suspensions are needed in the infinite suffix.

This is a specific inductive execution, not an exhaustive model checker or
production wire/recursive-verifier test. See the companion mathematical proof.
"""
from dataclasses import dataclass
from collections import Counter
from itertools import combinations
import json
from check_batched_updates import a_result, b_result, candidates

V,W=0,1

@dataclass(frozen=True)
class Reply:
    signer:int
    target:int
    state:frozenset
    processed:frozenset
    batch:int


def phase(requests, correct, faulty, low, q, kind):
    # Correct requests all carry v in A/R and false,w in B. First Byzantine
    # request is low; second is high (true,v in B). Each is signed by one
    # Byzantine identity, which may equivocate. Q counts distinct senders.
    low_id, high_id = -1,-2
    assert set(requests)==set(correct)|{low_id,high_id}
    all_correct=frozenset(correct)
    replies=[]
    for batch in (1,2):
        for i in correct:
            delivered=all_correct | ({low_id} if i in low else {high_id})
            if batch==2: delivered=frozenset(requests)
            assert len(all_correct)>=q and i in delivered
            assert all_correct <= delivered  # clean request-before-response
            values={requests[j] for j in delivered}
            state={max(values)} if kind=='R' else values
            for target in delivered:
                replies.append(Reply(i,target,frozenset(state),frozenset(delivered),batch))
    # Byzantine replicas can sign their own truthful low snapshots privately.
    for i in faulty:
        replies.append(Reply(i,low_id,frozenset({requests[low_id]}),frozenset({low_id}),1))
    return replies


def witness_signers(replies, origin, batch):
    return {r.signer for r in replies if r.target==origin and r.batch<=batch}


def reply(replies, signer, target, batch):
    return next(r for r in replies if r.signer==signer and r.target==target and r.batch==batch)


def verify_certificate(chosen, requests, replies, q, kind, available_batch=2):
    assert len(chosen)==q and len({r.signer for r in chosen})==q
    assert len({r.target for r in chosen})==1
    for r in chosen:
        assert r.target in r.processed
        assert r in replies
        for value in r.state:
            # A bounded-set origin may be any processed request responsible
            # for that value. Its Q complementing replies must actually exist.
            assert any(requests[o]==value and len(witness_signers(replies,o,available_batch))>=q
                       for o in r.processed)
    snapshots=[set(r.state) for r in chosen]
    if kind=='A': return a_result(snapshots)
    if kind=='B': return b_result(snapshots)
    return max(set.union(*snapshots))


def run(f,p=1,ranks=3):
    assert f>=1
    n=3*f+2*p+1; q=n-f-p; P=n-f; F=n-p
    correct=tuple(range(1,P+1)); faulty=tuple(range(P+1,n+1))
    low=correct[:q-f];high=correct[q-f:]
    assert len(low)==f+p+1 and len(high)==f+p
    assert len(low)<q and len(high)+f==q-1
    correct_carry={i:V for i in correct}
    previous_outcomes={V,W}  # authenticated rank-zero proposals
    trace=[]
    for rank in range(ranks):
        assert set(correct_carry.values())=={V}
        assert {V,W} <= previous_outcomes
        # R(v) from every correct requester; Byzantine valid R(w) disclosed
        # first to high, and later to low. Own lower request also valid.
        rr={**correct_carry,-1:V,-2:W}
        rh=phase(rr,correct,faulty,set(low),q,'R')
        first={i:next(iter(reply(rh,i,i,1).state)) for i in correct}
        assert first=={i:V if i in low else W for i in correct}
        # After relay every current R response returns w. Still no FV changes.
        for target in correct:
            for signers in combinations(correct,q):
                assert verify_certificate([reply(rh,i,target,2) for i in signers],rr,rh,q,'R')==W
        # Public recovery uses all P correct first votes, so no scheduling of a
        # smaller correct subset is needed. Byzantine FV(w) records stay private.
        assert candidates(first,f,p)=={V}
        private={i:first[i] for i in high+(low[0],)}
        private.update({i:W for i in faulty})
        assert len(private)==q and candidates(private,f,p)=={W}
        assert max(Counter(first.values()).values())+f < F

        # Every correct A input v is justified by the public recovery.
        # Byzantine A(w) is justified by private recovery, A(v) by public one.
        ar={**{i:V for i in correct},-1:V,-2:W}
        ah=phase(ar,correct,faulty,set(low),q,'A')
        assert len(witness_signers(ah,-2,1))==len(high)<q
        # Even adding all Byzantine witnesses cannot qualify this high origin
        # in batch 1. Public singleton responses number only Q-f < Q.
        assert len(high)+f<q
        a_private=[reply(ah,i,-1,1) for i in low+faulty]
        assert verify_certificate(a_private,ar,ah,q,'A')==(True,V)
        for target in correct:
            for signers in combinations(correct,q):
                assert verify_certificate([reply(ah,i,target,2) for i in signers],ar,ah,q,'A')==(False,W)
        # Every Q of publicly available first+second correct A snapshots has
        # some mixed response: fewer than Q correct identities ever signed {v}.
        assert len(low)<q

        # Every correct B request is false,w, justified by its public A result.
        # Byzantine low false,w is equally justified. Its true,v input uses
        # the privately assembled old A certificate (no forged signatures).
        br={**{i:(False,W) for i in correct},-1:(False,W),-2:(True,V)}
        bh=phase(br,correct,faulty,set(low),q,'B')
        assert len(witness_signers(bh,-2,1))+f<q
        b_private=[reply(bh,i,-1,1) for i in low+faulty]
        assert verify_certificate(b_private,br,bh,q,'B')==('adopt',W)
        for target in correct:
            for signers in combinations(correct,q):
                result=verify_certificate([reply(bh,i,target,2) for i in signers],br,bh,q,'B')
                assert result==('adopt',V)
        assert all(r.state!={ (True,V) } for r in bh if r.signer in correct)
        # No Q commit can be assembled: no correct singleton-true reply exists,
        # and Q>f. Fast listener also cannot decide, by the FV count above.
        previous_outcomes={V,W}  # actual public/private B results at this rank
        correct_carry={i:V for i in correct}
        trace.append({'rank':rank,'first_votes':first,'current_R_maximum':W,
                      'public_recovery_value':V,'private_recovery_snapshot':private,
                      'public_A_result':['false',W],'private_A_result':['true',V],
                      'public_B_result':['adopt',V],'private_B_result':['adopt',W],
                      'low_correct_group':low,'selective_correct_group':high,
                      'first_batch_high_origin_witnesses':len(high),
                      'first_batch_public_low_response_count':len(low),
                      'second_batch_high_origin_witnesses':P,
                      'all_correct_requests_before_responses':True,
                      'all_late_requests_processed':True,
                      'no_fast_or_slow_decision':True})
    return {'n':n,'f':f,'p':p,'Q':q,'P':P,'F':F,
            'correct':correct,'byzantine':faulty,'suspended_in_suffix':[],
            'ranks':trace,'repeat_invariant':'all correct carry v; private valid B(w) remains available',
            'scope':'stated recovery rule, historical certificates and Q per-origin eligibility; multiple response batches allowed'}

if __name__=='__main__':
    print(json.dumps([run(1),run(2)],indent=2))
