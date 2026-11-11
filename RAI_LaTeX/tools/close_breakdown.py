#!/usr/bin/env python3
"""Where a RAI close spends its time, from the node diagnostics of the
six-region matrix.

  close_breakdown.py <out file> <matrix dir>...
  close_breakdown.py --runs <out file> <run dir>...   (one row per run)

Each run comes from the first matrix that has it, so reruns go first (as in
wan_figures.py). For every close of every validator of every RAI run of the
six static scenarios it takes, from that validator's own clock:

  freeze     epoch ended -> own report frozen and signed
  reconcile  report frozen -> reports of N - f weight usable here
  check      evidence and BuildState time of every derivation or proposal
             check of the epoch (summed)
  agree      usable reports -> checkpoint decided here (close rounds)
  install    decided -> installed (recheck, gate, slots, committee)
  total      epoch ended -> installed

and the sizes: coded symbols reconciliation needed, report entries
(certified set, residual set), close rounds, and manifest entries when the
validator fetched a proposer's manifest. Writes one row per scenario with
the median and the maximum over all closes of all validators and runs.
"""
import argparse
import re
import statistics
from collections import defaultdict
from pathlib import Path

RATE = 2000
SCENARIOS = ['nofork', 'fork5-grace', 'fork10-grace',
             'nofork-byznode', 'fork5-grace-byznode', 'fork10-grace-byznode']
REPS = 3
FIELD = re.compile(r'(\w+)=(\S+)')


def run_dir(matrices, cell, rep):
    name = 'rai' if rep == 1 else f'rai-rep{rep}'
    for matrix in matrices:
        path = matrix / cell / f'r{RATE}' / name
        if any(path.glob('pr*/diagnostics.log')):
            return path
    raise FileNotFoundError(f'{cell}/{name}')


def closes_of(log):
    """Per epoch, the times and sizes this validator logged"""
    epochs = defaultdict(lambda: {'check': 0, 'symbols': 0, 'rounds': None})
    for line in log.read_text(errors='replace').splitlines():
        kind, _, rest = line.partition(' ')
        fields = dict(FIELD.findall(rest))
        if 'epoch' not in fields or not fields['epoch'].isdigit():
            continue
        e = epochs[int(fields['epoch'])]
        t = int(fields.get('t', 0))
        if kind == 'EPOCH_ENDED':
            e.setdefault('ended', t)
        elif kind == 'EPOCH_REPORT':
            e.setdefault('frozen', t)
            e['certified'] = int(fields['certified'])
            e['residual'] = int(fields['residual'])
        elif kind == 'EPOCH_CLOSE_READY':
            e.setdefault('ready', t)
        elif kind == 'EPOCH_VALIDATE_TIMING':
            e['check'] += int(fields['evidence_ms']) + int(fields['build_ms'])
        elif kind == 'EPOCH_CLOSED':
            if 'closed' not in e:
                e['closed'] = t
                e['rounds'] = int(fields['round']) + 1
        elif kind == 'EPOCH_INSTALL_TIMING':
            steps = re.findall(r'\("(\w+)", (\d+)\)', line)
            if steps:
                e['install'] = int(steps[-1][1])
        elif kind == 'EPOCH_RECONCILED':
            e['symbols'] += int(fields.get('symbols', 0))
        elif kind == 'EPOCH_MANIFEST_FETCHED':
            e['manifest'] = int(fields['entries'])
    return epochs


def measures(epochs):
    for epoch, e in sorted(epochs.items()):
        # Epoch 0 ends before the load starts
        if epoch == 0 or not all(k in e for k in ('ended', 'frozen', 'ready', 'closed')):
            continue
        row = {
            'freeze': e['frozen'] - e['ended'],
            'reconcile': max(0, e['ready'] - e['frozen']),
            'check': e['check'],
            'agree': max(0, e['closed'] - e['ready']),
            'install': e.get('install', 0),
            'rounds': e['rounds'],
            'symbols': e['symbols'],
            'certified': e.get('certified', 0),
            'residual': e.get('residual', 0),
        }
        row['total'] = e['closed'] - e['ended'] + row['install']
        if 'manifest' in e:
            row['manifest'] = e['manifest']
        yield row


COLUMNS = ['freeze', 'reconcile', 'check', 'agree', 'install', 'total',
           'rounds', 'symbols', 'certified', 'residual', 'manifest']


def summary(name, rows):
    values = []
    for column in COLUMNS:
        series = [row[column] for row in rows if column in row]
        if series:
            values += [f'{statistics.median(series):g}', f'{max(series):g}']
        else:
            values += ['-', '-']
    return f'{name} {len(rows)} ' + ' '.join(values)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--runs', action='store_true',
                        help='the directories are single runs, one row each')
    parser.add_argument('out', type=Path)
    parser.add_argument('dirs', type=Path, nargs='+')
    args = parser.parse_args()
    lines = ['# scenario closes ' + ' '.join(f'{c}_med {c}_max' for c in COLUMNS)]
    if args.runs:
        for run in args.dirs:
            rows = []
            for log in sorted(run.glob('pr*/diagnostics.log')):
                rows.extend(measures(closes_of(log)))
            lines.append(summary(run.name, rows))
    for cell in [] if args.runs else SCENARIOS:
        rows = []
        for rep in range(1, REPS + 1):
            for log in sorted(run_dir(args.dirs, cell, rep).glob('pr*/diagnostics.log')):
                rows.extend(measures(closes_of(log)))
        lines.append(summary(cell, rows))
    args.out.write_text('\n'.join(lines) + '\n')
    print('\n'.join(lines))


if __name__ == '__main__':
    main()
