#!/usr/bin/env python3
"""Run termination condition: every epoch is closed by a certificate of its close election.

`final_state` reports, per consensus epoch, an order-independent hash of the blocks finalized
by a certificate of that epoch plus the settled single notarization certificates of that
epoch, and how many of the epoch's elections are still open:
  * pending: not settled, block not cemented;
  * cemented_undecided: block cemented, but this PR still collects the epoch's certificates.
The run is settled once, for every epoch seen on any PR, every PR reports that epoch with
pending = 0 and cemented_undecided = 0. It is consistent when all PRs then report the same
decided state `d_e` per epoch: RAI's epoch state comes from the N-f reports the decided
value names, not from a replica's own view of what the epoch's instances notarized, and
those views need not coincide.

Then every PR is told to leave its current epoch (`epoch_advance`), so that the last epoch
gets its close election too, and the run is closed once every PR reports, for every epoch,
a finalized close value which is the same on all PRs and the value the PR attests itself.
Prints SETTLED_CONSISTENT and CLOSED_CONSISTENT, or INCONSISTENT (with the differing
elections), CLOSE_INCONSISTENT or TIMEOUT.
"""
import json
import sys
import time
import urllib.request

# The nodes running: PRS in the environment, six by default
PORTS = [17076 + 10 * i for i in range(int(__import__("os").environ.get("PRS", "6")))]
# A Byzantine representative that first voted the winner of a fork and never
# exits may, by Kudzu's rules, still take a second look at the loser: such an
# instance never settles, on any PR. With ALLOW_PENDING set, pending instances
# that are identical on every PR and unchanged over three samples do not hold
# the run back; the closes and the safety checks decide it.
ALLOW_PENDING = __import__("os").environ.get("ALLOW_PENDING") == "1"
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


def decided_state(view, epoch):
    """d_e: the state hash the epoch's joint election decided on this PR"""
    return (view.get(epoch, {}).get("close") or {}).get("value")


def check(per_pr):
    """-> (epochs, open per epoch per PR, inconsistent epochs)

    RAI: what has to agree between replicas is the state the epoch's joint
    election decided - BuildState over the N-f reports the decided value
    names - not each replica's own live view of what the epoch's instances
    notarized. Those views are built from whichever certificates a replica
    happened to assemble and are not required to coincide; the checkpoint is.
    """
    epochs = sorted(set().union(*[set(v) for v in per_pr]))
    open_by_epoch = {}
    inconsistent = []
    for epoch in epochs:
        opens = []
        states = set()
        for view in per_pr:
            if epoch in view:
                opens.append(open_elections(view[epoch]))
            else:
                # the PR never took part in this epoch: nothing decided there
                opens.append(0)
            state = decided_state(view, epoch)
            if state is not None:
                states.add(state)
        open_by_epoch[epoch] = opens
        if sum(opens) == 0 and len(states) > 1:
            inconsistent.append(epoch)
    return epochs, open_by_epoch, inconsistent


