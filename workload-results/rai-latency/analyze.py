"""Audit latency distributions; missing outcomes stay visible as censored roots."""
import collections, gzip, json, math, pathlib, sys
p = pathlib.Path(sys.argv[1])
with gzip.open(p / 'audit.json.gz', 'rt') as f:
    audit = json.load(f)
roots = {r: fork is not None for r, _, fork in audit['workload']}
cutoff = audit['performance_cutoff']

def describe(values):
    values = sorted(values)
    if not values:
        return {'samples': 0}
    return dict(samples=len(values), mean_ms=sum(values)/len(values),
                **{f'p{q}_ms': values[max(0, math.ceil(len(values)*q/100)-1)] for q in (50,95,99)}, max_ms=values[-1])

reports = []
for pr, events in enumerate(audit['observations']):
    by_root = collections.defaultdict(list)
    for k,r,h,e,t in events:
        if r in roots and t <= cutoff:
            by_root[r].append((k,h,e,t))
    samples = collections.defaultdict(list)
    counts = collections.Counter()
    for root, fork in roots.items():
        cohort = 'fork' if fork else 'nonfork'
        ev = by_root[root]
        starts = [t for k,h,e,t in ev if k == 0]
        finals = [(t,e) for k,h,e,t in ev if k in (2,4)]
        terms = [(t,e) for k,h,e,t in ev if k in (1,2,4,7)]
        counts[cohort + '/roots'] += 1
        if not starts:
            counts[cohort + '/missing_insertion'] += 1
            continue
        start = min(starts)
        for label, outcomes in [('finalization', finals), ('termination', terms)]:
            if not outcomes:
                counts[cohort + '/' + label + '_missing'] += 1
                continue
            end, epoch = min(outcomes)
            duration = (end-start)/1e6
            samples[cohort+'/'+label].append(duration)
            samples[f'epoch{epoch}/{cohort}/{label}'].append(duration)
        firsts = [t for k,h,e,t in ev if k == 8]
        if firsts:
            samples[cohort+'/insertion_to_first_applied'].append((min(firsts)-start)/1e6)
            if finals:
                samples[cohort+'/first_applied_to_finalization'].append((min(finals)[0]-min(firsts))/1e6)
        for epoch in {e for k,h,e,t in ev}:
            ee = [(k,t) for k,h,e,t in ev if e == epoch]
            for name, begin, end in [('second_look_ready_to_notarize_applied',9,13),('timeout_ready_to_timeout_applied',10,12),('timeout_ready_to_certificate',10,7),('notarization_to_finalization',1,2)]:
                bs=[t for k,t in ee if k==begin];es=[t for k,t in ee if k==end]
                if bs and es and min(es)>=min(bs):
                    samples[cohort+'/'+name].append((min(es)-min(bs))/1e6)
    reports.append({'pr':pr, 'counts':dict(counts), 'latencies':{k:describe(v) for k,v in sorted(samples.items())}})
result={'basis':'Local earliest election insertion to first local outcome across epochs, before performance cutoff. Missing outcomes are censored, not zero latency. Per-PR samples are correlated.', 'prs':reports}
(p/'latency.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(reports[0],indent=2))
