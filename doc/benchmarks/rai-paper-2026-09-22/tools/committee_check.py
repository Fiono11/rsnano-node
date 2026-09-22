#!/usr/bin/env python3
"""RAI committees: every PR derived the same committee from every epoch.

`final_state` lists, per PR, the committees it knows: the genesis committee of the setup and
the one each closed epoch derived from the state it finalized, each with an order-independent
digest of its weights, n (the weight of all members) and the members' weights.

Checks, over all PRs:
  * the genesis committee has the same digest everywhere;
  * every epoch derived on any PR has the same digest on every PR which derived it, and every
    PR derived every epoch closed at least two epochs ago (the epochs after need it);
n is reported per committee: every account delegates to a PR, but funds sent and not yet
received belong to no account, so n is the ledger's balance less what is in flight.
Prints one line per committee with the shares of the members, then COMMITTEES_CONSISTENT or
COMMITTEES_INCONSISTENT / COMMITTEES_MISSING.
"""
import json
import sys
import urllib.request

# The nodes running: PRS in the environment, six by default
PORTS = [17076 + 10 * i for i in range(int(__import__("os").environ.get("PRS", "6")))]


def rpc(port, body):
    req = urllib.request.Request(
        f"http://[::1]:{port}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    return json.loads(urllib.request.urlopen(req, timeout=10).read())


def main():
    per_pr = []
    current = []
    for port in PORTS:
        state = rpc(port, {"action": "final_state"})
        per_pr.append({c["derived_by"]: c for c in state.get("committees", [])})
        current.append(int(state["current_epoch"]))
    keys = sorted(set().union(*[set(v) for v in per_pr]), key=lambda k: -1 if k == "genesis" else int(k))
    inconsistent = []
    missing = []
    n_values = set()
    for key in keys:
        digests = {}
        for i, view in enumerate(per_pr):
            if key in view:
                digests.setdefault(view[key]["digest"], []).append(i)
                n_values.add(int(view[key]["online"]))
            else:
                # a PR needs the committee of epoch e from epoch e+2 on
                needed = key == "genesis" or current[i] >= int(key) + 2
                if needed:
                    missing.append((key, i))
        sample = next(v[key] for v in per_pr if key in v)
        shares = " ".join(
            f"{m['representative'][-8:]}={100 * int(m['weight']) / max(int(sample['online']), 1):.1f}%"
            for m in sample["weights"]
        )
        holders = ",".join(f"PR{i}" for i, v in enumerate(per_pr) if key in v)
        status = "ok" if len(digests) == 1 else f"DIFFERS {[(d[:8], prs) for d, prs in digests.items()]}"
        print(f"committee {key:>7}: members={sample['members']} n={int(sample['online']):.3e} "
              f"digest={sample['digest'][:8]} on {holders} {status}\n    {shares}")
        if len(digests) > 1:
            inconsistent.append(key)
    print(f"current epochs per PR: {current}")
    if len(n_values) > 1:
        low, high = min(n_values), max(n_values)
        print(f"n ranges over {low:.4e}..{high:.4e}: {100 * (high - low) / high:.2f}% in flight at most")
    if inconsistent:
        print(f"COMMITTEES_INCONSISTENT {inconsistent}")
        return 1
    if missing:
        print(f"COMMITTEES_MISSING {missing}")
        return 2
    print("COMMITTEES_CONSISTENT")
    return 0


if __name__ == "__main__":
    sys.exit(main())
