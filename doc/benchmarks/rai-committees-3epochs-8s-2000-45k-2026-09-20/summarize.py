#!/usr/bin/env python3
"""One line of headline numbers per nanospam run log.

nanospam's own headline first (`Confirming N blocks took Ts` / `Confirmation rate` /
`Average conf time`), which is what the earlier baseline records quote, then the
same statistics over the seconds at >= 1000 cps only, which drop the ramp-up and
the tail. A log without the headline is a run that did not finish publishing.
"""
import re
import statistics
import sys

for name in sys.argv[1:]:
    rows = []
    for line in open(name, "rb").read().decode("utf8", "replace").splitlines():
        m = re.search(
            r"Confirmed ([\d,]+) blocks \| [\d,]+ bps \| ([\d,]+) cps \| avg conf time: (\d+) ms",
            line,
        )
        if m:
            rows.append(tuple(int(g.replace(",", "")) for g in m.groups()))
    text = open(name, "rb").read().decode("utf8", "replace")
    done = re.search(r"Confirming (\d+) blocks took ([\d.]+)s", text)
    rate_own = re.search(r"Confirmation rate: (\d+) cps", text)
    avg_own = re.search(r"Average conf time: (\d+) ms", text)
    head = (
        f"published={done.group(1)} in {done.group(2)}s "
        f"rate={rate_own.group(1)} cps avg={avg_own.group(1)} ms | "
        if done and rate_own and avg_own
        else "DID NOT FINISH | "
    )
    busy = [r for r in rows if r[1] >= 1000]
    if not busy:
        print(f"{name}: no second reached 1000 cps ({len(rows)} status lines)")
        continue
    rate = sum(r[1] for r in busy) / len(busy)
    median = statistics.median(r[2] for r in busy)
    weighted = sum(r[1] * r[2] for r in busy) / sum(r[1] for r in busy)
    spikes = [(i, r[2]) for i, r in enumerate(busy) if r[2] >= 300]
    print(
        f"{name.split('/')[-1]}: {head}busy_s={len(busy)} "
        f"rate={rate:.0f} cps median={median:.0f} ms weighted={weighted:.0f} ms spikes={spikes}"
    )
