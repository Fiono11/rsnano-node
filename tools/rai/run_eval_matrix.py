#!/usr/bin/env python3
"""Repeated evaluation matrix: every variant, every arm, several repetitions,
interleaved so that host drift hits every arm alike.

An arm is a node build with flags, e.g.
  --arm rai=target/release
  --arm pause=target/release:--epoch-transition=pause
  --arm nano=/path/to/develop-bin:--no-epochs:--timeout=90:--settle-timeout=45
Flags after the colon are passed to run_gate_b.py (use '=' inside a flag).
Every run writes <out>/<variant>/<arm>-r<k>/ (run_gate_b.py's record) and
<out>/<variant>/<arm>-r<k>.summary.json (its printed summary). A run whose
summary exists is not repeated, so an interrupted matrix resumes."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import time

VARIANTS = {
    'nofork': [], 'nofork-offline1': ['--offline', '1'], 'nofork-byz1': ['--byzantine', '1'],
    'fork5': ['--fork-percentage', '5'], 'fork5-offline1': ['--fork-percentage', '5', '--offline', '1'],
    'fork5-byz1': ['--fork-percentage', '5', '--byzantine', '1'],
    'fork10': ['--fork-percentage', '10'], 'fork10-offline1': ['--fork-percentage', '10', '--offline', '1'],
    'fork10-byz1': ['--fork-percentage', '10', '--byzantine', '1'],
}


def parse_arm(spec):
    name, _, rest = spec.partition('=')
    parts = rest.split(':')
    return name, dict(bin_dir=parts[0], flags=parts[1:])


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('output', type=Path)
    parser.add_argument('--arm', action='append', required=True, help='name=bin_dir[:flag[:flag...]]')
    parser.add_argument('--variants', default=','.join(VARIANTS), help='comma-separated subset of ' + ', '.join(VARIANTS))
    parser.add_argument('--reps', type=int, default=3)
    parser.add_argument('--extra', default='', help='flags for every run, e.g. "--network-delay-ms 40"')
    parser.add_argument('--wait-quiet', type=int, default=900)
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    arms = dict(parse_arm(spec) for spec in args.arm)
    variants = [v.strip() for v in args.variants.split(',') if v.strip()]
    unknown = [v for v in variants if v not in VARIANTS]
    if unknown:
        sys.exit(f'unknown variants: {unknown}')
    args.output.mkdir(parents=True, exist_ok=True)
    harness = Path(__file__).resolve().parent / 'run_gate_b.py'
    plan = dict(arms=arms, variants=variants, reps=args.reps, extra=args.extra, started=time.time(), runs=[])
    (args.output / 'index.json').write_text(json.dumps(plan, indent=2) + '\n')
    for rep in range(1, args.reps + 1):
        for variant in variants:
            for arm, spec in arms.items():
                run = args.output / variant / f'{arm}-r{rep}'
                summary = run.with_suffix('.summary.json')
                if summary.exists():
                    continue
                if run.exists():
                    # an interrupted run: keep the directory under a new name
                    run.rename(run.with_name(run.name + f'.aborted-{int(time.time())}'))
                run.parent.mkdir(parents=True, exist_ok=True)
                command = [sys.executable, str(harness), str(run), '--bin-dir', spec['bin_dir'],
                           '--wait-quiet', str(args.wait_quiet), '--timeout', '150', '--settle-timeout', '120',
                           '--stall-abort', '45'] + VARIANTS[variant] + spec['flags'] + args.extra.split()
                print(f'[{time.strftime("%H:%M:%S")}] {variant} {arm} r{rep}', flush=True)
                if args.dry_run:
                    print('  ' + ' '.join(command))
                    continue
                started = time.time()
                completed = subprocess.run(command, capture_output=True, text=True)
                summary.write_text(completed.stdout)
                if completed.stderr:
                    run.with_suffix('.err').write_text(completed.stderr)
                try:
                    status = json.loads(completed.stdout).get('status')
                except (json.JSONDecodeError, AttributeError):
                    status = f'exit {completed.returncode}'
                plan['runs'].append(dict(variant=variant, arm=arm, rep=rep, status=status,
                                         seconds=round(time.time() - started)))
                (args.output / 'index.json').write_text(json.dumps(plan, indent=2) + '\n')
                print(f'  {status} in {round(time.time() - started)} s', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
