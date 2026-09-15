import json, re, sys
S='/private/tmp/claude-501/-Users-ruimorais-rsnano-node/73cc949b-b1b3-4bb1-a391-89e589580254/scratchpad/runs/'
KEYS=[('active_elections','started'),('active_elections','confirmed'),('active_elections','retired'),
      ('requests','requests_generated_hashes'),('election','confirmation_request'),
      ('drop','confirm_ack'),('drop','publish'),('vote_processor','vote_processed')]
def counters(c):
    if not c or 'entries' not in c: return {}
    return {(e['type'],e['detail']):int(e['value']) for e in c['entries']}
def summarize(name):
    t=open(S+name+'/nanospam.log').read()
    b=json.loads(re.findall(r'BENCHMARK_RESULT (\{.*)', t)[-1])
    m=re.findall(r'EPOCH_PERFORMANCE_RESULT (\{.*)', t); d=json.loads(m[-1]) if m else {'rows':[],'publication_windows':[]}
    closes=re.findall(r'EPOCH_CLOSED (\{.*)', t)
    ecr=re.findall(r'EPOCH_CLOSE_RESULT (\{.*)', t)
    exitm=re.search(r'NANOSPAM_EXIT=(\d+)', t)
    print(f"== {name}: cps={b['confirmation_rate_cps']:.0f} mean_nonfork={b['average_nonfork_confirmation_ms']:.0f} mean_fork={b.get('average_fork_confirmation_ms',0) or 0:.0f} confirmed={b['confirmed_blocks']} exit={exitm.group(1) if exitm else '?'}")
    if ecr:
        r=json.loads(ecr[-1]); print(f"   close_result success={r.get('success')} elapsed={r.get('elapsed_seconds', r.get('elapsed_ms'))}")
    print("   epoch forked | term n mean p95 | fin n mean p95")
    for r in d['rows']:
        te,fi=r['terminated'],r['finalized']
        print(f"     {r['epoch']} {str(r['forked']):5} | {te['count']:6} {te['mean_ms'] or 0:6.0f} {te['p95_ms'] or 0:6.0f} | {fi['count']:6} {fi['mean_ms'] or 0:6.0f} {fi['p95_ms'] or 0:6.0f}")
    # close timing from EPOCH_CLOSED lines with time offsets unknown; count rounds
    for c in closes[:12]:
        try: j=json.loads(c)
        except Exception: continue
        print(f"   closed epoch={j['epoch']} round={j['round']} blocks={j['blocks']} discarded={j['discarded']}")
    try: l=[json.loads(x) for x in open(S+name+'/samples.jsonl')]
    except FileNotFoundError: return
    cpu={}
    for line in open(S+name+'/cpu.log'):
        mm=re.match(r'T=(\d+) CPU=(.*)', line); tt=int(mm.group(1))
        parts=[p.split() for p in mm.group(2).split(';') if p.strip()]
        cpu[tt]=[(int(p[0]),float(p[1])) for p in parts]
    prev=None
    print("   t | cpu6 cpu_pr0 | active | cemented | deltas/s: " + " ".join(f"{a}/{b}" for a,b in KEYS))
    for s in l:
        c=counters(s['counters'])
        if not c: continue
        cp=sorted(cpu.get(s['t'],[]))
        act=s['active'].get('unconfirmed') if s['active'] else None
        cem=s['count'].get('cemented') if s['count'] else None
        if prev and (s['t']//2)%3==0:
            dt=max(1,s['t']-prev['t']); pc=counters(prev['counters'])
            deltas=[(c.get(k,0)-pc.get(k,0))/dt for k in KEYS]
            print(f"   {s['t']:4} | {sum(x for _,x in cp):5.0f} {cp[0][1] if cp else 0:5.0f} | {act:>6} | {cem:>6} | "+" ".join(f"{x:6.0f}" for x in deltas))
        prev=s
for n in sys.argv[1:]: summarize(n)
