#!/usr/bin/env python3
"""Paper evaluation: develop (no checkpoints) vs RAI v41 (8 s timed epochs,
three checkpoints) on the same workload and the same client source.

    paper_eval.py OUT [--repetitions 5] [--variants nofork,fork5,...]

Workload: 45,000 blocks, 45,000 accounts, 2,000 blocks/s, six PRs, --no-prio.
Every repetition runs every variant once per arm; the arm order alternates.
Nothing is retried or dropped. One line per run goes to OUT/progress.log.
Measurement and per-run evidence come from pair_run.py (a pinned copy of the
cross-epoch-minimal run.py): non-fork goodput and non-fork p50/p95/p99 from
the client's RAI_BENCH_METRICS histogram.
"""
import argparse
import json
import math
import signal
import subprocess
import time
import urllib.error
from pathlib import Path
from types import SimpleNamespace

import pair_run

# The editor showing this session keeps a renderer at ~30 %; only a process
# above this share of one core holds a run back (a pegged fileWatcher once
# made runs six times slower). Every run records what was busy at its start.
BUSY_PERCENT = 50
ARTIFACTS = Path('/tmp/rai-cross-epoch-artifacts/bin')
HERE = Path(__file__).resolve().parent.parent
ARMS = {
    # develop ff2b32710, legacy wire format; client: the v41 nanospam source
    # built without rai_protocol (the only difference is the vote encoding)
    'develop': dict(node=HERE / 'bin/develop/rsnano', client=HERE / 'bin/develop/nanospam',
                    epoch_ms=0, required_checkpoints=0, extra=[]),
    # rai-cross-epoch-minimal v43: v41 (6f67f33f1) plus (v42) the notarization
    # certificate assembled from retained votes counting final voters as the
    # election does (runs/ holds v41 results; fork10-byz1 stalled its epoch-2
    # close on that difference) and (v43) solicitations naming every candidate
    # held, so fork candidates are not re-sent to a node that holds them.
    # Pinned binaries.
    'rai': dict(node=ARTIFACTS / 'no-crash-v43/rsnano', client=ARTIFACTS / 'no-crash-v43/nanospam',
                epoch_ms=8000, required_checkpoints=3,
                extra=['--committee-model', 'equal_weight', '--committee-f', '1',
                       '--committee-p', '1', '--checkpoint-finalization', 'certificate_only']),
}
VARIANTS = {
    'nofork': dict(forks=0, absent=0, fault=[]),
    'fork5': dict(forks=5, absent=0, fault=[]),
    'fork5-offline1': dict(forks=5, absent=1, fault=['--offline', '1']),
    'fork5-byz1': dict(forks=5, absent=1, fault=['--byzantine', '1']),
    'nofork-offline1': dict(forks=0, absent=1, fault=['--offline', '1']),
    'nofork-byz1': dict(forks=0, absent=1, fault=['--byzantine', '1']),
    'fork10': dict(forks=10, absent=0, fault=[]),
    'fork10-offline1': dict(forks=10, absent=1, fault=['--offline', '1']),
    'fork10-byz1': dict(forks=10, absent=1, fault=['--byzantine', '1']),
}


def legacy_aware_settled(states, forks=0):
    """develop has no final_state RPC. Settled when all cemented counts are
    equal and, without forks, every node cemented every block it holds (with
    forks a deadlocked fork stays uncemented, as under RAI a split position
    may stay unresolved)."""
    if states and all('error' in s.get('final_state', {}) for s in states):
        counts = [s['block_count'] for s in states]
        if not all('count' in c for c in counts):
            return False
        if forks == 0 and any(c['count'] != c['cemented'] for c in counts):
            return False
        return len({c['cemented'] for c in counts}) == 1
    return ORIGINAL_SETTLED(states, forks)


def legacy_aware_rpc(port, body):
    """develop answers the RAI-only final_state action with HTTP 422"""
    try:
        return ORIGINAL_RPC(port, body)
    except urllib.error.HTTPError as error:
        if error.code == 422 and body.get('action') == 'final_state':
            return {'error': 'final_state unsupported (HTTP 422)'}
        raise


ORIGINAL_SETTLED = pair_run.settled
pair_run.settled = legacy_aware_settled
ORIGINAL_RPC = pair_run.rpc
pair_run.rpc = legacy_aware_rpc


def decided_checkpoints(run_dir):
    """Per node, how many epochs have a decided checkpoint (closed_value);
    None for develop, which has no final_state"""
    counts = []
    for snapshot in json.loads((run_dir / 'rpc.json').read_text()):
        epochs = snapshot.get('final_state', {}).get('epochs')
        if epochs is None:
            return None
        counts.append(sum(1 for e in epochs if e.get('close', {}).get('closed_value')))
    return counts


def develop_publish_timeout(out, variant):
    """develop gets the publishing time RAI needed: 1.5 x the slowest
    publishing duration of a completed RAI run of the same variant, else of
    any variant (None: none yet, only the overall ceiling applies). Setup is
    not charged: develop's setup is slower than RAI's."""
    def durations(pattern):
        return [r['metrics']['duration_secs'] for r in
                (json.loads(p.read_text()) for p in out.glob(pattern))
                if r.get('complete') and 'metrics' in r]
    found = durations(f'{variant}/pair-*-rai/result.json') or durations('*/pair-*-rai/result.json')
    return math.ceil(1.5 * max(found)) if found else None


