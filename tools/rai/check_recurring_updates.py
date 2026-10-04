#!/usr/bin/env python3
"""Recurring-batch, request-bound abstract trace; not a full network verifier.
Correct replicas update once per tick, incorporate every late request, and
reply only after incorporating the triggering request. Old replies remain
usable. Q distinct requests are required before the first phase response.
The trace is bounded-delay but intentionally not a clean first-update round.
"""
from itertools import combinations
from collections import Counter
import json
from check_batched_updates import a_result, b_result, candidates

V,W=1,2

def evolve(requests, correct, q, batches, phase):
    seen={i:set() for i in correct}; states={i:set() for i in correct}
    history=[]
    for tick,batch in enumerate(batches,1):
        for i in correct:
            prior=states[i].copy()
            seen[i].update(batch[i])
            assert i in seen[i] and len(seen[i])>=q
            incoming={requests[j] for j in seen[i]}
            states[i]= {max(incoming)} if phase=='R' else incoming
            if phase=='R' and prior: assert max(states[i])>=max(prior)
            if phase!='R': assert prior<=states[i]
        history.append({i:{'state':states[i].copy(),'processed':seen[i].copy()} for i in correct})
    assert all(seen[i]==set(requests) for i in correct)
    return history

def certificate(history, tick, target, correct_signers, requests, phase, q, byzantine_state=None):
    # Every counted correct reply is bound to this specific triggering request
    # and incorporates it, not just an independent receipt acknowledgement.
    snapshots=[]
    for i in correct_signers:
        entry=history[tick-1][i]
        assert target in entry['processed']
        assert requests[target] in entry['state'] or (phase=='R' and max(entry['state'])>=requests[target])
        snapshots.append(entry['state'])
    if byzantine_state is not None: snapshots.append(byzantine_state)
    assert len(set(correct_signers))==len(correct_signers)
    assert len(snapshots)==q
    # All used origin values must have an actual originating request with Q
    # processing witnesses by the end of this phase. No signature forgery.
    final=history[-1]
    for s in snapshots:
        for value in s:
            assert any(requests[o]==value and sum(o in e['processed'] for e in final.values())>=q for o in requests)
    return max(set.union(*snapshots)) if phase=='R' else (a_result(snapshots) if phase=='A' else b_result(snapshots))

def run(f, ranks=3):
    p=1;n=3*f+3;q=2*f+2;fast=n-1
    correct=tuple(range(1,n-f+1)); faulty=tuple(range(n-f+1,n+1)); byz=faulty[0]
    low=correct[:-2]; high=correct[-2:]
    first_low=set(low)|{byz}; first_mixed=set(low[:q-2])|set(high)
    carry={i:(V if i in low else W) for i in correct}; initial=carry.copy()
    traces=[]
    for rank in range(ranks):
        req={**carry,**{i:V for i in faulty}}
        batches=[{i:(first_low if i in low else first_mixed) for i in correct}, {i:set(req) for i in correct}]
        rh=evolve(req,correct,q,batches,'R')
        votes={i:max(rh[0][i]['state']) for i in correct}
        assert all(rh[-1][i]['state']=={W} for i in correct)
        # Historical replies may provide a prior R result, but even a later
        # R maximum w does not invalidate singleton recovery selecting v.
        rv={i:votes[i] for i in low+(high[0],)}
        rw={i:votes[i] for i in low[:q-2-f]+high};rw.update({i:W for i in faulty})
        assert len(rv)==len(rw)==q
        assert candidates(rv,f,p)=={V} and candidates(rw,f,p)=={W}
        assert all(Counter(votes.values())[v]+f<fast for v in (V,W))
        ar={**{i:V if i in low else W for i in correct},**{i:V for i in faulty}}
        ah=evolve(ar,correct,q,[batches[0],{i:set(ar) for i in correct}],'A')
        at=certificate(ah,1,1,low,ar,'A',q,{V})
        assert at==(True,V)
        br={1:at}
        for i in correct[1:]:
            br[i]=certificate(ah,2,i,correct[:q],ar,'A',q)
            assert br[i]==(False,W)
        br.update({i:(False,W) for i in faulty})
        false_group=correct[1:q]  # Q-1 correct responders; excludes true requester 1.
        first_false=set(false_group)|{byz}
        mixed={1,correct[-1]}|set(false_group[:q-2])
        b1={i:first_false if i in false_group else mixed for i in correct}
        # At tick 2, add every false request, but delay true request 1 at the
        # false group until tick 3. All reply states are updated every tick.
        b2={i:(set(br)-{1}) if i in false_group else set(br) for i in correct}
        bh=evolve(br,correct,q,[b1,b2,{i:set(br) for i in correct}],'B')
        outcomes={}
        for i in low:
            outcomes[i]=certificate(bh,3,i,correct[:q],br,'B',q)
            assert outcomes[i]==('adopt',V)
        for i in high:
            outcomes[i]=certificate(bh,2,i,false_group,br,'B',q,{(False,W)})
            assert outcomes[i]==('adopt',W)
        assert all(bh[-1][i]['state']=={(True,V),(False,W)} for i in correct)
        # No historical or fresh correct B reply is singleton true, so even
        # f Byzantine singleton-true replies cannot form a Q commit certificate.
        assert all(e['state']!={(True,V)} for step in bh for e in step.values())
        # By tick 2 all false origins already have Q witnesses; true origin 1
        # can become fully eligible at tick 3, enabling later mixed certificates.
        assert all(sum(o in e['processed'] for e in bh[1].values())>=q for o in br if o!=1)
        carry={i:v for i,(_,v) in outcomes.items()}
        assert carry==initial
        traces.append({'rank':rank,'first_votes':votes,'R_after_late_update':{i:max(rh[-1][i]['state']) for i in correct},
                       'A_after_late_update':['v','w'],'B_requests':br,
                       'B_after_late_update':[['true','v'],['false','w']],
                       'old_A_true_certificate_requester':1,'old_B_false_certificate_signers':list(false_group)+[byz],
                       'final_carry':carry,'all_requests_incorporated':True})
    return {'n':n,'f':f,'p':p,'Q':q,'semantics':'recurring updates, processed-request replies, historical certificates accepted',
            'ranks':traces,'repeatable_partition':True,'clean_first_update_assumption_satisfied':False}


def unanimous_first_votes(f):
    # Exhaust all binary Byzantine assignments and signer subsets. Analytical
    # proof extends to arbitrarily many Byzantine value names (see document).
    p=1;n=3*f+3;q=2*f+2;c=n-f; checked=0
    for bits in range(1<<f):
        votes={i:(V if i<c or bits&(1<<(i-c))==0 else W) for i in range(n)}
        for m in range(q,n+1):
            for ids in combinations(range(n),m):
                assert candidates({i:votes[i] for i in ids},f,p)=={V}
                checked+=1
    return checked

if __name__=='__main__':
    print(json.dumps({'traces':[run(1),run(2)],'unanimous_first_vote_checks':{f:unanimous_first_votes(f) for f in (1,2)}},indent=2))
