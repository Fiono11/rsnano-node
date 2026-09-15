import json,re,sys
S='/private/tmp/claude-501/-Users-ruimorais-rsnano-node/73cc949b-b1b3-4bb1-a391-89e589580254/scratchpad/runs/'
print(f"{'run':12} {'exit':4} {'cps':4} | e0 fin mean/p95 | e1 fin mean/p95 | close e0 rnd | close e1 rnd | close_after_workload_s | pr0 started retired gen_hashes conf_req drop_pub drop_ack")
for name in sys.argv[1:]:
    t=open(S+name+'/nanospam.log').read()
    b=json.loads(re.findall(r'BENCHMARK_RESULT (\{.*)', t)[-1])
    d=json.loads(re.findall(r'EPOCH_PERFORMANCE_RESULT (\{.*)', t)[-1])
    rows={(r['epoch'],r['forked']):r for r in d['rows']}
    def f(e):
        r=rows.get((e,False)); 
        return f"{r['finalized']['mean_ms'] or 0:5.0f}/{r['finalized']['p95_ms'] or 0:5.0f}" if r else "   -/-   "
    rnd={}
    for l in t.splitlines():
        if 'EPOCH_CLOSED' in l:
            m=re.search(r'"epoch":(\d+).*"round":(\d+)',l)
            if m: rnd.setdefault(int(m.group(1)),[]).append(int(m.group(2)))
    exitm=re.search(r'NANOSPAM_EXIT=(\d+)', t)
    ts=[]
    for l in t.splitlines():
        m=re.search(r'T(\d\d:\d\d:\d\d)\.\d+Z.*(BENCHMARK_RESULT|EPOCH_CLOSE_RESULT)',l)
        if m: ts.append(m.group(1))
    def sec(x): h,mi,s_=x.split(':'); return int(h)*3600+int(mi)*60+int(s_)
    closes = (sec(ts[1])-sec(ts[0])) if len(ts)>=2 else None
    c=json.load(open(S+name+'/stats_pr0.json'))
    e={(x['type'],x['detail']):int(x['value']) for x in c['entries']}
    g=lambda a,b_: e.get((a,b_),0)
    print(f"{name:12} {exitm.group(1) if exitm else '?':4} {b['confirmation_rate_cps']:4.0f} | {f(0)} | {f(1)} | {max(rnd.get(0,[-1])):3} | {max(rnd.get(1,[-1])):3} | {closes!s:>6} | {g('active_elections','started'):6} {g('active_elections','retired'):5} {g('requests','requests_generated_hashes'):7} {g('election','confirmation_request'):6} {g('drop','publish'):6} {g('drop','confirm_ack'):6}")
