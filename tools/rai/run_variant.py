#!/usr/bin/env python3
"""Run the phase-0 single-epoch Gate A workload and retain its evidence."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tempfile
import time


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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--bin-dir', type=Path, default=Path('target/release'))
    parser.add_argument('--timeout', type=int, default=300)
    parser.add_argument('--blocks', type=int, default=45000)
    parser.add_argument('--accounts', type=int, default=45000)
    parser.add_argument('--rate', type=int, default=2000)
    parser.add_argument('--allow-busy', action='store_true', help='Correctness run only; does not satisfy the performance gate')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    binary_dir = args.bin_dir.resolve()
    result = dict(gate='A', source_revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                  node_sha256=sha256(binary_dir / 'rsnano'), client_sha256=sha256(binary_dir / 'nanospam'))
    top_args = ['top', '-l', '2', '-n', '10', '-o', 'cpu', '-stats', 'pid,cpu,command'] if os.uname().sysname == 'Darwin' else ['top', '-b', '-n', '2', '-d', '1']
    host = subprocess.check_output(top_args, text=True)
    (args.output / 'host.txt').write_text(host)
    # macOS reports the first sample since boot; assess the second sample.
    busy = []
    if os.uname().sysname == 'Darwin':
        for line in host.rsplit('PID', 1)[-1].splitlines():
            match = re.match(r'\s*\d+\s+([\d.]+)\s+(\S+)', line)
            if match and float(match[1]) > 25 and match[2] != 'top':
                busy.append(line.strip())
    result['busy_processes'] = busy
    result['performance_eligible'] = not busy and not args.allow_busy
    if busy and not args.allow_busy:
        result['status'] = 'blocked_busy_host'
        (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2))
        return 7
    # Refuse to share the benchmark ports with another run.
    for i in range(6):
        try:
            rpc(i, 'version')
        except OSError:
            continue
        raise RuntimeError(f'Benchmark RPC port for PR{i} is already in use')
    with tempfile.TemporaryDirectory(prefix='rai-phase0-') as temporary:
        data = Path(temporary)
        command = [str(binary_dir / 'nanospam'), '--data-dir', str(data), '--prs', '6', '--no-prio',
                   '--blocks', str(args.blocks), '--accounts', str(args.accounts), '--rate', str(args.rate),
                   '--fork-percentage', '0', '--epoch-duration-ms', str((args.timeout + 60) * 1000), '--no-kill']
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
                counts = [rpc(i, 'block_count') for i in range(6)]
                states = [rpc(i, 'final_state') for i in range(6)]
                while time.monotonic() < deadline:
                    if all(c['count'] == c['cemented'] for c in counts) and len({s['hash'] for s in states}) == 1:
                        break
                    time.sleep(1)
                    counts = [rpc(i, 'block_count') for i in range(6)]
                    states = [rpc(i, 'final_state') for i in range(6)]
                result['block_counts'] = counts
                result['final_states'] = states
                result['stats'] = [rpc(i, 'stats', type='objects') for i in range(6)]
                result['counters'] = [rpc(i, 'stats', type='counters') for i in range(6)]
                result['finalization_p50_ms'] = [
                    int(entry['value']) for counters in result['counters']
                    for entry in counters.get('entries', [])
                    if entry.get('type') == 'latency_finalization_nonfork'
                    and entry.get('detail') == 'p50_ms'
                ]
                text = (args.output / 'run.log').read_text()
                rates = re.findall(r'Confirmation rate: ([\d.]+) cps', text)
                result['confirmation_rate_cps'] = float(rates[-1]) if rates else None
                confirmed = re.findall(r'Confirm(?:ed|ing) ([\d,]+) blocks', text)
                result['confirmed_primary'] = int(confirmed[-1].replace(',', '')) if confirmed else 0
                result['correctness_passed'] = (result['confirmed_primary'] >= args.blocks
                    and all(c['count'] == c['cemented'] and int(c['cemented']) >= args.blocks for c in counts)
                    and len({s['hash'] for s in states}) == 1
                    and all(int(s['current_epoch']) == 0 for s in states))
                result['status'] = 'correctness_passed' if result['correctness_passed'] else 'failed'
                for i in range(6):
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
            (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({k: v for k, v in result.items() if k not in ('stats', 'counters', 'final_states')}, indent=2))
    return 0 if result.get('correctness_passed') else 1


if __name__ == '__main__':
    raise SystemExit(main())
