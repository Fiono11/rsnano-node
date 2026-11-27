"""Summarises every run of the backpressure A/B (and, with a second argument, the matrix's
runs of the same cell): goodput, latency, slowest close, and the storm counters."""
import glob
import gzip
import json
import os
import sys
from collections import defaultdict


def load(path):
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path) as f:
        return json.load(f)


def row(path):
    d = load(path)
    c = d.get("client") or {}
    tot = defaultdict(int)
    nodes_dropping = 0
    for node in d.get("counters") or []:
        dropped = 0
        for e in node.get("entries", []):
            try:
                v = int(e["value"])
            except ValueError:
                continue
            tot[(e["type"], e["detail"], e["dir"])] += v
            if e["type"] == "vote_processor" and e["detail"] == "overfill":
                dropped += v
        nodes_dropping += dropped > 0
    cd = d.get("close_duration_ms") or {}
    close_max = (cd.get("max") or 0) / 1000 if isinstance(cd, dict) else 0
    over = tot[("message_processor_overfill", "report", "in")]
    proc = tot[("message_processor_type", "report", "in")]
    return [
        os.path.basename(os.path.dirname(path)),
        d.get("status"),
        c.get("nonfork_goodput") or d.get("confirmation_rate_cps"),
        c.get("nonfork_p50_ms"),
        c.get("nonfork_p95_ms"),
        f"{close_max:.0f}",
        d.get("settled_after_s"),
        f"{tot[('message', 'confirm_ack', 'out')] / 1e6:.2f}",
        f"{tot[('drop', 'confirm_ack', 'out')] / 1e6:.1f}",
        nodes_dropping,
        f"{100 * over / max(1, over + proc):.0f}%",
        tot[("election", "solicit_rate_cut", "in")],
    ]


header = ["run", "status", "goodput", "p50", "p95", "close_max_s", "settled_s",
          "acks_out_M", "ack_drops_M", "nodes_dropping_votes", "report_in_drop", "rate_cuts"]
print("\t".join(header))
for root in sys.argv[1:]:
    for path in sorted(glob.glob(os.path.join(root, "fork5-grace-reconf/r2000/*/result.json*"))):
        if "/develop" in path or "/pause" in path:
            continue
        print("\t".join(str(x) for x in row(path)))
