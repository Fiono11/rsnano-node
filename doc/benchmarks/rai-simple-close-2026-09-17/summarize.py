import sys, json, re, glob, os
log = sys.argv[1]
rows = []
for line in open(log):
    m = re.search(r"(EPOCH_CLOSED|EPOCH_COUNT_REACHED|EPOCH_CLOSE_LAGGING) (\{.*\})$", line)
    if not m: continue
    try: j = json.loads(m.group(2))
    except Exception: continue
    rows.append((j.get('unix_us', 0), m.group(1), j.get('pid'), j.get('epoch'), j.get('round'), j.get('blocks') or j.get('members')))
t0 = min(r[0] for r in rows if r[0])
for r in sorted(rows):
    print(f"{(r[0]-t0)/1e6 if r[0] else -1:7.2f}s {r[1]:20} pid={r[2]} epoch={r[3]} round={r[4]} blocks={r[5]}")
tr = sys.argv[2] if len(sys.argv) > 2 else None
if tr:
    ev = []
    for f in glob.glob(tr + '/*.jsonl'):
        pid = os.path.basename(f).split('.')[0]
        for line in open(f):
            j = json.loads(line); e = j['event']
            if e['type'] in ('close_vote', 'proposal', 'proposal_received', 'ready', 'round_complete', 'drain_start', 'lagging', 'members_changed'):
                ev.append((j['time_us'], pid, e['epoch'], e['type'], e.get('round'), e.get('kind'), (e.get('state') or '')[:8], e.get('members') or e.get('count')))
    for r in sorted(ev):
        print(f"  {(r[0]-t0)/1e6:7.2f}s pid={r[1]} e{r[2]} {r[3]:17} r{r[4]} kind={r[5]} {r[6]} {r[7] or ''}")
for line in open(log):
    m = re.search(r"EPOCH_CLOSE_TRANSPORT (\{.*\})$", line)
    if m:
        j = json.loads(m.group(1))
        if j['processing_ms'] > 200 or j['total_ms'] > 200 or j['dropped']:
            print('transport', j)
