import json, sys, urllib.request
out = sys.argv[1]
def rpc(port, body):
    req = urllib.request.Request(f"http://[::1]:{port}/", data=json.dumps(body).encode(), headers={"content-type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=120))
result = {}
for pr in range(6):
    port = 17076 + 10 * pr
    events = []; offset = 0; active = None
    while True:
        page = rpc(port, {"action": "confirmation_active", "termination_audit_offset": str(offset)})["termination_audit"]
        if offset == 0:
            active = page.get("active")
        batch = page["events"]
        events.extend(batch); offset += len(batch)
        if offset >= int(page["total"]) or not batch:
            break
    try:
        stats = rpc(port, {"action": "stats", "type": "counters"})
    except Exception as e:
        stats = {"error": str(e)}
    result[pr] = {"events": events, "active": active, "overflow": page.get("overflow"), "enabled": page.get("enabled"), "stats": stats}
    print("pr", pr, "events", len(events), "active", len(active or []), "overflow", page.get("overflow"), file=sys.stderr)
json.dump(result, open(out, "w"))
