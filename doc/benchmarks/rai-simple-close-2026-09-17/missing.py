import json, glob, os, sys
tr = sys.argv[1]; epoch = int(sys.argv[2]) if len(sys.argv) > 2 else 1
dumps = {}
for f in glob.glob(f'{tr}/*-members-{epoch}-*.txt'):
    pid, _, ep, root = os.path.basename(f)[:-4].split('-')
    dumps[(pid, root)] = set(open(f).read().split())
lag = {}
for f in glob.glob(tr + '/*.jsonl'):
    pid = os.path.basename(f).split('.')[0]
    for line in open(f):
        j = json.loads(line); e = j['event']
        if e['type'] == 'lagging' and e['epoch'] == epoch:
            lag[pid] = (e['decided'], e['state'])
for pid, (decided, state) in lag.items():
    theirs = next((m for (p, r), m in dumps.items() if r == decided), None)
    mine = dumps.get((pid, state))
    if theirs is None or mine is None:
        print(pid, 'no membership dump for', decided[:8], state[:8], sorted(dumps)); continue
    print(pid, 'lagging: decided', decided[:8], len(theirs), 'mine', state[:8], len(mine))
    print('  missing here:', sorted(theirs - mine))
    print('  extra here  :', sorted(mine - theirs))