def busy_processes():
    out = subprocess.run(['top', '-l', '2', '-o', 'cpu', '-n', '8', '-stats', 'cpu,command'],
                         capture_output=True, text=True).stdout.splitlines()
    second = out[len(out) // 2:]
    busy = []
    for line in second:
        parts = line.split(None, 1)
        try:
            cpu = float(parts[0])
        except (ValueError, IndexError):
            continue
        if cpu > BUSY_PERCENT and parts[1].strip() not in ('top',):
            busy.append(line.strip())
    return busy


def wait_quiet(log, limit=1800):
    start = time.monotonic()
    while True:
        busy = busy_processes()
        if not busy or time.monotonic() - start > limit:
            return busy
        log(f'  host busy, waiting: {busy}')
        time.sleep(15)


def stop_cleanly(signum, frame):
    raise KeyboardInterrupt


def main():
    # A detached run ignores SIGINT; either signal stops it through the
    # run's own cleanup, which shuts down that run's nodes
    signal.signal(signal.SIGINT, stop_cleanly)
    signal.signal(signal.SIGTERM, stop_cleanly)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--repetitions', type=int, default=5)
    parser.add_argument('--first-repetition', type=int, default=0)
    parser.add_argument('--variants', default=','.join(VARIANTS))
    parser.add_argument('--arms', default=','.join(ARMS))
    parser.add_argument('--timeout', type=int, default=420)
    parser.add_argument('--settle-seconds', type=int, default=30)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    progress = out / 'progress.log'

    def log(line):
        with progress.open('a') as f:
            f.write(line + '\n')
        print(line, flush=True)

    manifest = {'arms': {k: {**v, 'node': str(v['node']), 'client': str(v['client']),
                             'node_sha256': pair_run.sha256(v['node']),
                             'client_sha256': pair_run.sha256(v['client'])} for k, v in ARMS.items()},
                'variants': VARIANTS, 'blocks': 45000, 'accounts': 45000, 'rate': 2000,
                'timeout': args.timeout, 'settle_seconds': args.settle_seconds,
                'pair_run_sha256': pair_run.sha256(Path(pair_run.__file__)),
                'started': time.time()}
    (out / f'manifest-{int(time.time())}.json').write_text(json.dumps(manifest, indent=2) + '\n')
    variants = args.variants.split(',')
    arms = args.arms.split(',')
    for rep in range(args.first_repetition, args.first_repetition + args.repetitions):
        for vi, variant in enumerate(variants):
            order = arms if (rep + vi) % 2 == 0 else list(reversed(arms))
            for arm in order:
                a, v = ARMS[arm], VARIANTS[variant]
                target = out / variant
                target.mkdir(exist_ok=True)
                if (target / f'pair-{rep:02d}-{arm}').exists():
                    log(f'{variant} rep{rep} {arm}: exists, skipped')
                    continue
                if pair_run.occupied_ports():
                    log(f'STOP: ports occupied {pair_run.occupied_ports()}')
                    return 2
                timeout = args.timeout
                publish_timeout = develop_publish_timeout(out, variant) if arm == 'develop' else None
                busy = wait_quiet(log)
                ns = SimpleNamespace(out=target, client=a['client'], blocks=45000, accounts=45000,
                                     rate=2000, forks=v['forks'], epoch_ms=a['epoch_ms'],
                                     extra=a['extra'] + v['fault'], absent=v['absent'],
                                     required_checkpoints=a['required_checkpoints'],
                                     baseline_wall_times=[], timeout=timeout,
                                     timeout_factor=1.5, settle_seconds=args.settle_seconds,
                                     keep_data=False, publish_timeout=publish_timeout)
                r = pair_run.run(ns, arm, a['node'], rep)
                r['host_busy_at_start'] = busy
                r['timeout_rule'] = ('publishing: 1.5 x slowest completed RAI publishing duration, '
                                     'same variant first; overall ceiling for setup'
                                     if arm == 'develop' else 'overall ceiling only')
                r['decided_checkpoints'] = decided_checkpoints(target / f'pair-{rep:02d}-{arm}')
                path = target / f'pair-{rep:02d}-{arm}' / 'result.json'
                path.write_text(json.dumps(r, indent=2) + '\n')
                m = r.get('metrics', {})
                gp = f"{r['goodput']:.0f}" if r.get('goodput') else 'n/a'
                log(f"{variant} rep{rep} {arm}: complete={r.get('complete')} "
                    f"settled={r.get('settled_consistent')} goodput={gp} "
                    f"p50/p95/p99={r.get('p50_ms')}/{r.get('p95_ms')}/{r.get('p99_ms')} ms "
                    f"fork_unresolved={m.get('fork_unresolved_at_end')} wall={r['wall_secs']:.0f}s publish_timeout={publish_timeout}s checkpoints={r['decided_checkpoints']}"
                    + (f' busy={busy}' if busy else ''))
    log('DONE')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
