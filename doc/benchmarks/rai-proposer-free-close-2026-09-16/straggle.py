import json, re, sys, collections
log, audit = sys.argv[1], sys.argv[2]
maxpending = int(sys.argv[3]) if len(sys.argv) > 3 else 3
KIND = {0:"activate",1:"notarized",2:"finalized",4:"finalized-implicit",5:"fast-final",6:"slow-final",7:"timeout-cert",8:"first/ft-vote",9:"second-look",10:"timeout-eligible",11:"final-vote",12:"timeout-vote",13:"notarize-vote"}
pids = {}; waits = []; forks = collections.defaultdict(list); reached = {}; closed = {}
for line in open(log):
    m = re.search(r"NANOSPAM_NODE (\{.*\})$", line)
    if m:
        j = json.loads(m.group(1)); pids[j["pid"]] = j["pr"]; continue
    m = re.search(r"(EPOCH_DRAIN_WAIT|FORK_TRACE|EPOCH_COUNT_REACHED|EPOCH_CLOSED) (\{.*\})$", line)
    if not m: continue
    try: j = json.loads(m.group(2))
    except Exception: continue
    if m.group(1) == "EPOCH_DRAIN_WAIT": waits.append(j)
    elif m.group(1) == "FORK_TRACE": forks[j.get("root")].append(j)
    elif m.group(1) == "EPOCH_COUNT_REACHED": reached[(j["epoch"], j["pid"])] = j["unix_us"]
    else: closed[(j["epoch"], j["pid"])] = j["unix_us"]
a = json.load(open(audit))
t0 = min(e[4] for pr in a.values() for e in pr["events"])
def ts(ns): return f"{(ns - t0)/1e9:7.3f}"
roots = collections.OrderedDict()
for w in waits:
    if w.get("pending", 10**9) <= maxpending:
        for ex in w["examples"]:
            if "root" in ex: roots.setdefault(ex["root"], []).append((pids.get(w["pid"]), w["epoch"], w["pending"], ex))
print("epoch boundaries (s):")
for (epoch, pid), us in sorted(reached.items()): print(f"  epoch {epoch} pr{pids.get(pid)} drain {ts(us*1000)} close {ts(closed[(epoch,pid)]*1000) if (epoch,pid) in closed else 0}")
for root, seen in roots.items():
    print("=" * 100); print("root", root[:16], "seen pending at", sorted(set((pr, e, p) for pr, e, p, _ in seen)))
    ex = seen[-1][3]
    print("  last diag: state", ex.get("state"), "epoch", ex.get("epoch"), "candidates", [c[:8] for c in ex.get("candidates", [])], "participation", [(p["representative"][:6], p["kind"]) for p in ex.get("participation", [])], "tc", ex.get("timeout_certificate"), "eligible", ex.get("timeout_eligible"))
    for f in forks.get(root, []): print("  fork_trace pr", pids.get(f["pid"]), f["hash"][:8], "added", f["added"])
    rows = []
    for pr, data in a.items():
        for e in data["events"]:
            if e[1] == root: rows.append((e[4], int(pr), e[0], e[2][:8], e[3]))
    for t, pr, k, h, ep in sorted(rows):
        print(f"  {ts(t)} pr{pr} e{ep} {KIND.get(k, k):16} {h}")
    for pr, data in a.items():
        for act in data["active"] or []:
            if act.get("root") == root: print(f"  still active at pr{pr}: epoch {act['epoch']} participation {[(p['representative'][:6], p['kind']) for p in act['participation']]}")
