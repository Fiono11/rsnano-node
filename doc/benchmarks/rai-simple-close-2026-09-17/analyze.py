import json, re, sys, collections

def load(path):
    ev = collections.defaultdict(list); bench = None; perf = None; pids = {}
    for line in open(path):
        line = line.rstrip("\n")
        m = re.search(r"(EPOCH_COUNT_REACHED|EPOCH_CLOSED|EPOCH_CLOSE_COMPLETE|EPOCH_CLOSE_PROGRESS|EPOCH_MEMBERS_CHANGED|EPOCH_CLOSE_RECONCILE|EPOCH_CLOSE_SOLICIT|EPOCH_DRAIN_WAIT|EPOCH_CLOSE_RESULT|EPOCH_CLOSE_WAIT) (\{.*\})$", line)
        if m:
            try: ev[m.group(1)].append(json.loads(m.group(2)))
            except Exception: pass
            continue
        m = re.search(r"NANOSPAM_NODE (\{.*\})$", line)
        if m:
            j = json.loads(m.group(1)); pids[j["pid"]] = j["pr"]; continue
        m = re.search(r"BENCHMARK_RESULT (\{.*\})$", line)
        if m: bench = json.loads(m.group(1)); continue
        m = re.search(r"EPOCH_PERFORMANCE_RESULT (\{.*\})$", line)
        if m: perf = json.loads(m.group(1)); continue
    return ev, bench, perf, pids

def f(v, w=6, d=0):
    return f"{v:{w}.{d}f}" if isinstance(v, (int, float)) else " " * (w - 1) + "-"

def phases(ev, origin_us):
    """Return per-epoch (first_drain, last_drain, first_close, last_close) as seconds since first publication."""
    reached = collections.defaultdict(dict); closed = collections.defaultdict(dict)
    for e in ev["EPOCH_COUNT_REACHED"]: reached[e["epoch"]][e["pid"]] = e["unix_us"]
    for e in ev["EPOCH_CLOSED"]: closed[e["epoch"]][e["pid"]] = e
    out = {}
    for epoch in sorted(set(reached) | set(closed)):
        d = sorted(reached.get(epoch, {}).values()); c = sorted(x["unix_us"] for x in closed.get(epoch, {}).values())
        out[epoch] = dict(
            first_drain=(d[0] - origin_us) / 1e6 if d else None, last_drain=(d[-1] - origin_us) / 1e6 if d else None,
            first_close=(c[0] - origin_us) / 1e6 if c else None, last_close=(c[-1] - origin_us) / 1e6 if c else None,
            majority_close=(c[3] - origin_us) / 1e6 if len(c) >= 4 else None,
            drains=len(d), closes=len(c), rows=closed.get(epoch, {}),
            per_pid={pid: ((reached.get(epoch, {}).get(pid, 0) - origin_us) / 1e6 if pid in reached.get(epoch, {}) else None,
                           (x["unix_us"] - origin_us) / 1e6, x["round"]) for pid, x in closed.get(epoch, {}).items()})
    return out

def phase_label(t, ph):
    """Label a time (s since first publication) with the epoch phase: an epoch is
    'closing' from the first drain until 4 of 6 PRs closed; '+s' marks a straggler still closing."""
    label = None
    for epoch, p in sorted(ph.items()):
        if p["first_drain"] is not None and t < p["first_drain"]:
            return f"epoch {epoch}" + (label or "")
        done = p["majority_close"] if p["majority_close"] is not None else p["last_close"]
        if done is not None and t < done:
            return f"closing {epoch}"
        label = "+s" if p["last_close"] is not None and t < p["last_close"] else ""
    if ph:
        return f"after close {max(ph)}" + (label or "")
    return "?"

