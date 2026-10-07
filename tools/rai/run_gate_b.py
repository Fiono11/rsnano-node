#!/usr/bin/env python3
"""Run the Gate B workload: timed epochs that close through the Kudzu close
election, six equal-weight representatives, and retain the evidence.

A run settles when every validator has cemented the same blocks and holds
the same decided value for every epoch any of them decided, and at least
--min-closed epochs are decided everywhere."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import signal
import statistics
import subprocess
import tempfile
import time

PRS = 6


def rpc(index, action, **fields):
    connection = http.client.HTTPConnection('::1', 17076 + index * 10, timeout=5)
    try:
        connection.request('POST', '/', json.dumps(dict(action=action, **fields)))
        result = json.loads(connection.getresponse().read())
        if 'error' in result:
            raise RuntimeError(result['error'])
        return result
    finally:
        connection.close()


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def busy_processes(host):
    """macOS reports the first top sample since boot; assess the second one."""
    busy = []
    if os.uname().sysname == 'Darwin':
        for line in host.rsplit('PID', 1)[-1].splitlines():
            match = re.match(r'\s*\d+\s+([\d.]+)\s+(\S+)', line)
            if match and float(match[1]) > 25 and match[2] != 'top':
                busy.append(line.strip())
    return busy


def closes_of(state):
    """epoch -> close info of one node's final_state"""
    return {int(e['epoch']): e['close'] for e in state.get('epochs', []) if 'close' in e}


def settlement(counts, states, min_closed):
    cemented = {int(c['cemented']) for c in counts}
    all_cemented = all(c['count'] == c['cemented'] for c in counts) and len(cemented) == 1
    closes = [closes_of(s) for s in states]
    decided = [{e for e, c in node.items() if c.get('value')} for node in closes]
    every = set.intersection(*decided) if decided else set()
    any_ = set.union(*decided) if decided else set()
    values = {e: {node[e].get('closed_value') for node in closes if e in node and node[e].get('value')}
              for e in any_}
    agree = all(len(v) == 1 for v in values.values())
    return dict(all_cemented=all_cemented, cemented=sorted(cemented),
                decided_everywhere=sorted(every), decided_somewhere=sorted(any_),
                values_agree=agree,
                settled=all_cemented and agree and every == any_ and len(every) >= min_closed)