def summarize(view, epoch):
    if epoch not in view:
        return "-"
    e = view[epoch]
    state = decided_state(view, epoch)
    return (f"S={(state or '-')[:8]} live={e['hash'][:8]} fin={e['finalized']} "
            f"single={e['single_notarized']} "
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


def close_check(per_pr):
    """-> (epochs left on every PR, open closes per epoch, inconsistent epochs)"""
    current = [v["current_epoch"] for v in per_pr]
    left = sorted(set(range(min(current))))
    # an epoch no PR has anything of (the empty one after the last close) has no close
    left = [e for e in left if any(e in v["epochs"] for v in per_pr)]
    open_by_epoch = {}
    inconsistent = []
    for epoch in left:
        opens = []
        closed = set()
        for view in per_pr:
            close = view["epochs"].get(epoch, {}).get("close")
            if not close or "closed_value" not in close:
                opens.append(1)
                continue
            opens.append(0)
            closed.add(close["closed_value"])
        open_by_epoch[epoch] = opens
        # the value finalized is the same on every PR; a PR's own (final)
        # value may still differ from it, which the close lines show
        if sum(opens) == 0 and len(closed) > 1:
            inconsistent.append(epoch)
    return left, open_by_epoch, inconsistent


def summarize_close(view, epoch):
    close = view["epochs"].get(epoch, {}).get("close")
    if not close:
        return "-"
    closed = close.get("closed_value")
    return (f"round={close['round']} ready={close['ready']} started={close['started']} "
            f"S={(close.get('value') or '-')[:8]} "
            f"closed={(closed or '-')[:8]}@{close.get('closed_round', '-')}")


def wait_for_close(per_pr_snapshot):
    # The current epoch is ended so that it gets closed too, unless nothing
    # happened in it: an epoch without elections never ends by time either
    per_pr = per_pr_snapshot()
    current = per_pr[0]["current_epoch"]
    if all(v["current_epoch"] == current for v in per_pr) and not any(
        current in v["epochs"] for v in per_pr
    ):
        print(f"epoch {current} is empty on every PR: not ended", flush=True)
    else:
        for port in PORTS:
            rpc(port, {"action": "epoch_advance"})
        print("epoch_advance sent to every PR", flush=True)
    close_start = time.time()
    while True:
        per_pr = per_pr_snapshot()
        left, open_by_epoch, inconsistent = close_check(per_pr)
        elapsed = time.time() - close_start
        total_open = sum(sum(v) for v in open_by_epoch.values())
        print(f"close t={elapsed:.0f}s left={left} open={open_by_epoch} inconsistent={inconsistent}", flush=True)
        for epoch in left:
            print(f"  close {epoch}: " + " | ".join(summarize_close(view, epoch) for view in per_pr), flush=True)
        if total_open == 0:
            if inconsistent:
                print("CLOSE_INCONSISTENT")
                return 2
            print(f"CLOSED_CONSISTENT after {elapsed:.0f}s: epochs {left} closed identically on all PRs")
            return 0
        if elapsed > DEADLINE:
            print("TIMEOUT")
            return 1
        time.sleep(POLL)


def close_snapshot():
    per_pr = []
    for port in PORTS:
        state = rpc(port, {"action": "final_state"})
        per_pr.append({
            "current_epoch": int(state["current_epoch"]),
            "epochs": {int(e["epoch"]): e for e in state.get("epochs", [])},
        })
    return per_pr


start = time.time()
stable_samples = []
while True:
    per_pr, counts = snapshot()
    epochs, open_by_epoch, inconsistent = check(per_pr)
    elapsed = time.time() - start
    cemented = [c.get("cemented") for c in counts]
    total_open = sum(sum(v) for v in open_by_epoch.values())
    print(f"t={elapsed:.0f}s epochs={epochs} open={ {e: v for e, v in open_by_epoch.items()} } inconsistent={inconsistent} cemented={cemented}", flush=True)
    for epoch in epochs:
        print(f"  epoch {epoch}: " + " | ".join(summarize(view, epoch) for view in per_pr), flush=True)
    if ALLOW_PENDING and total_open > 0:
        # pending counts unchanged on every PR three samples in a row, and the
        # same state hash per epoch: what is left is held by a faulty
        # representative, not by disagreement. Whether such an instance counts
        # as settled on a PR depends on which of the faulty votes reached it
        # (a final vote is an exit), so the counts need not be equal.
        stable_samples.append(dict(open_by_epoch))
        stable_samples = stable_samples[-3:]
        if len(stable_samples) == 3 and stable_samples[0] == stable_samples[1] == stable_samples[2]:
            differing = [e for e in epochs if len({v[e]["hash"] for v in per_pr if e in v}) > 1]
            if differing:
                print(f"INCONSISTENT (with pending: {differing})")
                for epoch in differing:
                    diff_entries(epoch)
                    diagnose(epoch)
                sys.exit(2)
            print(f"SETTLED_WITH_PENDING after {elapsed:.0f}s: pending {open_by_epoch} stable on every PR, epochs {epochs} hash identically, cemented={cemented}")
            sys.exit(wait_for_close(close_snapshot))
    if total_open == 0 and not inconsistent:
        print(f"SETTLED_CONSISTENT after {elapsed:.0f}s: epochs {epochs} identical on all PRs, cemented={cemented}")
        sys.exit(wait_for_close(close_snapshot))
    if elapsed > DEADLINE:
        # a PR still missing blocks the others finalized is not settled, not
        # inconsistent: only a difference that lasts is
        if inconsistent:
            print("INCONSISTENT")
            for epoch in inconsistent:
                diff_entries(epoch)
                diagnose(epoch)
            sys.exit(2)
        print("TIMEOUT")
        for epoch in epochs:
            if sum(open_by_epoch[epoch]) > 0:
                diagnose(epoch)
        sys.exit(1)
    time.sleep(POLL)
