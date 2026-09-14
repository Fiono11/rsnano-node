import json,re,sys
S='/private/tmp/claude-501/-Users-ruimorais-rsnano-node/ff407da5-1c3b-4bab-ba7a-a1ea97707882/scratchpad/runs/'
D='doc/benchmarks/rai-epoch-vs-noepoch-2026-09-14/'
def cnt(c): return {(e['type'],e['detail'],e['dir']):int(e['value']) for e in c['entries']} if c and 'entries' in c else {}
for arg in sys.argv[1:]:
    n,out=arg.split('=')
    t=open(S+n+'/nanospam.log',errors='replace').read()
    bench=json.loads(re.findall(r'BENCHMARK_RESULT (\{.*)', t)[-1]); perf=json.loads(re.findall(r'EPOCH_PERFORMANCE_RESULT (\{.*)', t)[-1])
    cmd=re.search(r'extra=(.*?) rsnano=', t).group(1).strip()
    stats=json.load(open(S+n+'/stats_pr0.json'))
    counters={e['type']+'/'+e['detail']+'/'+e['dir']:int(e['value']) for e in stats['entries'] if int(e['value'])}
    cpu=[]
    for line in open(S+n+'/cpu.log'):
        m=re.match(r'T=(\d+) CPU=(.*)', line); cpu.append(sum(float(p.split()[1]) for p in m.group(2).split(';') if p.strip()))
    busy=[c for c in cpu if c>300]
    prev=None; t0=None; ev={}
    for line in open(S+n+'/samples.jsonl'):
        s=json.loads(line); c=cnt(s['counters'])
        if not c: continue
        if prev:
            pc=cnt(prev['counters']); dt=s['t']-prev['t'] or 1
            if (c.get(('active_elections','started','in'),0)-pc.get(('active_elections','started','in'),0))/dt>100 and t0 is None: t0=prev['t']
            if t0 is not None:
                bc=s['count'] or {}
                if 'draining_epoch' in bc: ev.setdefault('pr0_drain_e'+bc['draining_epoch'], s['t']-t0)
                for e in (bc.get('closed_epochs') or {}): ev.setdefault('pr0_closed_e'+e, s['t']-t0)
        prev=s
    agree=re.findall(r'TERMINATION_RESULT (\{.*)', t)
    json.dump({'run':n,'command':'nanospam --prs 6 --no-prio --fork-percentage 5 --blocks 50000 --accounts 50000 --rate 2000 --no-kill '+cmd,'exit':re.search(r'NANOSPAM_EXIT=(\d)',t).group(1),'bench':bench,'perf':perf,'pr0_epoch_events_s':ev,'termination_agreement':json.loads(agree[-1]) if agree else None,'pr0_counters':counters,'mean_node_cpu_percent_while_busy':round(sum(busy)/len(busy)/6,1) if busy else None}, open(D+out+'.json','w'), indent=1)
    print(out, bench['average_nonfork_confirmation_ms'], ev)