def close_timings(text):
    """Per epoch: when the first node ended and left it, and when the first
    and the last node saw its close certificate, from the node diagnostics
    (unix ms in the trailing t= field; the lines carry no node id)."""
    events = {}
    for kind, epoch, rest, t in re.findall(r'^(EPOCH_ENDED|EPOCH_ADVANCED|EPOCH_CLOSED) epoch=(\d+)(.*?) t=(\d+)$', text, re.M):
        epoch = int(epoch)
        if kind == 'EPOCH_ADVANCED':
            epoch -= 1  # advancing to e + 1 leaves e
        entry = events.setdefault(epoch, dict(ended=[], left=[], closed=[], rounds=[]))
        key = dict(EPOCH_ENDED='ended', EPOCH_ADVANCED='left', EPOCH_CLOSED='closed')[kind]
        entry[key].append(int(t))
        if kind == 'EPOCH_CLOSED':
            entry['rounds'].append(int(re.search(r'round=(\d+)', rest)[1]))
    result = {}
    for epoch, e in sorted(events.items()):
        if not e['closed'] or not e['left']:
            continue
        start = min(e['left'])
        result[epoch] = dict(nodes_left=len(e['left']), nodes_closed=len(e['closed']),
                             first_close_ms=min(e['closed']) - start,
                             last_close_ms=max(e['closed']) - start,
                             left_spread_ms=max(e['left']) - start,
                             rounds=sorted(set(e['rounds'])))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--bin-dir', type=Path, default=Path('target/release'))
    parser.add_argument('--timeout', type=int, default=300)
    parser.add_argument('--blocks', type=int, default=45000)
    parser.add_argument('--accounts', type=int, default=45000)
    parser.add_argument('--rate', type=int, default=2000)
    parser.add_argument('--fork-percentage', type=int, default=0)
    parser.add_argument('--epoch-ms', type=int, default=8000)
    parser.add_argument('--min-closed', type=int, default=3)
    parser.add_argument('--weighted', action='store_true', help='stake-weighted committees instead of equal weight f = p = 1')
    parser.add_argument('--allow-busy', action='store_true', help='Correctness run only; does not satisfy the performance gate')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    binary_dir = args.bin_dir.resolve()
    result = dict(gate='B', source_revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                  node_sha256=sha256(binary_dir / 'rsnano'), client_sha256=sha256(binary_dir / 'nanospam'))
    top_args = ['top', '-l', '2', '-n', '10', '-o', 'cpu', '-stats', 'pid,cpu,command'] if os.uname().sysname == 'Darwin' else ['top', '-b', '-n', '2', '-d', '1']
    host = subprocess.check_output(top_args, text=True)
    (args.output / 'host.txt').write_text(host)
    busy = busy_processes(host)
    result['busy_processes'] = busy
    result['performance_eligible'] = not busy and not args.allow_busy
    if busy and not args.allow_busy:
        result['status'] = 'blocked_busy_host'
        (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2))
        return 7
    for i in range(PRS):
        try:
            rpc(i, 'version')
        except OSError:
            continue
        raise RuntimeError(f'Benchmark RPC port for PR{i} is already in use')
    with tempfile.TemporaryDirectory(prefix='rai-gate-b-') as temporary:
        data = Path(temporary)
        command = [str(binary_dir / 'nanospam'), '--data-dir', str(data), '--prs', str(PRS), '--no-prio',
                   '--blocks', str(args.blocks), '--accounts', str(args.accounts), '--rate', str(args.rate),
                   '--fork-percentage', str(args.fork_percentage), '--epoch-duration-ms', str(args.epoch_ms),
                   '--no-kill']
        if not args.weighted:
            command += ['--committee-model', 'equal_weight', '--committee-f', '1', '--committee-p', '1']
        result['command'] = command
        result['status'] = 'running'
        env = dict(os.environ, PATH=str(binary_dir) + os.pathsep + os.environ['PATH'], RUST_LOG='nanospam=info', NANO_LOG='noansi')
        process = None
        try:
            with (args.output / 'run.log').open('w') as log:
                process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                deadline = time.monotonic() + args.timeout
                while process.poll() is None and time.monotonic() < deadline:
                    time.sleep(1)
                if process.poll() is None:
                    raise RuntimeError('nanospam exceeded the run deadline')
                if process.returncode:
                    raise RuntimeError(f'nanospam exited with {process.returncode}')
                result['client_finished_s'] = round(args.timeout - (deadline - time.monotonic()), 1)
                while True:
                    counts = [rpc(i, 'block_count') for i in range(PRS)]
                    states = [rpc(i, 'final_state') for i in range(PRS)]
                    settled = settlement(counts, states, args.min_closed)
                    if settled['settled'] or time.monotonic() >= deadline:
                        break
                    time.sleep(1)
                result['settled_after_s'] = round(args.timeout - (deadline - time.monotonic()), 1)
                result['settlement'] = settled
                result['block_counts'] = counts
                result['final_states'] = states
                result['stats'] = [rpc(i, 'stats', type='objects') for i in range(PRS)]
                result['counters'] = [rpc(i, 'stats', type='counters') for i in range(PRS)]
                result['finalization_p50_ms'] = [
                    int(entry['value']) for counters in result['counters']
                    for entry in counters.get('entries', [])
                    if entry.get('type') == 'latency_finalization_nonfork'
                    and entry.get('detail') == 'p50_ms'
                ]
                result['close_rounds'] = sorted({(e, c.get('closed_round')) for s in states
                                                 for e, c in closes_of(s).items() if c.get('closed_round')})
                text = (args.output / 'run.log').read_text()
                timings = close_timings(text)
                result['close_timings'] = timings
                last = [t['last_close_ms'] for t in timings.values()]
                result['close_duration_ms'] = dict(max=max(last), median=statistics.median(last)) if last else None
                rates = re.findall(r'Confirmation rate: ([\d.]+) cps', text)
                result['confirmation_rate_cps'] = float(rates[-1]) if rates else None
                confirmed = re.findall(r'Confirm(?:ed|ing) ([\d,]+) blocks', text)
                result['confirmed_primary'] = int(confirmed[-1].replace(',', '')) if confirmed else 0
                result['correctness_passed'] = (result['confirmed_primary'] >= args.blocks
                                                and settled['settled']
                                                and int(settled['cemented'][0]) >= args.blocks)
                result['status'] = 'settled' if result['correctness_passed'] else 'failed'
                for i in range(PRS):
                    saved = args.output / f'pr{i}'
                    saved.mkdir()
                    for config in (data / f'pr{i}').glob('config-*'):
                        shutil.copy2(config, saved / config.name)
        except Exception as error:
            result['status'] = 'failed'
            result['error'] = str(error)
        finally:
            # Only this run's process group; never signal unrelated node processes.
            if process is not None:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=5)
                    time.sleep(1)
                except (ProcessLookupError, subprocess.TimeoutExpired):
                    pass
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            (args.output / 'result.json').write_text(json.dumps(result, indent=2, default=list) + '\n')
    summary = {k: v for k, v in result.items() if k not in ('stats', 'counters', 'final_states', 'block_counts')}
    print(json.dumps(summary, indent=2, default=list))
    return 0 if result.get('correctness_passed') else 1


if __name__ == '__main__':
    raise SystemExit(main())
