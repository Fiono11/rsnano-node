#!/usr/bin/env python3
"""Per-run CSV and per-cell medians of a run_sweep.py matrix whose
result.json files may be gzipped.

  matrix_table.py MATRIX_DIR [--csv OUT.csv]

Prints, per variant, rate and arm: runs, settled runs, median and range of
goodput, p50, p95 and p99, and the median of the longest close."""
import argparse
import csv
import gzip
import json
import statistics
from pathlib import Path


def load(path):
    gz = path.with_name(path.name + '.gz')
    if gz.exists():
        with gzip.open(gz, 'rt') as f:
            return json.load(f)
    return json.loads(path.read_text())


def rows(matrix):
    index = json.loads((matrix / 'index.json').read_text())
    for run in index['runs']:
        name = run['arm'] if run['rep'] == 1 else f"{run['arm']}-rep{run['rep']}"
        result = load(matrix / run['variant'] / f"r{run['rate']}" / name / 'result.json')
        client = result.get('client') or {}
        settlement = result.get('settlement') or {}
        yield dict(variant=run['variant'], rate=run['rate'], arm=run['arm'], rep=run['rep'],
                   status=result.get('status'), correct=result.get('correctness_passed'),
                   same_blocks=len(settlement.get('held') or []) == 1,
                   values_agree=settlement.get('values_agree'),
                   goodput=client.get('nonfork_goodput_cps'), p50=client.get('nonfork_p50_ms'),
                   p95=client.get('nonfork_p95_ms'), p99=client.get('nonfork_p99_ms'),
                   forks_created=client.get('fork_created'), forks_confirmed=client.get('fork_confirmed'),
                   close_max_ms=(result.get('close_duration_ms') or {}).get('max'))


def spread(values):
    values = [v for v in values if v is not None]
    if not values:
        return '-'
    return f'{statistics.median(values):g} ({min(values):g}-{max(values):g})'


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('matrix', type=Path)
    parser.add_argument('--csv', type=Path)
    args = parser.parse_args()
    table = list(rows(args.matrix))
    if args.csv:
        with args.csv.open('w', newline='') as f:
            writer = csv.DictWriter(f, fieldnames=list(table[0]))
            writer.writeheader()
            writer.writerows(table)
    cells = {}
    for row in table:
        cells.setdefault((row['variant'], row['rate'], row['arm']), []).append(row)
    print('| variant | rate | arm | settled | goodput cps | p50 ms | p95 ms | p99 ms | close max ms |')
    print('|---|---|---|---|---|---|---|---|---|')
    for (variant, rate, arm), runs in cells.items():
        settled = sum(r['status'] == 'settled' for r in runs)
        print(f"| {variant} | {rate} | {arm} | {settled}/{len(runs)} | {spread([r['goodput'] for r in runs])} | "
              f"{spread([r['p50'] for r in runs])} | {spread([r['p95'] for r in runs])} | "
              f"{spread([r['p99'] for r in runs])} | {spread([r['close_max_ms'] for r in runs])} |")


if __name__ == '__main__':
    raise SystemExit(main())
