#!/usr/bin/env python3
"""Tables for the paper from paper_eval.py results.

    summarize_eval.py OUT

Per variant and arm: runs completed/settled, non-fork goodput and non-fork
p50/p95/p99 as median [min-max] over runs, and p50/p95/p99 over the pooled
non-fork latency histogram of all completed runs. Writes OUT/summary.json and
OUT/summary.md.
"""
import json
import re
import statistics
import sys
from collections import Counter
from pathlib import Path

from pair_run import quantile

out = Path(sys.argv[1])
VARIANTS = ['nofork', 'nofork-offline1', 'nofork-byz1', 'fork5', 'fork10', 'fork5-offline1', 'fork10-offline1', 'fork5-byz1', 'fork10-byz1']
ARMS = ['develop', 'rai']
rows, md = [], []
md.append('| Variant | Arm | Runs ok/total | Goodput (blocks/s) | p50 (ms) | p95 (ms) | p99 (ms) | Pooled p50/p95/p99 (ms) | Forks unresolved |')
md.append('|---|---|---:|---:|---:|---:|---:|---:|---:|')


def band(values, fmt='{:.0f}'):
    if not values:
        return 'n/a'
    return f"{fmt.format(statistics.median(values))} [{fmt.format(min(values))}-{fmt.format(max(values))}]"


for variant in VARIANTS:
    for arm in ARMS:
        results = [json.loads(p.read_text()) for p in sorted((out / variant).glob(f'pair-*-{arm}/result.json'))]
        if not results:
            continue
        ok = [r for r in results if r.get('complete') and r.get('settled_consistent')]
        pooled = Counter()
        for r in ok:
            pooled.update({int(k): n for k, n in r['metrics']['nonfork_histogram_ms'].items()})
        pooled_q = {p: quantile(pooled, p / 100) if pooled else None for p in (50, 95, 99)}
        row = {'variant': variant, 'arm': arm, 'runs': len(results), 'ok': len(ok),
               'failed': [{'pair': r['pair'], 'complete': r.get('complete'), 'settled': r.get('settled_consistent'),
                           'timed_out': r.get('timed_out')} for r in results if r not in ok],
               'goodput': [r['goodput'] for r in ok],
               **{f'p{p}': [r[f'p{p}_ms'] for r in ok] for p in (50, 95, 99)},
               'pooled': pooled_q,
               'fork_unresolved': [r['metrics'].get('fork_unresolved_at_end') for r in ok]}
        rows.append(row)
        unresolved = [u for u in row['fork_unresolved'] if u is not None]
        md.append(f"| {variant} | {arm} | {len(ok)}/{len(results)} | {band(row['goodput'])} | {band(row['p50'])} | "
                  f"{band(row['p95'])} | {band(row['p99'])} | "
                  f"{pooled_q[50]}/{pooled_q[95]}/{pooled_q[99]} | {band(unresolved) if unresolved else 'n/a'} |")
STATUS = re.compile(r'Confirmed ([\d,]+) blocks \| [\d,]+ bps \| ([\d,]+) cps')


def timeline(run_dir):
    """Per-second (confirmed so far, cps) from the client's status lines"""
    text = (run_dir / 'run.log').read_text(errors='replace')
    return [(int(m.group(1).replace(',', '')), int(m.group(2).replace(',', '')))
            for m in map(STATUS.search, text.splitlines()) if m]


def stall_onset(seconds, floor):
    """First second from which every later second stays below `floor` cps"""
    onset = None
    for i, (_, cps) in enumerate(seconds):
        if cps >= floor:
            onset = None
        elif onset is None:
            onset = i
    return onset


md.append('')
md.append('Runs that did not finish: all confirmed blocks (forks included) out of 45,000 at the timeout,')
md.append('and the second after the first confirmation from which confirmations stayed below 200/s.')
md.append('')
md.append('| Variant | Arm | Pair | Publishing timeout (s) | Confirmed | Share | Stall onset (s) |')
md.append('|---|---|---:|---:|---:|---:|---:|')
unfinished = []
for variant in VARIANTS:
    for arm in ARMS:
        for path in sorted((out / variant).glob(f'pair-*-{arm}/result.json')):
            r = json.loads(path.read_text())
            if r.get('complete'):
                continue
            seconds = timeline(path.parent)
            confirmed = seconds[-1][0] if seconds else 0
            onset = stall_onset(seconds, 200) if seconds else None
            unfinished.append({'variant': variant, 'arm': arm, 'pair': r['pair'],
                               'timeout': r.get('publish_timeout_seconds') or r.get('timeout_seconds'), 'confirmed': confirmed,
                               'stall_onset_s': onset, 'timed_out': r.get('timed_out')})
            md.append(f"| {variant} | {arm} | {r['pair']} | {r.get('publish_timeout_seconds') or r.get('timeout_seconds')} | {confirmed:,} | "
                      f"{confirmed / 45000:.1%} | {onset if onset is not None else 'none'} |")
rows.append({'unfinished': unfinished})
(out / 'summary.json').write_text(json.dumps(rows, indent=2) + '\n')
(out / 'summary.md').write_text('\n'.join(md) + '\n')
print('\n'.join(md))
