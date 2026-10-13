#!/usr/bin/env python3
"""Data for the evaluation's three figures from the 'paper' sweep set
(tools/rai/run_sweep.py --set paper): one load, three systems, four
scenarios, one run each.

  paper_figures.py <sweep dir> <out dir> [--rate 1000]

Writes to <out dir>:
  timeline-<scenario>-<arm>.dat  per second since the first publication:
      t confirmed p95 (non-fork blocks confirmed in that second, p95 ms of
      the blocks published in it)
  closes-<scenario>-<arm>.dat    the close windows as a band to fill:
      t y, y 1 inside a close (first validator leaving the epoch to the
      last one seeing its checkpoint decided) and 0 outside
  latency-<arm>.dat              one row per scenario: x p50 p95 (ms)
  forks-<arm>.dat                blocks not cemented over time, from the end
      of the load: t open (the most on any validator)
  paper-numbers.tex              \\papernum{<scenario>-<arm>-<key>} macros
"""
import argparse
import json
import math
from pathlib import Path

from sweep_figures import close_windows, fmt, quantile, split_latencies, sustained_throughput

SCENARIOS = ['nofork', 'fork5-grace', 'fork10-grace', 'fork5-byzlead-grace']
ARMS = ['rai', 'pause', 'develop']


def load(run):
    path = run / 'result.json'
    return json.loads(path.read_text()) if path.exists() else None


def load_end_ms(client, blocks):
    """Unix ms of the end of the second in which the client's publications
    reached the run's block count (a few republications may follow)"""
    start = client.get('series_start_unix_ms')
    series = client.get('nonfork_series') or []
    target = min(blocks, sum(row[1] for row in series))
    published = 0
    for second, count, *_ in series:
        published += count
        if start and target and published >= target:
            return start + (second + 1) * 1000
    return None


def write_timeline(out, name, result):
    client = result.get('client') or {}
    lines = ['t confirmed p95']
    for second, _, confirmed, _, p95 in client.get('nonfork_series') or []:
        lines.append(f'{second} {confirmed} {fmt(p95)}')
    (out / f'timeline-{name}.dat').write_text('\n'.join(lines) + '\n')
    band = ['t y', '0 0']
    for begin, end in sorted(close_windows(result)):
        band += [f'{begin:.2f} 0', f'{begin:.2f} 1', f'{end:.2f} 1', f'{end:.2f} 0']
    (out / f'closes-{name}.dat').write_text('\n'.join(band) + '\n')


def write_forks(out, arm, result, blocks):
    end = load_end_ms(result.get('client') or {}, blocks)
    trace = result.get('open_trace') or []
    if end is None or not trace:
        return None
    lines = ['t open'] + [f'{(t - end) / 1000:.1f} {count}' for t, count in trace]
    (out / f'forks-{arm}.dat').write_text('\n'.join(lines) + '\n')
    return trace[-1][1]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('sweep', type=Path)
    parser.add_argument('out', type=Path)
    parser.add_argument('--rate', type=int, default=1000)
    parser.add_argument('--blocks', type=int, default=45000)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    numbers = []
    latency = {arm: ['x p50 p95'] for arm in ARMS}

    def number(key, value):
        numbers.append(f'\\expandafter\\def\\csname paper@{key}\\endcsname{{{fmt(value)}}}')

    for x, scenario in enumerate(SCENARIOS):
        for arm in ARMS:
            result = load(args.sweep / scenario / f'r{args.rate}' / arm)
            if result is None:
                latency[arm].append(f'{x} nan nan')
                continue
            client = result.get('client') or {}
            name = f'{scenario}-{arm}'
            write_timeline(args.out, name, result)
            p50, p95 = client.get('nonfork_p50_ms'), client.get('nonfork_p95_ms')
            latency[arm].append(f'{x} {fmt(p50)} {fmt(p95)}')
            inside, outside = split_latencies(result)
            closes = result.get('close_duration_ms') or {}
            grace = result.get('fork_grace') or {}
            values = dict(p50=p50, p95=p95, p99=client.get('nonfork_p99_ms'),
                          throughput=sustained_throughput(client),
                          close_p95=quantile(inside, .95), open_p95=quantile(outside, .95),
                          close_median=closes.get('median') and closes['median'] / 1000,
                          open_forks=max((result.get('settlement') or {}).get('uncemented') or [math.nan]),
                          grace_outcome=grace.get('outcome'), grace_waited=grace.get('waited_s'))
            if scenario == 'fork10-grace':
                write_forks(args.out, arm, result, args.blocks)
            for key, value in values.items():
                number(f'{scenario}-{arm}-{key}', value)
    for arm, lines in latency.items():
        (args.out / f'latency-{arm}.dat').write_text('\n'.join(lines) + '\n')
    numbers.append('\\newcommand{\\papernum}[1]{\\csname paper@#1\\endcsname}')
    (args.out / 'paper-numbers.tex').write_text('\n'.join(numbers) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
