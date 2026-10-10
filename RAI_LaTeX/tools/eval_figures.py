#!/usr/bin/env python3
"""Turn a run_eval_matrix.py directory into pgfplots data files and a macro
file for the paper.

  tools/eval_figures.py <matrix dir> <out dir> [--delay <delay matrix dir>]

Writes <out dir>/*.dat (one row per variant and arm with medians and spreads,
time series of one representative run per arm and variant) and
<out dir>/numbers.tex (\\evalnum{key} macros)."""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import statistics

VARIANTS = ['nofork', 'nofork-offline1', 'nofork-byz1', 'fork5', 'fork5-offline1', 'fork5-byz1',
            'fork10', 'fork10-offline1', 'fork10-byz1']
LABELS = {'nofork': 'no forks', 'nofork-offline1': 'no forks, 1 off', 'nofork-byz1': 'no forks, 1 Byz',
          'fork5': '5\\% forks', 'fork5-offline1': '5\\%, 1 off', 'fork5-byz1': '5\\%, 1 Byz',
          'fork10': '10\\% forks', 'fork10-offline1': '10\\%, 1 off', 'fork10-byz1': '10\\%, 1 Byz'}
ARMS = ['rai', 'pause', 'nano']


def load_runs(matrix):
    runs = {}
    for summary in sorted(Path(matrix).glob('*/*.summary.json')):
        variant = summary.parent.name
        arm, _, rep = summary.stem.replace('.summary', '').rpartition('-r')
        try:
            d = json.loads(summary.read_text())
        except json.JSONDecodeError:
            continue
        result = summary.with_suffix('').with_suffix('') / 'result.json'
        result = summary.parent / summary.name.replace('.summary.json', '') / 'result.json'
        full = json.loads(result.read_text()) if result.exists() else d
        runs.setdefault(variant, {}).setdefault(arm, []).append(dict(summary=d, full=full, rep=int(rep),
                                                                    dir=summary.parent / summary.name.replace('.summary.json', '')))
    return runs


def spread(values):
    values = [v for v in values if v is not None]
    if not values:
        return None
    return dict(median=statistics.median(values), lo=min(values), hi=max(values), n=len(values))


def log_series(run_dir):
    """Per second from the client log: confirmed blocks and mean confirmation
    time of that second, with the unix ms of the line; and when publishing
    started."""
    text = (run_dir / 'run.log').read_text(errors='replace')
    rows, start = [], None
    for line in text.splitlines():
        stamp = line[:27]
        try:
            t = datetime.strptime(stamp[:26], '%Y-%m-%dT%H:%M:%S.%f').replace(tzinfo=timezone.utc).timestamp() * 1000
        except ValueError:
            continue
        if start is None and 'Starting with' in line:
            start = t
        m = re.search(r'Confirmed ([\d,]+) blocks \| ([\d,]+) bps \| ([\d,]+) cps \| avg conf time: ([\d,]+) ms', line)
        if m:
            rows.append((t, int(m[1].replace(',', '')), int(m[2].replace(',', '')), int(m[3].replace(',', '')), int(m[4].replace(',', ''))))
    return start, rows


def write_series(out, name, run):
    start, rows = log_series(run['dir'])
    if start is None and rows:
        start = rows[0][0]
    if not rows:
        return
    with open(out / f'series-{name}.dat', 'w') as f:
        f.write('t confirmed bps cps mean_ms\n')
        for t, confirmed, bps, cps, mean in rows:
            f.write(f'{(t - start) / 1000:.1f} {confirmed} {bps} {cps} {mean}\n')
    client = run['full'].get('client') or {}
    series = client.get('nonfork_series') or []
    s0 = client.get('series_start_unix_ms')
    if series and s0:
        with open(out / f'pseries-{name}.dat', 'w') as f:
            f.write('t published confirmed p50 p95\n')
            for second, published, confirmed, p50, p95 in series:
                f.write(f'{(s0 + second * 1000 - start) / 1000:.1f} {published} {confirmed} {p50 if p50 is not None else "nan"} {p95 if p95 is not None else "nan"}\n')
    events = run['full'].get('epoch_events') or []
    seen = {}
    for e in events:
        key = (e['kind'], e['epoch'])
        if e['kind'] in ('advanced', 'closed') and key not in seen:
            seen[key] = e['t_unix_ms']
    with open(out / f'events-{name}.dat', 'w') as f:
        f.write('kind epoch t\n')
        for (kind, epoch), t in sorted(seen.items(), key=lambda kv: kv[1]):
            f.write(f'{kind} {epoch} {(t - start) / 1000:.1f}\n')
    # vertical marks for pgfplots (empty line=jump): one segment per event
    for kind in ('advanced', 'closed'):
        with open(out / f'marks-{kind}-{name}.dat', 'w') as f:
            f.write('t y\n')
            for (k, epoch), t in sorted(seen.items(), key=lambda kv: kv[1]):
                if k == kind:
                    x = (t - start) / 1000
                    f.write(f'{x:.1f} 0.001\n{x:.1f} 30000\n\n')


