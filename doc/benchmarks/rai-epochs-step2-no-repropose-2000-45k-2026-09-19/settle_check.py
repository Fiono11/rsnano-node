#!/usr/bin/env python3
"""Run termination condition: every PR has reached the same final state in every epoch.

`final_state` reports, per consensus epoch, an order-independent hash of the blocks finalized
by a certificate of that epoch plus the settled single notarization certificates of that
epoch, and how many of the epoch's elections are still open:
  * pending: not settled, block not cemented;
  * cemented_undecided: block cemented, but this PR still collects the epoch's certificates.
The run is settled once, for every epoch seen on any PR, every PR reports that epoch with
pending = 0 and cemented_undecided = 0. It is consistent when all PRs then report the same
hash per epoch.
Prints SETTLED_CONSISTENT, INCONSISTENT (with the differing elections) or TIMEOUT.
"""
import json
import sys
import time
import urllib.request

PORTS = [17076 + 10 * i for i in range(6)]
POLL = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
DEADLINE = float(sys.argv[2]) if len(sys.argv) > 2 else 60.0


def rpc(port, body):
    req = urllib.request.Request(
        f"http://[::1]:{port}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    return json.loads(urllib.request.urlopen(req, timeout=10).read())


def snapshot():
    """per PR: {epoch: EpochFinalState} and block counts"""
    per_pr = []
    counts = []
    for port in PORTS:
        counts.append(rpc(port, {"action": "block_count"}))
        state = rpc(port, {"action": "final_state"})
        per_pr.append({int(e["epoch"]): e for e in state.get("epochs", [])})
    return per_pr, counts


def open_elections(epoch_state):
    return int(epoch_state["pending"]) + int(epoch_state["cemented_undecided"])


def check(per_pr):
    """-> (epochs, open per epoch per PR, inconsistent epochs)"""
    epochs = sorted(set().union(*[set(v) for v in per_pr]))
    open_by_epoch = {}
    inconsistent = []
    for epoch in epochs:
        opens = []
        hashes = set()
        for view in per_pr:
            if epoch in view:
                opens.append(open_elections(view[epoch]))
                hashes.add(view[epoch]["hash"])
            else:
                # the PR never took part in this epoch: nothing decided there
                opens.append(0)
                hashes.add(None)
        open_by_epoch[epoch] = opens
        if sum(opens) == 0 and len(hashes) > 1:
            inconsistent.append(epoch)
    return epochs, open_by_epoch, inconsistent


def summarize(view, epoch):
    if epoch not in view:
        return "-"
    e = view[epoch]
    return (f"{e['hash'][:8]} fin={e['finalized']} single={e['single_notarized']} "
            f"pend={e['pending']} cem_und={e['cemented_undecided']} "
            f"empty={e['empty']} confl={e['conflicting']}")


def diff_entries(epoch):
    """The entries of the epoch's state that are not on every PR"""
    views = []
    for port in PORTS:
        state = rpc(port, {"action": "final_state", "epoch": str(epoch)})
        views.append({(e["account"], e["height"], e["hash"]): e["kind"] for e in state.get("entries", [])})
    everywhere = set.intersection(*[set(v) for v in views])
    shown = 0
    for i, view in enumerate(views):
        for key in sorted(set(view) - everywhere):
            others = [v.get(key, "-") for v in views]
            print(f"  epoch {epoch} entry {key[0][-8:]}#{key[1]} {key[2][:8]}: " + " | ".join(others))
            shown += 1
            if shown >= 30:
                return
    print(f"  epoch {epoch}: {len(everywhere)} entries on every PR, sizes {[len(v) for v in views]}")


def diagnose(epoch):
    """Per (root, epoch) certificate sets on every PR, printed where they differ"""
    views = []
    for port in PORTS:
        active = rpc(port, {"action": "confirmation_active"})
        keys = list(zip(active.get("confirmations", []), [int(e) for e in active.get("epochs", [])]))
        view = {}
        for root, e in keys:
            if e != epoch:
                continue
            info = rpc(port, {"action": "confirmation_info", "root": root, "epoch": str(e),
                              "representatives": "true", "contents": "false"})
            certs = info.get("certificates") or {}
            reps = tuple(sorted(
                (h[:8], tuple(sorted((a[-6:], k) for a, k in (b.get("representatives_kudzu") or {}).items())))
                for h, b in info.get("blocks", {}).items()))
            view[root] = (info.get("state"), tuple(sorted(certs.get("notarized", []))),
                          bool(certs.get("timeout")), tuple(sorted(info.get("blocks", {}).keys())), reps)
        views.append(view)
    roots = set().union(*[set(v) for v in views])
    # open instances first, then settled ones whose candidate sets differ
    def is_open(root):
        return any(o is not None and o[0] not in ("settled",) for o in (v.get(root) for v in views))
    shown = 0
    for root in sorted(roots, key=lambda r: (not is_open(r), r)):
        outcomes = [v.get(root) for v in views]
        if len(set(outcomes)) > 1:
            print(f"  epoch {epoch} root {root[:16]}: " +
                  " | ".join("none" if o is None else f"{o[0]} notar={[h[:8] for h in o[1]]} timeout={o[2]} cands={[h[:8] for h in o[3]]}" for o in outcomes))
            # the votes behind the differing views: every PR's for an open
            # instance, one PR per distinct view for settled ones
            seen = set()
            for i, o in enumerate(outcomes):
                if o is None:
                    continue
                key = (o[0], o[1], o[2], o[3])
                if is_open(root) or key not in seen:
                    seen.add(key)
                    print(f"      PR{i} votes: {o[4]}")
            shown += 1
            if shown >= 20:
                break
    print(f"  epoch {epoch}: {len(roots)} roots with an election on some PR, {shown} shown")


start = time.time()
while True:
    per_pr, counts = snapshot()
    epochs, open_by_epoch, inconsistent = check(per_pr)
    elapsed = time.time() - start
    cemented = [c.get("cemented") for c in counts]
    total_open = sum(sum(v) for v in open_by_epoch.values())
    print(f"t={elapsed:.0f}s epochs={epochs} open={ {e: v for e, v in open_by_epoch.items()} } inconsistent={inconsistent} cemented={cemented}", flush=True)
    for epoch in epochs:
        print(f"  epoch {epoch}: " + " | ".join(summarize(view, epoch) for view in per_pr), flush=True)
    if total_open == 0:
        if inconsistent:
            print("INCONSISTENT")
            for epoch in inconsistent:
                diff_entries(epoch)
                diagnose(epoch)
            sys.exit(2)
        print(f"SETTLED_CONSISTENT after {elapsed:.0f}s: epochs {epochs} identical on all PRs, cemented={cemented}")
        sys.exit(0)
    if elapsed > DEADLINE:
        print("TIMEOUT")
        for epoch in epochs:
            if sum(open_by_epoch[epoch]) > 0:
                diagnose(epoch)
        sys.exit(1)
    time.sleep(POLL)
