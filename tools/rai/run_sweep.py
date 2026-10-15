#!/usr/bin/env python3
"""Throughput-latency sweep: every variant at several offered rates, one run per
point, each system with the same workload seed. Resumable: a point whose
summary exists is skipped.

  run_sweep.py OUT --set main --arm rai=target/release \\
      --arm pause=target/release:--epoch-transition=pause \\
      --arm develop=<develop bin dir>:--no-epochs:--timeout=240:--settle-timeout=45

Runs land in OUT/<variant>/r<rate>/<arm>{,.summary.json}; with --reps N the
points at --reps-from offered blocks/s and above get repetitions 2..N in
<arm>-rep<k>. Repetition k uses the point's seed plus k - 1.
"""
import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

MAIN_RATES = [500, 1000, 1500, 2000, 2500, 3000]
BYZ_RATES = [1000, 2000, 3000]
RECONF_RATES = [500, 1000, 1500, 2000]
PAPER_RATES = [1000, 2000]


def rotation_schedule(members, standby, epochs=16):
    """One member replaced per epoch: the member in longest leaves, the
    representative out longest joins. PR0 (genesis) never rotates."""
    inside = list(members)
    outside = list(standby)
    parts = []
    for epoch in range(1, epochs + 1):
        leaving = inside.pop(0)
        joining = outside.pop(0)
        inside.append(joining)
        outside.append(leaving)
        parts.append(f'{epoch}:{leaving}>{joining}')
    return ';'.join(parts)


# variant: (flags, rates, arms that run it)
SETS = {
    'main': {
        'nofork': ([], MAIN_RATES, None),
        'fork5': (['--fork-percentage', '5'], MAIN_RATES, None),
        'fork10': (['--fork-percentage', '10'], MAIN_RATES, None),
    },
    'byz': {
        'nofork-byzlead': (['--byzantine', '1', '--byzantine-leads-close'], BYZ_RATES, None),
        'fork5-byzlead': (['--fork-percentage', '5', '--byzantine', '1', '--byzantine-leads-close'], BYZ_RATES, None),
        'fork10-byzlead': (['--fork-percentage', '10', '--byzantine', '1', '--byzantine-leads-close'], BYZ_RATES, None),
    },
    # Fork resolution: after settling, the nodes keep running until no
    # forked position is open anywhere, or none has resolved for 45 s
    # (about five epochs); 600 s at most
    'forks': {
        'fork5-grace': (['--fork-percentage', '5', '--fork-grace', '600'], [1000, 2000], None),
        'fork10-grace': (['--fork-percentage', '10', '--fork-grace', '600'], [1000, 2000], None),
    },
    # The paper's figures: half the host's capacity and the highest load all
    # three sustain without forks, the three systems in
    # four scenarios; fork runs keep the nodes running until no forked
    # position is open (the fork-resolution trace)
    'paper': {
        'nofork': ([], PAPER_RATES, None),
        'fork5-grace': (['--fork-percentage', '5', '--fork-grace', '600'], PAPER_RATES, None),
        'fork10-grace': (['--fork-percentage', '10', '--fork-grace', '600'], PAPER_RATES, None),
        'fork5-byzlead-grace': (['--fork-percentage', '5', '--byzantine', '1', '--byzantine-leads-close',
                                 '--fork-grace', '600'], PAPER_RATES, None),
    },
    # Eight running representatives in both: six committee members and two
    # standby ones (PR6, PR7); the rotating variant replaces one member
    # every epoch. With the Byzantine leader: PR7 is Byzantine (no node),
    # PR5 and PR6 standby, five honest members and the Byzantine one.
    'reconf': {
        'static8': (['--prs', '8', '--standby', '2'], RECONF_RATES, {'rai', 'pause'}),
        'rotate8': (['--prs', '8', '--standby', '2', '--rotation', rotation_schedule([1, 2, 3, 4, 5], [6, 7])],
                    RECONF_RATES, {'rai', 'pause'}),
        'rotate8-byzlead': (['--prs', '8', '--standby', '2', '--byzantine', '1', '--byzantine-leads-close',
                             '--rotation', rotation_schedule([1, 2, 3, 4], [5, 6])],
                            RECONF_RATES, {'rai', 'pause'}),
    },
}


def parse_arm(spec):
    name, _, rest = spec.partition('=')
    parts = rest.split(':')
    return name, dict(bin_dir=parts[0], flags=parts[1:])


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('output', type=Path)
    parser.add_argument('--set', action='append', required=True, choices=list(SETS))
    parser.add_argument('--arm', action='append', required=True, help='name=bin_dir[:flag[:flag...]]')
    parser.add_argument('--blocks', type=int, default=45000)
    parser.add_argument('--wait-quiet', type=int, default=3600)
    parser.add_argument('--reps', type=int, default=1, help='repetitions at the high rates')
    parser.add_argument('--reps-from', type=int, default=1500, help='offered rate from which --reps applies')
    parser.add_argument('--rate', type=int, action='append', help='run only these offered rates')
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    arms = dict(parse_arm(spec) for spec in args.arm)
    args.output.mkdir(parents=True, exist_ok=True)
    harness = Path(__file__).resolve().parent / 'run_gate_b.py'
    plan = dict(sets=args.set, arms=arms, started=time.time(), runs=[])
    index = args.output / 'index.json'
    if index.exists():
        plan['runs'] = json.loads(index.read_text()).get('runs', [])
    for name in args.set:
        for variant, (flags, rates, only) in SETS[name].items():
            for rate in rates:
                if args.rate and rate not in args.rate:
                    continue
                # the same workload choices for every system at this point
                base_seed = 1000 * rate + sum(map(ord, variant))
                reps = args.reps if rate >= args.reps_from else 1
                for k, (arm, spec) in [(k, a) for k in range(1, reps + 1) for a in arms.items()]:
                    if only and arm not in only:
                        continue
                    seed = base_seed + k - 1
                    run = args.output / variant / f'r{rate}' / (arm if k == 1 else f'{arm}-rep{k}')
                    summary = run.with_suffix('.summary.json')
                    if summary.exists():
                        continue
                    if run.exists():
                        run.rename(run.with_name(run.name + f'.aborted-{int(time.time())}'))
                    run.parent.mkdir(parents=True, exist_ok=True)
                    # longer runs at low rates: the load alone lasts blocks / rate seconds
                    timeout = max(150, int(args.blocks / rate * 1.6) + 60)
                    command = [sys.executable, str(harness), str(run), '--bin-dir', spec['bin_dir'],
                               '--wait-quiet', str(args.wait_quiet), '--timeout', str(timeout),
                               '--settle-timeout', '120', '--stall-abort', '45',
                               '--rate', str(rate), '--blocks', str(args.blocks), '--accounts', str(args.blocks),
                               '--seed', str(seed)] + flags + spec['flags']
                    print(f'[{time.strftime("%H:%M:%S")}] {variant} {rate}/s {arm} rep {k}', flush=True)
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
                    plan['runs'].append(dict(variant=variant, rate=rate, arm=arm, rep=k, status=status,
                                             seconds=round(time.time() - started)))
                    index.write_text(json.dumps(plan, indent=2) + '\n')
                    print(f'  {status} in {round(time.time() - started)} s', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
