#!/usr/bin/env python3
"""Run termination condition: every PR has settled every election and all PRs report the
same final state hash (`final_state` RPC: per account the settled single notarization
certificate if there is one, else the cemented frontier; timeout certificates and
conflicting notarization certificates contribute nothing).

The hash is compared only once every election on every PR is settled: a single
notarization certificate can still turn into a conflicting pair before that. After
convergence the blocks of conflicting roots are the ones a PR discards; the check asserts
that none of them is cemented on any PR.

Prints CONVERGED, DISCARD_VIOLATION (a discarded block is finalized somewhere) or TIMEOUT.
"""
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


def truthy(v):
    return v is True or v == "true"


def snapshot():
    states = [rpc(port, {"action": "final_state"}) for port in PORTS]
    cemented = [rpc(port, {"action": "block_count"}).get("cemented") for port in PORTS]
    return states, cemented


def discarded_blocks(states):
    """root -> candidate hashes over the conflicting roots of every PR"""
    blocks = {}
    for state in states:
        for conflicting in state.get("conflicting", []):
            blocks.setdefault(conflicting["root"], set()).update(conflicting["blocks"])
    return blocks


def cemented_anywhere(hashes):
    """(hash, pr) pairs for the given hashes that are cemented on some PR"""
    found = []
    hashes = sorted(hashes)
    for i, port in enumerate(PORTS):
        for chunk in range(0, len(hashes), 1000):
            info = rpc(port, {"action": "blocks_info", "include_not_found": "true", "hashes": hashes[chunk:chunk + 1000]})
            for hash, block in info.get("blocks", {}).items():
                if truthy(block.get("confirmed")):
                    found.append((hash, i))
    return found


def check_discard(states):
    blocks = discarded_blocks(states)
    by_hash = {hash: root for root, hashes in blocks.items() for hash in hashes}
    violations = [(by_hash[hash], hash, pr) for hash, pr in cemented_anywhere(by_hash)]
    return blocks, violations


start = time.time()
while True:
    states, cemented = snapshot()
    elapsed = time.time() - start
    hashes = [s["hash"] for s in states]
    settled = [truthy(s["all_settled"]) for s in states]
    summary = [
        f"PR{i}:{s['hash'][:8]} pend={s['pending']} single={s['single_notarized']} conf={len(s['conflicting'])} empty={s['empty']}"
        for i, s in enumerate(states)
    ]
    print(f"t={elapsed:.0f}s settled={sum(settled)}/6 distinct_hashes={len(set(hashes))} cemented={cemented} {' '.join(summary)}", flush=True)

    if all(settled) and len(set(hashes)) == 1:
        blocks, violations = check_discard(states)
        discarded = sum(len(h) for h in blocks.values())
        if violations:
            print(f"DISCARD_VIOLATION after {elapsed:.0f}s: {len(violations)} finalized blocks would be discarded")
            for root, hash, pr in violations[:20]:
                print(f"  root {root[:16]} block {hash[:16]} is cemented on PR{pr}")
            sys.exit(2)
        print(f"CONVERGED after {elapsed:.0f}s: hash {hashes[0]} on all PRs, accounts={states[0]['accounts']}, "
              f"single_notarized={[s['single_notarized'] for s in states]}, conflicting roots={len(blocks)}, "
              f"discarded blocks={discarded}, cemented={cemented}")
        sys.exit(0)

    if elapsed > DEADLINE:
        print("TIMEOUT")
        for i, s in enumerate(states):
            print(f"  PR{i}: {json.dumps({k: v for k, v in s.items() if k != 'conflicting'})} conflicting={len(s['conflicting'])}")
        sys.exit(1)
    time.sleep(POLL)
