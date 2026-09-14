import json, re, sys
S='/private/tmp/claude-501/-Users-ruimorais-rsnano-node/ff407da5-1c3b-4bab-ba7a-a1ea97707882/scratchpad/runs/'
KEYS=[('active_elections','started'),('active_elections','confirmed'),('active_elections_stopped','active'),
      ('requests','requests_generated_hashes'),('election','confirmation_request'),('message_processor_overfill','publish'),
      ('vote_processor_overfill','pr'),('drop','confirm_ack'),('drop','publish'),('election_scheduler','loop'),
      ('vote_processor','vote_processed'),('blockprocessor','process'),('vote_generator','generator_broadcasts'),('vote_generator_final','generator_broadcasts')]
def counters(c):
    if not c or 'entries' not in c: return {}
    return {(e['type'],e['detail']):int(e['value']) for e in c['entries']}
def summarize(name, every=2):
    t=open(S+name+'/nanospam.log').read()
    m=re.findall(r'EPOCH_PERFORMANCE_RESULT (\{.*)', t); d=json.loads(m[-1])
    b=json.loads(re.findall(r'BENCHMARK_RESULT (\{.*)', t)[-1])
    print(f"== {name}: cps={b['confirmation_rate_cps']:.0f} mean_nonfork={b['average_nonfork_confirmation_ms']:.0f} confirmed={b['confirmed_blocks']}")
    print("epoch forked | term n mean p95 | fin n mean p95")
    for r in d['rows']:
        te,fi=r['terminated'],r['finalized']
        print(f"  {r['epoch']} {str(r['forked']):5} | {te['count']:6} {te['mean_ms'] or 0:6.0f} {te['p95_ms'] or 0:6.0f} | {fi['count']:6} {fi['mean_ms'] or 0:6.0f} {fi['p95_ms'] or 0:6.0f}")
    print("window e | n | term mean p95 | fin mean p95 | first_to_term mean p95 | pub->first mean")
    for w in d['publication_windows']:
        if w['forked']: continue
        te,fi,ft=w['termination'],w['finalization'],w['first_to_termination']
        pf = (te['mean_ms'] or 0)-(ft['mean_ms'] or 0)
        print(f"  {w['publication_start_seconds']:3} {w['epoch']} | {te['count']:5} | {te['mean_ms'] or 0:5.0f} {te['p95_ms'] or 0:5.0f} | {fi['mean_ms'] or 0:5.0f} {fi['p95_ms'] or 0:5.0f} | {ft['mean_ms'] or 0:5.0f} {ft['p95_ms'] or 0:5.0f} | {pf:5.0f}")
    # samples
    try: l=[json.loads(x) for x in open(S+name+'/samples.jsonl')]
    except FileNotFoundError: return
    prev=None
    print("t | cpu_sum(6 nodes) cpu_pr0 | active | cemented | deltas/s: " + " ".join(f"{a}/{b}" for a,b in KEYS))
    cpu={}
    for line in open(S+name+'/cpu.log'):
        mm=re.match(r'T=(\d+) CPU=(.*)', line); t=int(mm.group(1))
        parts=[p.split() for p in mm.group(2).split(';') if p.strip()]
        cpu[t]=[(int(p[0]),float(p[1])) for p in parts]
    for s in l:
        c=counters(s['counters']); 
        if not c: continue
        cp=cpu.get(s['t'],[]); cp=sorted(cp)
        act=s['active'].get('unconfirmed') if s['active'] else None
        cem=s['count'].get('cemented') if s['count'] else None
        if prev:
            dt=max(1,s['t']-prev['t']); pc=counters(prev['counters'])
            deltas=[ (c.get(k,0)-pc.get(k,0))/dt for k in KEYS]
            if (s['t']//every)%1==0:
                print(f"{s['t']:4} | {sum(x for _,x in cp):6.0f} {cp[0][1] if cp else 0:5.0f} | {act:>6} | {cem:>6} | "+" ".join(f"{x:6.0f}" for x in deltas))
        prev=s
for n in sys.argv[1:]: summarize(n)
