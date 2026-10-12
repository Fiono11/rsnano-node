#!/usr/bin/env python3
"""Data for the throughput-latency figures from a sweep (tools/rai/run_sweep.py).

  sweep_figures.py <sweep dir> <out dir>

Writes <out dir>/sweep-<variant>-<arm>.dat, one row per offered rate:
  rate throughput goodput p50 p95 p99 close_p50 close_p95 open_p50 open_p95 close_share open_forks close_median close_max settled
where throughput is the sustained rate: 99 % of the non-fork blocks divided
by the time from the first publication until they were confirmed (the last
1 % is the latency tail, in p99); goodput divides all of them by the time to
the last confirmation. close_* are the non-fork latencies of blocks published while a close
was in progress (from the first node ending epoch e to the last node closing
it), open_* those of the other blocks, close_share the share of blocks
published during closes, open_forks the positions left unresolved (blocks not
cemented at PR0 when the run ended) and close_median/close_max the close
durations in seconds. With repetitions (<arm>-rep<k>) each value is the
median over the runs, and <column>_lo/<column>_hi (for throughput, p50, p95,
p99, close_p50, close_p95) give the min and max. Missing values are nan.
Also writes sweep-numbers.tex
with \\sweepnum{<variant>-<arm>-<rate>-<key>} macros.
"""
import json
import math
import sys
from pathlib import Path


def quantile(histogram, q):
    total = sum(count for _, count in histogram)
    if total == 0:
        return None
    target = q * total
    seen = 0
    for ms, count in sorted(histogram):
        seen += count
        if seen >= target:
            return ms
    return None


def close_windows(result):
    """(start, end) in seconds since the client's series start"""
    start = (result.get('client') or {}).get('series_start_unix_ms')
    events = result.get('epoch_events') or []
    if not start or not events:
        return []
    ended, closed = {}, {}
    for event in events:
        epoch, t = event['epoch'], event['t_unix_ms']
        if event['kind'] == 'ended':
            ended[epoch] = min(ended.get(epoch, t), t)
        elif event['kind'] == 'closed':
            closed[epoch] = max(closed.get(epoch, t), t)
    return [((ended[e] - start) / 1000, (closed[e] - start) / 1000) for e in ended if e in closed]


def split_latencies(result):
    windows = close_windows(result)
    inside, outside = [], []
    for second, buckets in (result.get('client') or {}).get('nonfork_by_publish') or []:
        middle = second + 0.5
        target = inside if any(a <= middle <= b for a, b in windows) else outside
        target.extend((ms, count) for ms, count in buckets)
    return inside, outside


def sustained_throughput(client, share=0.99):
    series = client.get('nonfork_series') or []
    total = client.get('nonfork_created') or 0
    confirmed = 0
    for second, _, count, *_ in series:
        confirmed += count
        if total and confirmed >= share * total:
            return round(share * total / (second + 1), 1)
    return None


def row(run):
    result_path = run / 'result.json'
    if not result_path.exists():
        return None
    result = json.loads(result_path.read_text())
    client = result.get('client') or {}
    inside, outside = split_latencies(result)
    total = sum(c for _, c in inside) + sum(c for _, c in outside)
    share = 100.0 * sum(c for _, c in inside) / total if total else None
    settlement = result.get('settlement') or {}
    uncemented = settlement.get('uncemented') or [None]
    closes = result.get('close_duration_ms') or {}
    return dict(throughput=sustained_throughput(client),
                goodput=client.get('nonfork_goodput_cps'), p50=client.get('nonfork_p50_ms'),
                p95=client.get('nonfork_p95_ms'), p99=client.get('nonfork_p99_ms'),
                close_p50=quantile(inside, .5), close_p95=quantile(inside, .95),
                open_p50=quantile(outside, .5), open_p95=quantile(outside, .95),
                close_share=share, open_forks=uncemented[0],
                close_median=closes.get('median') and closes['median'] / 1000,
                close_max=closes.get('max') and closes['max'] / 1000,
                settled=int(result.get('status') == 'settled'))


SPREAD = ['throughput', 'p50', 'p95', 'p99', 'close_p50', 'close_p95']
COLUMNS = ['throughput', 'goodput', 'p50', 'p95', 'p99', 'close_p50', 'close_p95', 'open_p50', 'open_p95', 'close_share',
           'open_forks', 'close_median', 'close_max', 'settled']


def present(values):
    return [v for v in values if v is not None and not (isinstance(v, float) and math.isnan(v))]


def median(values):
    values = sorted(present(values))
    if not values:
        return None
    middle = len(values) // 2
    return values[middle] if len(values) % 2 else (values[middle - 1] + values[middle]) / 2


def low(values):
    values = present(values)
    return min(values) if values else None


def high(values):
    values = present(values)
    return max(values) if values else None


def fmt(value):
    if value is None or (isinstance(value, float) and math.isnan(value)):
        return 'nan'
    return f'{value:.4g}' if isinstance(value, float) else str(value)


def main():
    sweep, out = Path(sys.argv[1]), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    numbers = []
    for variant_dir in sorted(p for p in sweep.iterdir() if p.is_dir()):
        arms = sorted({run.name.split('-rep')[0] for rate_dir in variant_dir.iterdir() if rate_dir.is_dir()
                       for run in rate_dir.iterdir() if run.is_dir() and '.' not in run.name})
        for arm in arms:
            lines = ['rate n ' + ' '.join(COLUMNS) + ' ' + ' '.join(f'{c}_lo {c}_hi' for c in SPREAD)]
            for rate_dir in sorted(variant_dir.iterdir(), key=lambda p: int(p.name[1:]) if p.name[1:].isdigit() else 0):
                if not rate_dir.is_dir() or not rate_dir.name[1:].isdigit():
                    continue
                runs = [rate_dir / arm] + sorted(rate_dir.glob(f'{arm}-rep*'))
                rows = [r for r in (row(run) for run in runs if run.is_dir() and '.' not in run.name) if r]
                if not rows:
                    continue
                values = {c: median([r[c] for r in rows]) for c in COLUMNS}
                spread = {c: (low([r[c] for r in rows]), high([r[c] for r in rows])) for c in SPREAD}
                rate = int(rate_dir.name[1:])
                lines.append(f'{rate} {len(rows)} ' + ' '.join(fmt(values[c]) for c in COLUMNS) + ' '
                             + ' '.join(f'{fmt(spread[c][0])} {fmt(spread[c][1])}' for c in SPREAD))
                for key in COLUMNS:
                    numbers.append(f'\\expandafter\\def\\csname sweep@{variant_dir.name}-{arm}-{rate}-{key}\\endcsname{{{fmt(values[key])}}}')
            (out / f'sweep-{variant_dir.name}-{arm}.dat').write_text('\n'.join(lines) + '\n')
    numbers.append('\\newcommand{\\sweepnum}[1]{\\csname sweep@#1\\endcsname}')
    (out / 'sweep-numbers.tex').write_text('\n'.join(numbers) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