def main(paths):
    for path in paths:
        ev, bench, perf, pids = load(path)
        name = path.split("/")[-1]
        print("=" * 100); print(name)
        if bench:
            print("BENCHMARK:", {k: bench[k] for k in ("published_blocks", "confirmed_blocks", "confirmed_nonforks", "confirmed_forks", "duration_seconds", "confirmation_rate_cps", "average_confirmation_ms", "average_nonfork_confirmation_ms", "average_fork_confirmation_ms") if k in bench})
        else:
            print("NO BENCHMARK_RESULT"); 
        if not perf:
            print("NO EPOCH_PERFORMANCE_RESULT"); continue
        tl = perf.get("timeline")
        origin = (tl or perf.get("timeline_omitted") or {}).get("first_publication_unix_us_lower_bound")
        if origin is None:
            # no wall-clock anchor: use the earliest drain as an approximate origin
            print("WARNING: no timeline anchor; phase times relative to first EPOCH_COUNT_REACHED")
            origin = min(e["unix_us"] for e in ev["EPOCH_COUNT_REACHED"])
        ph = phases(ev, origin)
        for epoch, p in sorted(ph.items()):
            rows = p["rows"]
            hashes = {e["hash"] for e in rows.values()}
            rounds = sorted(e["round"] for e in rows.values())
            print(f"epoch {epoch}: drains={p['drains']} closes={p['closes']} hashes={len(hashes)} rounds={rounds} blocks={sorted({e['blocks'] for e in rows.values()})} discarded={sorted(e['discarded'] for e in rows.values())}")
            if p["first_drain"] is not None:
                print(f"  drain reached at t={p['first_drain']:.1f}..{p['last_drain']:.1f}s (spread {p['last_drain']-p['first_drain']:.2f}s)", end="")
                if p["first_close"] is not None:
                    print(f"; closed at t={p['first_close']:.1f}..{p['last_close']:.1f}s; close after last drain: first {p['first_close']-p['last_drain']:.2f}s last {p['last_close']-p['last_drain']:.2f}s")
                else:
                    print("; NOT CLOSED")
            print("  per PR (drain t, close t, round): " + ", ".join(f"PR{pids.get(pid, '?')}=({f(dr,5,1) if dr is not None else '  -'},{cl:6.1f},r{rd})" for pid, (dr, cl, rd) in sorted(p["per_pid"].items(), key=lambda kv: pids.get(kv[0], 99))))
        print("drain_wait events:", len(ev["EPOCH_DRAIN_WAIT"]), " members_changed:", len(ev["EPOCH_MEMBERS_CHANGED"]), " reconcile:", len(ev["EPOCH_CLOSE_RECONCILE"]), " solicit:", len(ev["EPOCH_CLOSE_SOLICIT"]), " close_wait polls:", len(ev["EPOCH_CLOSE_WAIT"]))
        print("per-epoch latency (publication -> PR0 websocket outcome):")
        for r in perf["rows"]:
            t = r["terminated"]; fi = r["finalized"]
            print(f"  epoch {r['epoch']} forked={r['forked']!s:5} terminated n={t['count']:5} mean/p95/max {f(t['mean_ms'])}/{f(t['p95_ms'])}/{f(t['max_ms'])} ms   finalized n={fi['count']:5} mean/p95/max {f(fi['mean_ms'])}/{f(fi['p95_ms'])}/{f(fi['max_ms'])} ms")
        # time series: non-fork finalization throughput and latency by 5 s receipt window (all epochs merged per window)
        print("non-fork time series (5 s windows by finalization receipt time; blocks/s = finalizations per second; latency = publication->finalization):")
        win = collections.defaultdict(lambda: dict(n=0, sum=0.0, p95=[], term_n=0, first=[]))
        for w in perf["outcome_windows"]:
            if w["forked"]: continue
            k = w["outcome_start_seconds"]
            fin = w["finalization"]; term = w["termination"]
            if fin["count"]:
                win[k]["n"] += fin["count"]; win[k]["sum"] += fin["mean_ms"] * fin["count"]; win[k]["p95"].append((fin["count"], fin["p95_ms"]))
            if term["count"]:
                win[k]["term_n"] += term["count"]
            ftf = w["first_to_finalization"]
            if ftf["count"]:
                win[k]["first"].append((ftf["count"], ftf["mean_ms"]))
        # publication windows for publication-cohort latency (which blocks were published in the window)
        pub = collections.defaultdict(lambda: dict(n=0, sum=0.0))
        for w in perf["publication_windows"]:
            if w["forked"]: continue
            fin = w["finalization"]
            if fin["count"]:
                pub[w["publication_start_seconds"]]["n"] += fin["count"]; pub[w["publication_start_seconds"]]["sum"] += fin["mean_ms"] * fin["count"]
        pubs = tl["publications"] if tl else []
        pub_count = collections.Counter((p[0] // 5_000_000) * 5 for p in pubs if not p[1])
        print(f"  {'t(s)':>5} {'phase':<15} {'pub/s':>6} {'term/s':>6} {'fin/s':>6} {'fin mean':>9} {'fin p95~':>9} {'1st->fin':>9} {'pubcohort':>9}")
        for k in sorted(set(win) | set(pub_count)):
            w = win[k]
            mean = w["sum"] / w["n"] if w["n"] else None
            p95 = max(p for _, p in w["p95"]) if w["p95"] else None
            first = sum(c * m for c, m in w["first"]) / sum(c for c, _ in w["first"]) if w["first"] else None
            pc = pub[k]["sum"] / pub[k]["n"] if pub[k]["n"] else None
            print(f"  {k:>5} {phase_label(k + 2.5, ph):<15} {pub_count.get(k,0)/5:>6.0f} {w['term_n']/5:>6.0f} {w['n']/5:>6.0f} {f(mean,9)} {f(p95,9)} {f(first,9)} {f(pc,9)}")
        print()

main(sys.argv[1:])
