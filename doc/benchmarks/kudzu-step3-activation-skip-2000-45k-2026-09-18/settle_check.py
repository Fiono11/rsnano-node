#!/usr/bin/env python3
"""Run termination condition: every PR has settled every election, and in the same way.

For every root seen on any PR, each PR must be in one of two states:
  * finalized: a candidate of the root is cemented in its ledger (no election), or
  * settled: an election in state `settled` with its certificate set and no finalization
    certificate possible any more (a settled election that can still be finalized is pending).
Consistency across PRs:
  * if any PR finalized block X, every other PR must have finalized X or hold a settled
    election whose only certificate is X (no second notarization, no timeout certificate);
  * if no PR finalized the root, all PRs must hold identical certificate sets (notarized
    blocks + timeout certificate).
Prints SETTLED_CONSISTENT when all of this holds, INCONSISTENT when a settled root violates
it, TIMEOUT when the deadline passes.
"""
import collections
import json
import sys
import time
import urllib.request

PORTS = [17076 + 10 * i for i in range(6)]
POLL = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
DEADLINE = float(sys.argv[2]) if len(sys.argv) > 2 else 900.0


def rpc(port, body):
    req = urllib.request.Request(
        f"http://[::1]:{port}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    return json.loads(urllib.request.urlopen(req, timeout=60).read())


def snapshot():
    """per PR: {root: (state, certs, timeout, candidates)} and block counts"""
    per_pr = []
    counts = []
    for port in PORTS:
        counts.append(rpc(port, {"action": "block_count"}))
        roots = rpc(port, {"action": "confirmation_active"}).get("confirmations", [])
        view = {}
        for root in roots:
            info = rpc(
                port,
                {"action": "confirmation_info", "root": root, "representatives": "false", "contents": "false"},
            )
            certs = info.get("certificates") or {}
            view[root] = (
                info.get("state"),
                frozenset(certs.get("notarized", [])),
                bool(certs.get("timeout")),
                tuple(info.get("blocks", {}).keys()),
                bool(certs.get("finalizable")),
            )
        per_pr.append(view)
    return per_pr, counts


_cemented = {}  # (port, hash) -> True once seen cemented; cementing is permanent


def finalized_block(port, candidates):
    for hash in candidates:
        if _cemented.get((port, hash)):
            return hash
    for hash in candidates:
        try:
            info = rpc(port, {"action": "block_info", "hash": hash, "json_block": "true"})
        except Exception:
            continue
        if info.get("confirmed") == "true":
            _cemented[(port, hash)] = True
            return hash
    return None


def check(per_pr, counts):
    problems = []  # (kind, root, detail); kind in {"pending", "inconsistent"}
    all_roots = set().union(*[set(v) for v in per_pr])
    for root in sorted(all_roots):
        candidates = set()
        for view in per_pr:
            if root in view:
                candidates.update(view[root][3])
        outcome = []  # per PR: ("final", X) | ("settled", certs, timeout) | ("pending", state)
        for i, view in enumerate(per_pr):
            if root in view:
                state, certs, timeout, _, finalizable = view[root]
                if state == "settled" and not finalizable:
                    outcome.append(("settled", certs, timeout))
                elif state == "settled":
                    # still collecting the final votes of its notarized block
                    outcome.append(("pending", "settled, finalizable"))
                else:
                    outcome.append(("pending", state))
            else:
                x = finalized_block(PORTS[i], candidates)
                outcome.append(("final", x) if x else ("pending", "no election, nothing cemented"))
        if any(o[0] == "pending" for o in outcome):
            problems.append(("pending", root, outcome))
            continue
        finals = {o[1] for o in outcome if o[0] == "final"}
        if len(finals) > 1:
            problems.append(("inconsistent", root, f"two different blocks finalized: {outcome}"))
            continue
        if finals:
            x = next(iter(finals))
            for i, o in enumerate(outcome):
                if o[0] == "settled" and (o[1] != frozenset([x]) or o[2]):
                    problems.append(("inconsistent", root, f"PR{i} settled with {sorted(h[:8] for h in o[1])} timeout={o[2]} while {x[:8]} is finalized elsewhere: {outcome}"))
                    break
        else:
            sets = {(o[1], o[2]) for o in outcome}
            if len(sets) > 1:
                problems.append(("inconsistent", root, f"certificate sets differ: {[(sorted(h[:8] for h in o[1]), o[2]) for o in outcome]}"))
    return all_roots, problems


start = time.time()
last_pending = None
while True:
    per_pr, counts = snapshot()
    all_roots, problems = check(per_pr, counts)
    pending = [p for p in problems if p[0] == "pending"]
    inconsistent = [p for p in problems if p[0] == "inconsistent"]
    elapsed = time.time() - start
    states = [collections.Counter(v[0] for v in view.values()) for view in per_pr]
    cemented = [c.get("cemented") for c in counts]
    print(f"t={elapsed:.0f}s roots={len(all_roots)} pending={len(pending)} inconsistent={len(inconsistent)} cemented={cemented} states={[dict(s) for s in states]}", flush=True)
    for kind, root, detail in inconsistent[:3]:
        print(f"  transient? root {root[:16]}: {detail}")
        for i, port in enumerate(PORTS):
            if root in per_pr[i]:
                info = rpc(port, {"action": "confirmation_info", "root": root, "representatives": "true", "contents": "false"})
                reps = {h[:8]: b.get("representatives_kudzu") for h, b in info.get("blocks", {}).items()}
                print(f"    PR{i}: state={info.get('state')} certs={info.get('certificates')} reps={reps}")
    if not pending:
        if inconsistent:
            print("INCONSISTENT")
            for kind, root, detail in inconsistent[:20]:
                print(f"  root {root[:16]}: {detail}")
            sys.exit(2)
        print(f"SETTLED_CONSISTENT after {elapsed:.0f}s: {len(all_roots)} roots settled identically on all PRs, cemented={cemented}")
        sys.exit(0)
    if elapsed > DEADLINE:
        print("TIMEOUT")
        for kind, root, detail in pending[:20]:
            print(f"  root {root[:16]}: {detail}")
        for kind, root, detail in inconsistent[:20]:
            print(f"  root {root[:16]}: {detail}")
        sys.exit(1)
    time.sleep(POLL)