def representative(runs):
    """The run with the median non-fork p95, else the longest-lived one."""
    finished = [r for r in runs if (r['summary'].get('client') or {}).get('nonfork_p95_ms') is not None]
    if finished:
        finished.sort(key=lambda r: r['summary']['client']['nonfork_p95_ms'])
        return finished[len(finished) // 2]
    return max(runs, key=lambda r: r['summary'].get('confirmed_primary') or 0)


def summarize(runs, out, suffix=''):
    numbers = {}
    for arm in ARMS:
        with open(out / f'summary-{arm}{suffix}.dat', 'w') as f:
            f.write('x variant n settled goodput goodput_lo goodput_hi p50 p50_lo p50_hi p95 p95_lo p95_hi p99 p99_lo p99_hi close close_lo close_hi open share share_lo share_hi open_lo open_hi client_n forks\n')
            for x, variant in enumerate(VARIANTS):
                rs = runs.get(variant, {}).get(arm, [])
                if not rs:
                    continue
                clients = [r['summary'].get('client') or {} for r in rs]
                settled = sum(r['summary'].get('status') == 'settled' for r in rs)
                g = spread([c.get('nonfork_goodput_cps') for c in clients])
                p50 = spread([c.get('nonfork_p50_ms') for c in clients])
                p95 = spread([c.get('nonfork_p95_ms') for c in clients])
                p99 = spread([c.get('nonfork_p99_ms') for c in clients])
                close = spread([(r['summary'].get('close_duration_ms') or {}).get('max') for r in rs])
                opens = spread([(r['summary'].get('settlement') or {}).get('uncemented', [None])[0] for r in rs])
                blocks = 45000
                share = spread([100.0 * (r['summary'].get('confirmed_primary') or 0) / blocks for r in rs])
                def fmt(sp, scale=1.0, key='median'):
                    return 'nan' if sp is None else f'{sp[key] * scale:.3g}' if isinstance(sp[key], float) else str(round(sp[key] * scale, 3))
                row = [x, variant, len(rs), settled]
                for sp, scale in ((g, 1), (p50, 1), (p95, 1), (p99, 1), (close, 0.001)):
                    row += [fmt(sp, scale), fmt(sp, scale, 'lo'), fmt(sp, scale, 'hi')]
                row += [fmt(opens)]
                row += [fmt(share), fmt(share, 1, 'lo'), fmt(share, 1, 'hi')]
                row += [fmt(opens, 1, 'lo'), fmt(opens, 1, 'hi')]
                client_n = sum(c.get('nonfork_p50_ms') is not None for c in clients)
                forks = spread([c.get('fork_created') for c in clients])
                row += [client_n, fmt(forks)]
                numbers[f'{arm}{suffix}-{variant}-clientn'] = client_n
                if forks is not None:
                    numbers[f'{arm}{suffix}-{variant}-forks-median'] = forks['median']
                f.write(' '.join(str(v) for v in row) + '\n')
                for key, sp in (('goodput', g), ('p50', p50), ('p95', p95), ('p99', p99), ('close', close), ('open', opens), ('share', share)):
                    if sp is not None:
                        for k in ('median', 'lo', 'hi'):
                            numbers[f'{arm}{suffix}-{variant}-{key}-{k}'] = sp[k]
                numbers[f'{arm}{suffix}-{variant}-settled'] = settled
                numbers[f'{arm}{suffix}-{variant}-n'] = len(rs)
                rep = representative(rs)
                write_series(out, f'{variant}-{arm}{suffix}', rep)
                numbers[f'{arm}{suffix}-{variant}-rep'] = rep['rep']
    return numbers


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('matrix', type=Path)
    parser.add_argument('out', type=Path)
    parser.add_argument('--delay', type=Path, default=None)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    numbers = summarize(load_runs(args.matrix), args.out)
    if args.delay:
        numbers.update(summarize(load_runs(args.delay), args.out, suffix='-delay'))
    with open(args.out / 'numbers.tex', 'w') as f:
        f.write('% generated by tools/eval_figures.py\n')
        for key, value in sorted(numbers.items()):
            if isinstance(value, float):
                value = f'{value:.3g}' if value < 10 else f'{value:,.0f}'.replace(',', '{,}')
            f.write('\\expandafter\\def\\csname evalnum@%s\\endcsname{%s}\n' % (key, value))
        f.write('\\newcommand{\\evalnum}[1]{\\csname evalnum@#1\\endcsname}\n')
    print('wrote', args.out)


if __name__ == '__main__':
    main()
