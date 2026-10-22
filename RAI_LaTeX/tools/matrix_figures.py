#!/usr/bin/env python3
"""Data for the evaluation's figures from the paper matrices
(tools/rai/run_sweep.py --set final, --set rotation and --set rotation-byz):
every rep of every cell, the pooled ones included.

  matrix_figures.py --p0 <dir> --p1 <dir> --rotation <dir> --rotation-byz <dir> <out dir>

Writes to <out dir>:
  bars-p0-<arm>.dat     one row per scenario of the p = 0 matrix (Nano's
      quorum): x p50 p50lo p50hi p95 p95lo p95hi gput gputlo gputhi, the
      median over the reps and its range
  bars-fast-<cfg>.dat   the same for RAI alone, cfg p0 or p1 (f = p = 1)
  bars-rot-<arm>.dat    the same for the rotation cells, the Byzantine ones
      after the honest ones
  forks-<scenario>-<arm>.dat  blocks not final after the load (rep 1): t open
  timeline-, closes-, loadend-<name>.dat  per-second confirmations, close
      windows and the end of the load of one run (see paper_figures.py)
  handoff-<name>.dat    the first validator entering the epoch the new
      committee runs: t y
  matrix-numbers.tex    \\mnum{<key>} macros
"""
import argparse
import json
import math
import statistics
from pathlib import Path

from paper_figures import load_end_ms, write_timeline
from sweep_figures import fmt

SCENARIOS = ['nofork', 'fork5-grace', 'fork10-grace',
             'nofork-byznode', 'fork5-grace-byznode', 'fork10-grace-byznode']
ROTATION = ['nofork-rot', 'fork5-grace-rot', 'fork10-grace-rot']
ROTATION_BYZ = ['nofork-rot-byznode', 'fork5-grace-rot-byznode', 'fork10-grace-rot-byznode']
ARMS = ['rai', 'pause', 'develop']
BLOCKS = 48000
# The epoch the rotated committee runs: C(0), derived from epoch 0's state
HANDOFF_EPOCH = 2


def reps(matrix, cell, arm, rate=2000):
    """Every rep of a cell: (summary, run directory), in rep order"""
    base = matrix / cell / f'r{rate}'
    found = []
    for k in range(1, 100):
        name = arm if k == 1 else f'{arm}-rep{k}'
        summary = base / f'{name}.summary.json'
        if not summary.exists():
            if k > 3:
                break
            continue
        found.append((json.loads(summary.read_text()), base / name))
    return found


def result(run):
    path = run / 'result.json'
    return json.loads(path.read_text()) if path.exists() else None


def spread(values):
    values = [v for v in values if v is not None and not (isinstance(v, float) and math.isnan(v))]
    if not values:
        return (math.nan,) * 3
    return statistics.median(values), min(values), max(values)


def cell_stats(runs):
    clients = [summary.get('client') or {} for summary, _ in runs]
    return {
        'p50': spread([c.get('nonfork_p50_ms') for c in clients]),
        'p95': spread([c.get('nonfork_p95_ms') for c in clients]),
        'gput': spread([c.get('nonfork_goodput_cps') for c in clients]),
    }


def open_forks(runs):
    """The most blocks left open on a validator after the fork grace, per rep"""
    left = []
    for summary, _ in runs:
        grace = summary.get('fork_grace') or {}
        after = grace.get('uncemented_after')
        left.append(max(after) if after else 0)
    return left


def resolved_after(res):
    """Seconds from the end of the load until no block was open anywhere"""
    end = load_end_ms(res.get('client') or {}, BLOCKS)
    if end is None:
        return None
    for t, count in res.get('open_trace') or []:
        if count == 0:
            return max(0.0, (t - end) / 1000)
    return None


def bars_row(x, stats):
    return ' '.join([str(x)] + [fmt(round(v)) if not math.isnan(v) else fmt(v)
                                for key in ('p50', 'p95', 'gput') for v in stats[key]])


def write_forks(out, name, res):
    end = load_end_ms(res.get('client') or {}, BLOCKS)
    trace = res.get('open_trace') or []
    if end is None or not trace:
        return
    lines = ['t open'] + [f'{(t - end) / 1000:.1f} {count}' for t, count in trace]
    (out / f'forks-{name}.dat').write_text('\n'.join(lines) + '\n')


