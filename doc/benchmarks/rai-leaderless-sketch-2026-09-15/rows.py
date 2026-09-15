import json,re,sys
def f(v): return f"{v:6.0f}" if isinstance(v,(int,float)) else "     -"
for line in open(sys.argv[1]):
    m=re.search(r"EPOCH_PERFORMANCE_RESULT (\{.*\})$", line)
    if not m: continue
    j=json.loads(m.group(1))
    for r in j['rows']:
        t=r['terminated']; fi=r['finalized']
        print(f"rows epoch {r['epoch']} forked={r['forked']!s:5} terminated n={t['count']:5} mean/p95 {f(t['mean_ms'])}/{f(t['p95_ms'])}   finalized n={fi['count']:5} mean/p95 {f(fi['mean_ms'])}/{f(fi['p95_ms'])}")
