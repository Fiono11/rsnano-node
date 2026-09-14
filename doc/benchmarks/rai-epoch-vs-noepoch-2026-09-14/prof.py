import re, sys, collections
IDLE={'__psynch_cvwait','kevent','__semwait_signal','__workq_kernreturn','mach_msg2_trap','__select','poll','__ulock_wait','__sigsuspend','swtch_pri','__accept','__recvfrom','__sendto','__psynch_cvsignal','semaphore_wait_trap','sem_wait','semaphore_signal_trap','__semwait_signal_nocancel','read','__read_nocancel','__pthread_kill','usleep','nanosleep','__sleep'}
LOCK={'__psynch_mutexwait','_pthread_mutex_firstfit_lock_slow','__psynch_rw_rdlock','__psynch_rw_wrlock','_pthread_rwlock_lock_slow','__psynch_mutexdrop','__ulock_wait2','__ulock_wait','__psynch_rw_unlock'}
def parse(path):
    threads=[]; cur=None; stack=[]
    for line in open(path, errors='replace'):
        m=re.match(r'^    (\d+)\s+Thread_\d+(?::\s*(.*)|\s+(.*))?$', line)
        if m:
            cur={'name':(m.group(2) or m.group(3) or 'main').strip(),'total':int(m.group(1)),'frames':[]}; threads.append(cur); stack=[]; continue
        m=re.match(r'^(\s+(?:[+!:|]\s*)*)(\d+)\s+(.*?)(?:\s+\(in .*)?(?:\s+\[0x.*)?$', line)
        if not m or cur is None: continue
        depth=len(m.group(1)); n=int(m.group(2)); sym=m.group(3).strip()
        while stack and stack[-1][0]>=depth: stack.pop()
        stack.append((depth,sym,n))
        cur['frames'].append((depth,sym,n,[s[1] for s in stack]))
    return threads
def leaves(th):
    fr=th['frames']; out=[]
    for i,(d,s,n,st) in enumerate(fr):
        nxt=fr[i+1][0] if i+1<len(fr) else 0
        if nxt<=d: out.append((s,n,st))
    return out
def classify(sym):
    b=sym.split('  ')[0].strip()
    if b in IDLE: return 'idle'
    if b in LOCK: return 'lock'
    return 'cpu'
def short(s): return re.sub(r'::h[0-9a-f]{16}$','',s)[:90]
for path in sys.argv[1:]:
    ths=parse(path); print('=====',path.split('/')[-2:], 'threads',len(ths))
    rows=[]; leafcpu=collections.Counter(); incl=collections.Counter()
    for th in ths:
        c=collections.Counter()
        for s,n,st in leaves(th):
            k=classify(s); c[k]+=n
            if k!='idle':
                leafcpu[short(s)]+=n
                seen=set()
                for f in st:
                    f=short(f)
                    if f in seen: continue
                    seen.add(f); incl[f]+=n
        rows.append((c['cpu']+c['lock'],th['name'],c['cpu'],c['lock'],c['idle']))
    rows.sort(reverse=True)
    tot=sum(r[2] for r in rows); totl=sum(r[3] for r in rows)
    print(f'TOTAL cpu={tot} lock={totl}  (samples at 1ms over 4s => 4000 = 1 core)')
    for r in rows[:14]: print(f'  {r[1][:40]:40} cpu={r[2]:5} lock={r[3]:5} idle={r[4]:5}')
    print(' top leaves (cpu+lock):')
    for s,n in leafcpu.most_common(18): print(f'   {n:6} {s}')
    lockby=collections.Counter()
    for th in ths:
        for s_,n,st in leaves(th):
            if classify(s_)=='lock':
                fr=[short(f) for f in st if 'rsnano' in f]
                key=th['name'][:18]+' <- '+(fr[-1][:60] if fr else '?')+' <- '+(fr[-2][:50] if len(fr)>1 else '')
                lockby[key]+=n
    print(' lock waits by thread/caller:')
    for k_,n in lockby.most_common(14): print(f'   {n:6} {k_}')
    semby=collections.Counter(); semtot=collections.Counter()
    for th in ths:
        for s_,n,st in leaves(th):
            b=s_.split('  ')[0].strip()
            if b in ('sem_wait','sem_post') and any('begin_read' in f or 'begin_write' in f for f in st):
                kind='begin_write' if any('begin_write' in f for f in st) else 'begin_read'
                fr=[short(f) for f in st if 'rsnano' in f and 'nullable_lmdb' not in f and 'store_lmdb' not in f]
                semby[(kind, th['name'][:16], fr[-1][:70] if fr else '?')]+=n; semtot[kind]+=n
    print(f' LMDB semaphore waits: {dict(semtot)}')
    for k_,n in semby.most_common(16): print(f'   {n:6} {k_}')
    print(' top inclusive rsnano frames:')
    k=0
    for s,n in incl.most_common(400):
        if 'rsnano' in s or 'nano' in s.lower():
            print(f'   {n:6} {s}'); k+=1
            if k>=40: break
