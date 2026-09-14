import sys, collections, re
sys.argv=[sys.argv[0]]
exec(open('/private/tmp/claude-501/-Users-ruimorais-rsnano-node/ff407da5-1c3b-4bab-ba7a-a1ea97707882/scratchpad/prof.py').read().split("for path in sys.argv[1:]")[0])
R='/private/tmp/claude-501/-Users-ruimorais-rsnano-node/ff407da5-1c3b-4bab-ba7a-a1ea97707882/scratchpad/runs/'
def role(name):
    return re.sub(r'\s+\d+$','',name)[:18]
def table(paths):
    cols=[]
    for p in paths:
        ths=parse(p); agg=collections.defaultdict(lambda: [0,0,0,0])  # run, lock, sem, n
        for th in ths:
            r=role(th['name']); agg[r][3]+=1
            for s_,n,st in leaves(th):
                b=s_.split('  ')[0].strip(); k=classify(s_)
                if b in ('sem_wait','sem_post'): agg[r][2]+=n
                elif k=='lock': agg[r][1]+=n
                elif k=='cpu': agg[r][0]+=n
        tot=ths[0]['total'] if ths else 1
        cols.append((agg,tot))
    roles=set()
    for agg,_ in cols: roles|=set(agg)
    def key(r): return -sum(c[0][r][0]+c[0][r][1] for c in cols if r in c[0])
    print('role(threads)          '+'  '.join(f'{p.split("/")[-2][:7]}/{p.split("_")[-1][:5]:5} run lock sem' for p in paths))
    print('  (values = % of one thread-window; run = running or runnable)')
    for r in sorted(roles,key=key)[:22]:
        line=f'{r:18} '
        for agg,tot in cols:
            v=agg.get(r,[0,0,0,0]); line+=f'   n={v[3]:2} {100*v[0]/tot:5.0f} {100*v[1]/tot:5.0f} {100*v[2]/tot:4.0f} '
        print(line)
def subtree(path, needle, depth=4, top=12):
    ths=parse(path); agg=collections.Counter(); total=0
    for th in ths:
        for s_,n,st in leaves(th):
            if classify(s_)=='idle' and s_.split('  ')[0].strip() not in ('sem_wait',): continue
            idx=[i for i,f in enumerate(st) if needle in f]
            if not idx: continue
            i=idx[0]; total+=n
            rest=[short(f) for f in st[i+1:] if ('rsnano' in f or 'psynch' in f or 'sem_wait' in f or 'mutex' in f or 'rwlock' in f or 'ulock' in f)]
            agg[' > '.join(x[:48] for x in rest[:depth])]+=n
    print(f'--- subtree {needle} in {path.split("/")[-2]}/{path.split("_")[-1]}: total {total}')
    for k,n in agg.most_common(top): print(f'   {n:6} {k}')
E=R+'epoch-2000-prof/sample_'; N=R+'noepoch-2000-prof/sample_'
table([E+'off4.txt',E+'off16.txt',E+'off23.txt',N+'off4.txt',N+'off16.txt'])
for needle in ['RequestAggregatorLoop::process','AecService::count_by_behavior','AecService::transition_active','AecService::recovery_entries','EpochCloser::tick','AecService::kudzu_candidates']:
    subtree(E+'off16.txt', needle)
