#!/usr/bin/env python3
"""All-online live checkpoint smoke benchmark; does not claim Gate B or termination proof."""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time
from run_variant import rpc, sha256


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('output', type=Path)
    p.add_argument('--bin-dir', type=Path, default=Path('target/release'))
    p.add_argument('--timeout', type=int, default=180)
    p.add_argument('--blocks', type=int, default=360)
    p.add_argument('--rate', type=int, default=10)
    p.add_argument('--epoch-ms', type=int, default=5000)
    args = p.parse_args()
    for i in range(6):
        try:
            rpc(i, 'version')
        except OSError:
            continue
        raise RuntimeError(f'RPC port for PR{i} is already occupied')
    args.output.mkdir(parents=True, exist_ok=False)
    bins = args.bin_dir.resolve()
    data = args.output.resolve() / 'data'
    data.mkdir()
    command = [str(bins / 'nanospam'), '--data-dir', str(data), '--prs', '6',
               '--committee-model', 'equal_weight', '--committee-f', '1', '--committee-p', '1',
               '--blocks', str(args.blocks), '--accounts', str(args.blocks), '--rate', str(args.rate),
               '--epoch-duration-ms', str(args.epoch_ms), '--no-prio', '--no-kill']
    result = dict(command=command, status='running', all_online=True, gate_b=False,
                  source_revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                  synchronizer_proof='deferred', node_sha256=sha256(bins / 'rsnano'),
                  client_sha256=sha256(bins / 'nanospam'))
    env = dict(os.environ, PATH=str(bins) + os.pathsep + os.environ['PATH'],
               RAI_CHECKPOINT_BENCHMARK='1', RUST_LOG='nanospam=info', NANO_LOG='noansi')
    process = None
    try:
        with (args.output / 'run.log').open('w') as log:
            process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            deadline = time.monotonic() + args.timeout
            while process.poll() is None and time.monotonic() < deadline:
                time.sleep(1)
            if process.poll() is None:
                raise RuntimeError('nanospam timed out')
            if process.returncode:
                raise RuntimeError(f'nanospam exited with {process.returncode}')
            while time.monotonic() < deadline:
                text = (args.output / 'run.log').read_text()
                proofs = re.findall(r'CHECKPOINT_BENCH_PROOF epoch=(\d+) node=(\S+) value=(\S+) path=(\w+) bytes=(\d+)', text)
                epochs = {}
                for epoch, node, value, path, size in proofs:
                    epochs.setdefault(epoch, {})[node] = dict(value=value, path=path, proof_bytes=int(size))
                complete = [e for e, nodes in epochs.items() if len(nodes) == 6 and len({n['value'] for n in nodes.values()}) == 1]
                if len(complete) >= 3:
                    break
                time.sleep(1)
            result['checkpoint_proofs'] = epochs
            result['complete_epochs'] = complete
            result['final_states'] = [rpc(i, 'final_state') for i in range(6)]
            result['block_counts'] = [rpc(i, 'block_count') for i in range(6)]
            result['counters'] = [rpc(i, 'stats', type='counters') for i in range(6)]
            rates = re.findall(r'Confirmation rate: ([\d.]+) cps', text)
            result['confirmation_rate_cps'] = float(rates[-1]) if rates else None
            result['finalization_p50_ms'] = [
                int(entry['value']) for counters in result['counters']
                for entry in counters.get('entries', [])
                if entry.get('type') == 'latency_finalization_nonfork' and entry.get('detail') == 'p50_ms'
            ]
            starts = {(epoch, node): int(ts) for epoch, node, ts in re.findall(
                r'CHECKPOINT_BENCH_START epoch=(\d+) node=(\S+) t=(\d+)', text)}
            result['checkpoint_duration_ms'] = [
                dict(epoch=int(epoch), node=node, duration_ms=int(ts) - starts[(epoch, node)])
                for epoch, node, ts in re.findall(
                    r'CHECKPOINT_BENCH_PROOF epoch=(\d+) node=(\S+).*? t=(\d+)', text)
                if (epoch, node) in starts
            ]
            result['fast_decisions'] = sum(n['path'] == 'fast' for nodes in epochs.values() for n in nodes.values())
            result['slow_decisions'] = sum(n['path'] == 'slow' for nodes in epochs.values() for n in nodes.values())
            result['installed_count'] = len(re.findall(r'CHECKPOINT_INSTALLED epoch=', text))
            result['agreement'] = all(len({n['value'] for n in nodes.values()}) == 1 for nodes in epochs.values())
            result['status'] = 'passed' if (len(complete) >= 3 and result['agreement'] and result['installed_count'] >= 18
                and all(int(s['current_epoch']) >= 3 for s in result['final_states'])) else 'failed'
    except Exception as error:
        result['status'] = 'failed'
        result['error'] = str(error)
    finally:
        if process is not None:
            try:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=5)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                pass
            time.sleep(1)
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({k:v for k,v in result.items() if k not in ('counters', 'final_states', 'checkpoint_proofs')}, indent=2))
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