def write_handoff(out, name, res):
    start = (res.get('client') or {}).get('series_start_unix_ms')
    entered = [e['t_unix_ms'] for e in res.get('epoch_events') or []
               if e['kind'] == 'advanced' and e['epoch'] == HANDOFF_EPOCH]
    if not start or not entered:
        return None
    t = (min(entered) - start) / 1000
    (out / f'handoff-{name}.dat').write_text(f't y\n{t:.2f} 0\n{t:.2f} 1\n')
    return t


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--p0', type=Path, required=True)
    parser.add_argument('--p1', type=Path, required=True)
    parser.add_argument('--rotation', type=Path, required=True)
    parser.add_argument('--rotation-byz', type=Path, required=True)
    parser.add_argument('out', type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    numbers = []

    def number(key, value):
        if isinstance(value, float) and not math.isnan(value) and value >= 100:
            value = round(value)
        numbers.append(f'\\expandafter\\def\\csname mnum@{key}\\endcsname{{{fmt(value)}}}')

    header = 'x p50 p50lo p50hi p95 p95lo p95hi gput gputlo gputhi'

    def matrix_cells(matrix, cells, arms, prefix, tag):
        """cells: names in `matrix`, or (matrix, name) pairs"""
        rows = {arm: [header] for arm in arms}
        for x, cell in enumerate(cells):
            source, cell = cell if isinstance(cell, tuple) else (matrix, cell)
            for arm in arms:
                runs = reps(source, cell, arm)
                stats = cell_stats(runs)
                rows[arm].append(bars_row(x, stats))
                key = f'{tag}-{cell}-{arm}'
                number(f'{key}-reps', len(runs))
                for metric, (med, lo, hi) in stats.items():
                    number(f'{key}-{metric}', med)
                    number(f'{key}-{metric}-lo', lo)
                    number(f'{key}-{metric}-hi', hi)
                left = open_forks(runs)
                number(f'{key}-open-max', max(left) if left else math.nan)
                number(f'{key}-open-min', min(left) if left else math.nan)
                resolved = [resolved_after(res) for _, run in runs if (res := result(run))]
                resolved = [r for r in resolved if r is not None]
                if resolved and len(resolved) == len(runs):
                    number(f'{key}-resolved-max', max(resolved))
                closes = [((s.get('close_duration_ms') or {}).get('median') or math.nan) / 1000 for s, _ in runs]
                number(f'{key}-close-median', spread(closes)[0])
        for arm, lines in rows.items():
            (args.out / f'{prefix}-{arm}.dat').write_text('\n'.join(lines) + '\n')

    matrix_cells(args.p0, SCENARIOS, ARMS, 'bars-p0', 'p0')
    matrix_cells(args.p1, SCENARIOS, ARMS, 'bars-p1', 'p1')
    matrix_cells(None, [(args.rotation, c) for c in ROTATION] + [(args.rotation_byz, c) for c in ROTATION_BYZ],
                 ARMS, 'bars-rot', 'rot')
    # The fast-path comparison: RAI in both configurations
    for cfg, matrix in (('p0', args.p0), ('p1', args.p1)):
        lines = [header]
        for x, cell in enumerate(SCENARIOS):
            lines.append(bars_row(x, cell_stats(reps(matrix, cell, 'rai'))))
        (args.out / f'bars-fast-{cfg}.dat').write_text('\n'.join(lines) + '\n')

    # Fork termination after the load: rep 1 of each system
    for cell in ('fork10-grace', 'fork10-grace-byznode'):
        for arm in ARMS:
            runs = reps(args.p0, cell, arm)
            if runs and (res := result(runs[0][1])):
                write_forks(args.out, f'{cell}-{arm}', res)
    # A close under load with a Byzantine node: rep 1, RAI and pause
    for arm in ('rai', 'pause'):
        runs = reps(args.p0, 'fork10-grace-byznode', arm)
        if runs and (res := result(runs[0][1])):
            write_timeline(args.out, f'p0-fork10-byz-{arm}', res, BLOCKS)
    # The rotation: rep 1 of RAI with 10% forks
    runs = reps(args.rotation, 'fork10-grace-rot', 'rai')
    if runs and (res := result(runs[0][1])):
        write_timeline(args.out, 'rot-fork10-rai', res, BLOCKS)
        number('rot-handoff-t', write_handoff(args.out, 'rot-fork10-rai', res))

    numbers.append('\\newcommand{\\mnum}[1]{\\csname mnum@#1\\endcsname}')
    (args.out / 'matrix-numbers.tex').write_text('\n'.join(numbers) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
