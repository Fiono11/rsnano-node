#!/usr/bin/env python3
"""Where the time of a RAI run goes: the epochs' drains and closes against the
confirmation rate, from a nanospam run log (the nodes' EPOCH_* lines and
nanospam's per-second status lines).

Per epoch: when it ended, when the last PR left it (the drain), how many
rounds its close took and how long until it was agreed, and the spread of
instance counts over the PRs at its end. Then the per-second confirmations
inside the drain windows (an ended epoch some PR has not left yet, when no
new election starts) against those outside, and the busy seconds per epoch.

Usage: drain_check.py <run.log> [<run.log> ...]
"""
import re
import statistics
import sys
from datetime import datetime, timezone

ANSI = re.compile(r"\x1b\[[0-9;]*m")
STATUS = re.compile(
    r"(\d{4}-\d\d-\d\dT[\d:.]+Z).*?Confirmed ([\d,]+) blocks \| ([\d,]+) bps \| ([\d,]+) cps \| avg conf time: (\d+) ms"
)


def marks(text, pattern):
    """epoch -> sorted timestamps (ms) of every PR's line"""
    found = {}
    for m in re.finditer(pattern, text):
        # the nodes write to one pipe: a line torn by another is skipped
        if int(m.group(2)) < 1000:
            found.setdefault(int(m.group(2)), []).append(int(m.group(1)))
    return {epoch: sorted(times) for epoch, times in found.items()}


def analyse(path):
    text = ANSI.sub("", open(path, encoding="utf8", errors="replace").read())
    ended = marks(text, r"EPOCH_ENDED t=(\d+) epoch=(\d+)")
    advanced = marks(text, r"EPOCH_(?:START|ADVANCED) t=(\d+) epoch=(\d+)")
    agreed = marks(text, r"EPOCH_AGREED t=(\d+) epoch=(\d+)")
    rounds = {}
    for m in re.finditer(r"EPOCH_CLOSE_ROUND t=\d+ epoch=(\d+) round=(\d+)", text):
        rounds.setdefault(int(m.group(1)), set()).add(int(m.group(2)))
    instances = {}
    for m in re.finditer(r"EPOCH_ENDED t=\d+ epoch=(\d+) elections=(\d+) active=(\d+)", text):
        instances.setdefault(int(m.group(1)), []).append((int(m.group(2)), int(m.group(3))))
    seconds = []
    for m in STATUS.finditer(text):
        at = datetime.strptime(m.group(1)[:23], "%Y-%m-%dT%H:%M:%S.%f").replace(tzinfo=timezone.utc)
        seconds.append((at.timestamp() * 1000, int(m.group(4).replace(",", "")), int(m.group(5))))
    if not ended or not seconds:
        print(f"{path}: no epochs or no status lines")
        return

    t0 = min(ended[min(ended)])
    print(f"\n{path}")
    print(f"{'epoch':>5} {'ended':>7} {'left':>7} {'drain':>7} {'rounds':>10} {'agreed':>7} {'close':>7}  instances at end, active (min..max over PRs)")
    drains = []
    for epoch in sorted(ended):
        end = ended[epoch][0]
        left = advanced.get(epoch + 1, [None])[-1]
        done = agreed.get(epoch, [None])[-1]
        if left:
            drains.append((end, left))
        counts = instances.get(epoch, [])
        spread = (
            f"{min(c for c, _ in counts)}..{max(c for c, _ in counts)}, "
            f"{min(a for _, a in counts)}..{max(a for _, a in counts)}"
            if counts
            else "-"
        )
        print(
            f"{epoch:>5} {(end - t0) / 1000:>7.1f} "
            f"{((left - t0) / 1000 if left else float('nan')):>7.1f} "
            f"{((left - end) / 1000 if left else float('nan')):>7.1f} "
            f"{str(sorted(rounds.get(epoch, []))):>10} "
            f"{((done - t0) / 1000 if done else float('nan')):>7.1f} "
            f"{((done - end) / 1000 if done else float('nan')):>7.1f}  {spread}"
        )

    inside = [(c, ms) for t, c, ms in seconds if any(a <= t <= b for a, b in drains)]
    outside = [(c, ms) for t, c, ms in seconds if c > 0 and not any(a <= t <= b for a, b in drains)]
    busy = [(c, ms) for _, c, ms in seconds if c >= 1000]
    confirming = [t for t, c, _ in seconds if c > 0]
    print(
        f"confirming window {(max(confirming) - min(confirming)) / 1000:.0f} s, "
        f"drain windows {sum(b - a for a, b in drains) / 1000:.0f} s"
    )
    for label, rows in [("inside a drain", inside), ("outside", outside), ("busy (>= 1000 cps)", busy)]:
        if rows:
            print(
                f"  seconds {label:<20} {len(rows):>4}  mean {sum(c for c, _ in rows) // len(rows):>5} cps  "
                f"median {statistics.median([ms for _, ms in rows]):>6.0f} ms  max {max(ms for _, ms in rows)} ms"
            )

    print(f"{'epoch':>5} {'busy s':>6} {'mean cps':>9} {'median ms':>10} {'max ms':>7}")
    bounds = sorted((times[0], epoch) for epoch, times in advanced.items())
    per_epoch = {}
    for t, c, ms in seconds:
        if c < 1000:
            continue
        current = None
        for start, epoch in bounds:
            if t >= start:
                current = epoch
        if current is not None:
            per_epoch.setdefault(current, []).append((c, ms))
    for epoch, rows in sorted(per_epoch.items()):
        print(
            f"{epoch:>5} {len(rows):>6} {sum(c for c, _ in rows) // len(rows):>9} "
            f"{statistics.median([ms for _, ms in rows]):>10.0f} {max(ms for _, ms in rows):>7}"
        )


for path in sys.argv[1:]:
    analyse(path)
