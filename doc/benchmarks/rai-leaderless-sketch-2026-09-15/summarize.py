import json, sys, re, collections
log = open(sys.argv[1]).read().splitlines()
ev = collections.defaultdict(list)
for line in log:
    m = re.match(r"^(EPOCH_COUNT_REACHED|EPOCH_CLOSED|EPOCH_CLOSE_COMPLETE|EPOCH_CLOSE_PROGRESS|EPOCH_MEMBERS_CHANGED|EPOCH_CLOSE_RECONCILE|EPOCH_CLOSE_SOLICIT) (\{.*\})$", line)
    if m:
        try: ev[m.group(1)].append(json.loads(m.group(2)))
        except Exception: pass
reached = collections.defaultdict(dict); closed = collections.defaultdict(dict)
for e in ev["EPOCH_COUNT_REACHED"]: reached[e["epoch"]][e["pid"]] = e["unix_us"]
for e in ev["EPOCH_CLOSED"]: closed[e["epoch"]][e["pid"]] = e
for epoch in sorted(closed):
    drains = reached.get(epoch, {})
    last_drain = max(drains.values()) if drains else None
    first_drain = min(drains.values()) if drains else None
    rows = closed[epoch]
    hashes = {e["hash"] for e in rows.values()}
    rounds = sorted(e["round"] for e in rows.values())
    times = sorted(e["unix_us"] for e in rows.values())
    print(f"epoch {epoch}: {len(rows)} PRs closed, hashes={len(hashes)}, rounds={rounds}, blocks={sorted({e['blocks'] for e in rows.values()})}, discarded={sorted(e['discarded'] for e in rows.values())}")
    if last_drain:
        print(f"  drain spread {(last_drain-first_drain)/1e6:.2f}s; close after last drain: first {(times[0]-last_drain)/1e6:.2f}s, last {(times[-1]-last_drain)/1e6:.2f}s; close spread {(times[-1]-times[0])/1e6:.2f}s")
print("members_changed events:", len(ev["EPOCH_MEMBERS_CHANGED"]), " reconcile events:", len(ev["EPOCH_CLOSE_RECONCILE"]), " solicit events:", len(ev["EPOCH_CLOSE_SOLICIT"]))
for line in log:
    if line.startswith("BENCHMARK_RESULT") or line.startswith("EPOCH_PERFORMANCE_RESULT"):
        try:
            j = json.loads(line.split(" ",1)[1])
        except Exception:
            print(line[:300]); continue
        if line.startswith("BENCHMARK_RESULT"):
            print("BENCHMARK:", {k:j[k] for k in j if k in ("confirmed_blocks","published_blocks","average_confirmation_ms","average_nonfork_confirmation_ms","average_fork_confirmation_ms","confirmation_rate_cps","duration_seconds","confirmed_forks","confirmed_nonforks")})
        else:
            for row in j.get("rows", j if isinstance(j,list) else []):
                print("EPOCH_PERF:", json.dumps(row)[:400])
            if not isinstance(j, dict) or "rows" not in j:
                print("EPOCH_PERF raw:", line[:600])
