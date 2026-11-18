#!/usr/bin/env python3
"""Numbers and the supplement table for the twelve-validator matrix across six regions.

  n12_numbers.py <matrix dir> <out dir>

Reads every run of the matrix (result.json.gz; the set-aside/ originals are not read) and writes
<out dir>/numbers.txt (every value the text quotes, per cell and arm: runs, how many passed,
median and range of the non-fork confirmation rate, median latency, 95th percentile, and the
slowest close) and <out dir>/table.tex (the rows of the supplement's table: medians).

A RAI or pause run passes when the harness reports it settled; a Nano run passes when every
validator holds the same blocks and the decided values agree (its forks do not terminate).
"""
import gzip
import json
import statistics
import sys
from pathlib import Path

CELLS = [('nofork', 'No forks'), ('fork5-grace', '5\\% forks'), ('fork10-grace', '10\\% forks'),
         ('nofork-byznode', 'No forks, Byz.'), ('fork5-grace-byznode', '5\\%, Byz.'),
         ('fork10-grace-byznode', '10\\%, Byz.'), ('nofork-reconf', 'No forks, change'),
         ('fork5-grace-reconf', '5\\%, change'), ('fork10-grace-reconf', '10\\%, change'),
         ('nofork-reconf-byznode', 'No forks, change, Byz.'), ('fork5-grace-reconf-byznode', '5\\%, change, Byz.'),
         ('fork10-grace-reconf-byznode', '10\\%, change, Byz.')]
ARMS = [('rai', 'RAI'), ('pause', 'pause'), ('develop', 'Nano')]


def runs(matrix, cell, arm):
    d = matrix / cell / 'r2000'
    paths = sorted(list(d.glob(f'{arm}/result.json.gz')) + list(d.glob(f'{arm}-rep*/result.json.gz')))
    return [json.load(gzip.open(p)) for p in paths]


def row(results, arm):
    rate, p50, p95, close, passed = [], [], [], [], 0
    for r in results:
        c = r.get('client') or {}
        s = r.get('settlement') or {}
        if arm == 'develop':
            passed += bool(s.get('values_agree', True) and len(s.get('held', [])) <= 1)
        else:
            passed += r.get('status') == 'settled'
        if c.get('nonfork_goodput_cps') is not None:
            rate.append(c['nonfork_goodput_cps']); p50.append(c['nonfork_p50_ms']); p95.append(c['nonfork_p95_ms'])
        closes = [v.get('last_close_ms') or 0 for v in (r.get('close_timings') or {}).values()]
        if closes and arm != 'develop':
            close.append(max(closes) / 1000)
    return dict(runs=len(results), passed=passed, no_client=len(results) - len(rate),
                rate=rate, p50=p50, p95=p95, close=close)


def stat(xs):
    if not xs:
        return None
    return dict(median=statistics.median(xs), min=min(xs), max=max(xs))


def ms(v):
    return f'{v / 1000:.1f}\\,s' if v >= 10000 else f'{v:,.0f}'.replace(',', '{,}')


def cell_text(st, fmt, many):
    if st is None:
        return '--'
    if not many or st['min'] == st['max']:
        return fmt(st['median'])
    return f"{fmt(st['median'])} ({fmt(st['min'])}--{fmt(st['max'])})"


def main():
    matrix, out = Path(sys.argv[1]), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    lines, table = [f'source {matrix}'], []
    for cell, label in CELLS:
        cells = []
        for arm, name in ARMS:
            res = runs(matrix, cell, arm)
            if not res:
                lines.append(f'{cell} {arm}: not run')
                cells.append(None)
                continue
            r = row(res, arm)
            s = {k: stat(r[k]) for k in ('rate', 'p50', 'p95', 'close')}
            lines.append(f"{cell} {arm}: runs {r['runs']} passed {r['passed']} no_client_figures {r['no_client']} "
                         + ' '.join(f"{k} {v['median']:.0f} [{v['min']:.0f}, {v['max']:.0f}]"
                                    if k != 'close' else f"{k}_s {v['median']:.1f} [{v['min']:.1f}, {v['max']:.1f}]"
                                    for k, v in s.items() if v))
            cells.append((r, s))
        if all(c is None for c in cells):
            continue
        parts = [label]
        for (arm, _), c in zip(ARMS, cells):
            if c is None:
                parts += ['--'] * (4 if arm != 'develop' else 3)
                continue
            r, s = c
            many = False  # medians only; numbers.txt keeps the ranges
            parts += [cell_text(s['rate'], lambda v: f'{v:,.0f}'.replace(',', '{,}'), many),
                      cell_text(s['p50'], ms, many), cell_text(s['p95'], ms, many)]
            if arm != 'develop':
                parts.append(cell_text(s['close'], lambda v: f'{v:.0f}', many))
        n = cells[0][0]['runs'] if cells[0] else 0
        table.append(' & '.join(parts) + f' \\\\ % RAI runs: {n}')
    (out / 'numbers.txt').write_text('\n'.join(lines) + '\n')
    (out / 'table.tex').write_text('\n'.join(table) + '\n')
    print('\n'.join(lines))


if __name__ == '__main__':
    main()
