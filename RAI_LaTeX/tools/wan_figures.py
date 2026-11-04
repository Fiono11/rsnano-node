#!/usr/bin/env python3
"""Data for the evaluation's figures from the six-region matrix
(tools/rai/run_sweep.py --set final-wan): every rep of every cell, with
gzipped result files.

  wan_figures.py <matrix dir> <out dir>

Writes to <out dir>:
  bars-<arm>.dat        one row per scenario: x p50 p50lo p50hi p95 p95lo p95hi
      gput gputlo gputhi, the median over the reps and its range
  forks-<scenario>-<arm>.dat  blocks not final after the load (rep 1): t open
  timeline-, closes-, loadend-fork10-byz-<arm>.dat  one run (rep 1), see
      paper_figures.py
  numbers.txt           the per-run values the text of the evaluation quotes
"""
import argparse
import gzip
import json
import statistics
from pathlib import Path

from paper_figures import confirmation_rate, load_end_ms, write_timeline

BLOCKS = 45000
SCENARIOS = [('nofork', 2000), ('fork5-grace', 2000), ('fork10-grace', 2000),
             ('nofork-byznode', 1000), ('fork5-grace-byznode', 1000), ('fork10-grace-byznode', 1000)]
ARMS = ['rai', 'pause', 'develop']
REPS = 3
TIMELINE = ('fork10-grace-byznode', 1000)
FORKS = [('fork10-grace', 2000), ('fork10-grace-byznode', 1000)]


def load(matrix, cell, rate, arm, rep):
    name = arm if rep == 1 else f'{arm}-rep{rep}'
    with gzip.open(matrix / cell / f'r{rate}' / name / 'result.json.gz', 'rt') as f:
        return json.load(f)


def spread(values):
    return statistics.median(values), min(values), max(values)


def resolved_after(result):
    """Seconds from the end of the load to the first check with no block open"""
    end = load_end_ms(result.get('client') or {}, BLOCKS)
    for t, count in result.get('open_trace') or []:
        if end and count == 0:
            return max(0.0, (t - end) / 1000)
    return None


def longest_gap(result):
    """The longest run of load seconds without a non-fork confirmation"""
    series = (result.get('client') or {}).get('nonfork_series') or []
    published = [row[0] for row in series if row[1] >= 100]
    longest = current = 0
    for row in series:
        if published and published[0] < row[0] < published[-1]:
            current = current + 1 if row[2] == 0 else 0
            longest = max(longest, current)
    return longest


def counter(node, detail):
    for entry in node['entries']:
        if entry['type'] == 'active_elections' and entry['detail'] == detail:
            return int(entry['value'])
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('matrix', type=Path)
    parser.add_argument('out', type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    header = 'x p50 p50lo p50hi p95 p95lo p95hi gput gputlo gputhi'
    rows = {arm: [header] for arm in ARMS}
    numbers = []
    rtts, floors = [], []
    for x, (cell, rate) in enumerate(SCENARIOS):
        for arm in ARMS:
            results = [load(args.matrix, cell, rate, arm, rep) for rep in range(1, REPS + 1)]
            clients = [r['client'] for r in results]
            stats = [spread([c[key] for c in clients])
                     for key in ('nonfork_p50_ms', 'nonfork_p95_ms', 'nonfork_goodput_cps')]
            rows[arm].append(' '.join([str(x)] + [str(round(v)) for stat in stats for v in stat]))
            rates = [confirmation_rate(r) for r in results]
            closes = [r.get('close_duration_ms') or {} for r in results]
            settlement = [r.get('settlement') or {} for r in results]
            numbers.append(f'{cell} r{rate} {arm}: p50 {stats[0]} p95 {stats[1]} goodput {stats[2]}')
            numbers.append(f'  status {[r["status"] for r in results]}'
                           f' open {[max(s.get("uncemented") or [0]) for s in settlement]}'
                           f' cemented-values {[len(s.get("cemented") or []) for s in settlement]}'
                           f' held-values {[len(s.get("held") or []) for s in settlement]}'
                           f' resolved-after {[resolved_after(r) for r in results]}')
            numbers.append(f'  load rate {[round(q[0]) for q in rates]} zero-seconds {[q[2] for q in rates]}'
                           f' longest-gap {[longest_gap(r) for r in results]}'
                           f' rate-in-closes {[round(q[3]) if q[3] == q[3] else None for q in rates]}'
                           f' close-median {[c.get("median") for c in closes]} close-max {[c.get("max") for c in closes]}')
            if arm == 'rai':
                fast = [counter(n, 'finalized_fast') for r in results for n in r['counters']]
                final = [counter(n, 'finalized_final') for r in results for n in r['counters']]
                overlap = [counter(n, 'overlap_eligible') for r in results for n in r['counters']]
                share = [a / (a + b) for a, b in zip(fast, final) if a + b]
                numbers.append(f'  fast share of finalized, any node: {min(share):.2f}-{max(share):.2f};'
                               f' overlap eligible: {min(overlap)}-{max(overlap)}')
            for r in results:
                rtts += [v for row in r.get('rtt_ms') or [] for v in row if v > 0]
                if r.get('min_request_interval_ms'):
                    floors.append(r['min_request_interval_ms'])
    numbers.append(f'round trips {min(rtts):.1f}-{max(rtts):.1f} ms, request floor {min(floors)}-{max(floors)} ms')
    for arm, lines in rows.items():
        (args.out / f'bars-{arm}.dat').write_text('\n'.join(lines) + '\n')
    for cell, rate in FORKS:
        for arm in ARMS:
            result = load(args.matrix, cell, rate, arm, 1)
            end = load_end_ms(result['client'], BLOCKS)
            lines = ['t open'] + [f'{(t - end) / 1000:.1f} {count}' for t, count in result['open_trace']]
            (args.out / f'forks-{cell}-{arm}.dat').write_text('\n'.join(lines) + '\n')
    for arm in ('rai', 'pause'):
        write_timeline(args.out, f'fork10-byz-{arm}', load(args.matrix, *TIMELINE, arm, 1), BLOCKS)
    (args.out / 'numbers.txt').write_text('\n'.join(numbers) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
