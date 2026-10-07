import re, sys, collections
log = open(sys.argv[1]).read()
ev = collections.defaultdict(lambda: collections.defaultdict(list))
for kind, epoch, t in re.findall(r'^(EPOCH_ENDED|EPOCH_REPORT|EPOCH_CLOSE_READY|EPOCH_PROPOSED|EPOCH_CLOSED|EPOCH_INSTALLED|EPOCH_MANIFEST_FETCHED|EPOCH_RECONCILED) epoch=(\d+).*? t=(\d+)$', log, re.M):
    ev[int(epoch)][kind].append(int(t))
for epoch in sorted(ev):
    e = ev[epoch]
    if not e['EPOCH_ENDED'] or not e['EPOCH_CLOSED']: continue
    t0 = min(e['EPOCH_ENDED'])
    f = lambda k, agg: (agg(e[k]) - t0) if e[k] else None
    print(f"epoch {epoch}: ended spread {max(e['EPOCH_ENDED'])-t0} | reports first/last {f('EPOCH_REPORT',min)}/{f('EPOCH_REPORT',max)} | reconciled last {f('EPOCH_RECONCILED',max)} n={len(e['EPOCH_RECONCILED'])} | ready first/last {f('EPOCH_CLOSE_READY',min)}/{f('EPOCH_CLOSE_READY',max)} | proposed first {f('EPOCH_PROPOSED',min)} n={len(e['EPOCH_PROPOSED'])} | manifests fetched {len(e['EPOCH_MANIFEST_FETCHED'])} | closed first/last {f('EPOCH_CLOSED',min)}/{f('EPOCH_CLOSED',max)} | installed last {f('EPOCH_INSTALLED',max)}")
