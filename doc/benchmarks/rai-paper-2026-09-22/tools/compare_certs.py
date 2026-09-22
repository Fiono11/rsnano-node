import json, sys, urllib.request, collections
def rpc(port, body):
    req = urllib.request.Request(f"http://[::1]:{port}", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    return json.loads(urllib.request.urlopen(req, timeout=30).read())
ports = [17076 + 10*i for i in range(int(__import__("os").environ.get("PRS", "6")))]
per_pr = {}; details = {}
for i, port in enumerate(ports):
    active = rpc(port, {"action": "confirmation_active"})
    roots = active.get("confirmations", [])
    outcome = {}; states = collections.Counter()
    for root in roots:
        info = rpc(port, {"action": "confirmation_info", "root": root, "representatives": "true", "contents": "false"})
        certs = info.get("certificates") or {}
        notarized = tuple(sorted(certs.get("notarized", [])))
        outcome[root] = (notarized, certs.get("timeout"), info.get("state"), len(info.get("blocks", {})))
        states[info.get("state")] += 1
        details[(i, root)] = info
    per_pr[i] = outcome
    print(f"PR{i}: unconfirmed roots={len(roots)} states={dict(states)} with>=1 cert={sum(1 for v in outcome.values() if v[0])} with 2 certs={sum(1 for v in outcome.values() if len(v[0])==2)} no cert={sum(1 for v in outcome.values() if not v[0])} timeout cert={sum(1 for v in outcome.values() if v[1])}")
all_roots = set().union(*[set(o) for o in per_pr.values()])
mismatch_roots = [r for r in all_roots if any(r not in per_pr[i] for i in per_pr)]
mismatch_certs = [r for r in all_roots if r not in mismatch_roots and len({per_pr[i][r][0] for i in per_pr}) > 1]
nocert_everywhere = [r for r in all_roots if r not in mismatch_roots and all(not per_pr[i][r][0] for i in per_pr)]
not_settled=[r for r in all_roots if r not in mismatch_roots and any(per_pr[i][r][2]!="settled" for i in per_pr)]
ledger_blocks={}
for r in all_roots:
    if r in mismatch_roots: continue
    blocks=set()
    for i, port in enumerate(ports):
        info=details[(i,r)]
        for h,b in info["blocks"].items():
            # the block our ledger holds is the one we cast a first vote for: representatives_kudzu of PR i's own rep contains "first"
            pass
    ledger_blocks[r]=blocks
print(f"not settled on some PR={len(not_settled)}")
print(f"roots total={len(all_roots)} missing on some PR={len(mismatch_roots)} certificate sets differ={len(mismatch_certs)} no cert anywhere={len(nocert_everywhere)}")
def show(r):
    for i in per_pr:
        info = details.get((i, r))
        if not info: print(f"   PR{i}: (none)"); continue
        blocks = {h[:8]: (b.get("first_tally"), b.get("tally"), b.get("representatives_kudzu")) for h, b in info["blocks"].items()}
        print(f"   PR{i}: state={info.get('state')} certs={per_pr[i][r][0] and [h[:8] for h in per_pr[i][r][0]]} timeout={per_pr[i][r][1]} blocks={blocks}")
for r in mismatch_certs[:3]:
    print("differ:", r[:16]); show(r)
for r in nocert_everywhere[:3]:
    print("no cert anywhere:", r[:16]); show(r)

for r in mismatch_roots[:5]:
    print("missing on some PR:", r[:16])
    hashes=set()
    for i in per_pr:
        if (i,r) in details: hashes.update(details[(i,r)]["blocks"].keys())
    show(r)
    for i, port in enumerate(ports):
        if (i,r) in details: continue
        for h in hashes:
            try:
                bi=rpc(port, {"action":"block_info","hash":h,"json_block":"true"})
                print(f"   PR{i}: block {h[:8]} in ledger confirmed={bi.get('confirmed')} height={bi.get('height')}")
            except Exception as e:
                print(f"   PR{i}: block {h[:8]} not in ledger ({str(e)[:40]})")
