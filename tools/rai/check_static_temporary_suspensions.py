#!/usr/bin/env python3
"""Bounded recovery checks with a fixed temporarily suspended correct cohort.
Every correct vote is eventually emitted and delivered; Byzantine replicas
withhold votes. This checks recovery availability, not global termination.
"""
from itertools import product
from collections import Counter
import json
from check_batched_updates import candidates
from check_alignment_obligation import run as alignment_example


def run(f,p=1):
    n=3*f+2*p+1;q=n-f-p;P=n-f;F=n-p
    active=tuple(range(q));suspended=tuple(range(q,P))
    counts=Counter();examples={}
    for values in product(range(3),repeat=P):
        votes=dict(enumerate(values))
        before=candidates({i:votes[i] for i in active},f,p)
        after=candidates(votes,f,p)
        assert len(after)<=1
        counts['correct_vote_assignments']+=1
        if len(before)>1:
            counts['ambiguous_before_return']+=1
            tag='singleton_after_return' if after else 'empty_after_return'
            counts['ambiguous_then_'+tag]+=1
            examples.setdefault(tag,{'before':[votes[i] for i in active],
                                     'returning_votes':[votes[i] for i in suspended],
                                     'candidates_before':sorted(before),'candidates_after':sorted(after)})
        counts['all_P_singleton' if after else 'all_P_empty']+=1
    assert counts['ambiguous_before_return']>0
    assert counts['ambiguous_then_empty_after_return']>0
    # The already checked fresh-R split requires no suspended correct process.
    split=alignment_example(f)
    assert split['no_Q_correct_matching_first_votes']
    return {'n':n,'f':f,'p':p,'Q':q,'P':P,'F':F,
            'fixed_core':active,'temporarily_suspended':suspended,
            'counts':dict(counts),'return_examples':examples,
            'all_correct_online_suffice_for_F_matching_votes':P>=F,
            'R_agreement_without_Q_matching_first_votes_survives_final_return':True,
            'scope':'all P votes remove ambiguity; empty recovery still needs valid R evidence'}

if __name__=='__main__':
    print(json.dumps([run(1),run(2)],indent=2))
