#!/usr/bin/env python3
"""One bounded candidate diagnostic; not a baseline performance comparison."""
import argparse
import collections
import json
import math
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time
import urllib.request
from run import occupied_ports, prune_run_data, sha256, stop_run_process_group


FINAL_STATUSES = ('Finalized', 'FinalizedDerived')


def checkpoint_index(checkpoint):
    entries = checkpoint.get('entries', []) if checkpoint else []
    hashes = {e['hash']: e for e in entries}
    finals = {(e['account'], e['previous']): e for e in entries if e['status'] in FINAL_STATUSES}
    return hashes, finals


def disposition(fork, block_hash, checkpoint, index=None):
    if not checkpoint:
        return {'outcome': 'unresolved', 'reason': 'no_checkpoint'}
    hashes, finals = index if index is not None else checkpoint_index(checkpoint)
    context = {'checkpoint_epoch': checkpoint['epoch'], 'checkpoint_hash': checkpoint['state_hash']}
    entry = hashes.get(block_hash)
    if entry and (entry['account'], entry['previous']) == (fork['account'], fork['previous']):
        return dict(context, outcome='included', status=entry['status'], height=entry['height'])
    winner = finals.get((fork['account'], fork['previous']))
    if winner and winner['hash'] != block_hash:
        # A certificate-backed winner is the paper's discard justification; a
        # winner the unique-branch variant derived is reported apart from it
        origin = 'derived' if winner['status'] == 'FinalizedDerived' else 'certificate'
        return dict(context, outcome='safely_discarded', reason='conflicting_finalized_checkpoint_block',
                    witness=winner['hash'], witness_origin=origin, height=winner['height'])
    return dict(context, outcome='unresolved', reason='no_inclusion_or_finalized_conflict_witness')


def evaluate(forks, checkpoints):
    indexes = {n: checkpoint_index(c) for n, c in checkpoints.items()}
    rows = []
    for fork in forks.values():
        for key in ('primary', 'alternative'):
            h = fork[key]
            nodes = {str(n): disposition(fork, h, checkpoints.get(n), indexes.get(n)) for n in range(6)}
            rows.append({'hash': h, 'primary': fork['primary'], 'branch': key, 'nodes': nodes,
                         'terminated_on_all_nodes': all(x['outcome'] != 'unresolved' for x in nodes.values())})
    roots = {(c['epoch'], c['state_hash']) for c in checkpoints.values() if c}
    consistent = len(checkpoints) == 6 and all(checkpoints.values()) and len(roots) == 1
    return rows, bool(consistent)


def rpc(node, body, timeout=3):
    req = urllib.request.Request(f'http://[::1]:{17076+10*node}', json.dumps(body).encode(),
                                 {'Content-Type': 'application/json'})
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req, timeout=timeout) as reply:
        return json.load(reply)


def checkpoint_key(state):
    if state.get('epoch') is None or not state.get('state_hash'):
        return None
    return (int(state['epoch']), state['state_hash'])


def cached_checkpoint(state, cache):
    # An epoch alone is insufficient: reuse only the exact installed root.
    return cache.get(checkpoint_key(state))


def retain_terminal_witnesses(rows, witnesses):
    result = []
    for row in rows:
        if row['terminated_on_all_nodes']:
            witnesses.setdefault(row['hash'], dict(row))
        if row['hash'] in witnesses:
            row = dict(witnesses[row['hash']], historical_checkpoint_witness=True)
        result.append(row)
    return result


def installed_checkpoints_consistent(keys):
    return (set(keys) == set(range(6)) and all(v is not None for v in keys.values())
            and len(set(keys.values())) == 1)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--node', type=Path, required=True)
    p.add_argument('--client', type=Path, required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--deadline', type=float, default=156)
    p.add_argument('--min-free-gib', type=float, default=8)
    p.add_argument('--label', default='candidate')
    p.add_argument('--epoch-ms', type=int, default=8000, help='epoch duration; 0 omits the flag (count-based or single epoch via --client-arg)')
    p.add_argument('--client-arg', action='append', default=[],
                   help='extra client flag passed to every node, e.g. --client-arg=--committee-model=equal_weight')
    args = p.parse_args()
    args.out = args.out.resolve(); args.node = args.node.resolve(); args.client = args.client.resolve()
    free_gib = shutil.disk_usage(args.out.parent if args.out.parent.exists() else '/').free / 2**30
    if free_gib < args.min_free_gib:
        raise RuntimeError(f'Only {free_gib:.1f} GiB free, below the {args.min_free_gib} GiB threshold')
    if occupied_ports(): raise RuntimeError('Benchmark ports occupied')
    args.out.mkdir(parents=True, exist_ok=False)
    d = args.out / f'pair-00-{args.label}'; d.mkdir(); (d/'data').mkdir(); (d/'bin').mkdir()
    (d/'bin/rsnano').symlink_to(args.node)
    cmd = [str(args.client), '--data-dir', str(d/'data'), '--prs', '6', '--no-prio', '--blocks', '45000',
           '--accounts', '45000', '--rate', '2000', '--fork-percentage', '5', '--no-kill']
    if args.epoch_ms:
        cmd += ['--epoch-duration-ms', str(args.epoch_ms)]
    for extra in args.client_arg:
        cmd += extra.split('=', 1) if extra.startswith('--') and '=' in extra else [extra]
    manifest = {'kind': 'fork termination diagnostic, not performance', 'command': cmd, 'free_gib_before': round(free_gib, 2),
                'node_sha256': sha256(args.node), 'client_sha256': sha256(args.client),
                'controller_sha256': sha256(Path(__file__)), 'deadline_seconds': args.deadline,
                'deadline_basis': 'bounded diagnostic uses prior baseline-derived 156s ceiling; no matching fork calibration',
                'trust': 'locally accepted experimental checkpoint, not independently verified decision proof',
                'termination_rule': 'both branch hashes included or conflict-discarded on all six nodes with a common checkpoint',
                'sampling': 'poll installed epoch/root ~2s plus RPC time; reuse contents only for matching roots; latency is an observation upper bound'}
    (args.out/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    started = time.time(); checkpoints = {}; seen_epochs = {}; publications = {}; generated = {}; checkpoint_cache = {}; latest_installed = {}
    first_terminal = {}; terminal_witnesses = {}; load_complete = False; position = 0; forced = False; sequence = 0
    result = {'kind': manifest['kind'], 'started': started, 'complete': False}
    with (d/'run.log').open('w') as log, (d/'observations.jsonl').open('w') as observations:
        process = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
                    env=os.environ | {'PATH': str(d/'bin')+os.pathsep+os.environ['PATH'],
                                      'RUST_LOG': 'nanospam=info', 'NANO_LOG': 'noansi'})
        try:
            while time.time()-started < args.deadline:
                with (d/'run.log').open() as stream:
                    stream.seek(position)
                    for line in stream:
                        for marker, dest in [('RAI_FORK_CREATED ', generated), ('RAI_FORK_PUBLISHED ', publications)]:
                            if marker in line:
                                item = json.loads(line.split(marker, 1)[1]); dest[item['primary']] = item
                        if 'RAI_INPUT_COMPLETE created=45000' in line: load_complete = True
                    position = stream.tell()
                force = not forced and time.time()-started > args.deadline-25
                for node in range(6):
                    if time.time()-started >= args.deadline: break
                    try:
                        state = rpc(node, {'action': 'epoch_locks'})
                        epoch = state.get('epoch')
                        key = checkpoint_key(state)
                        if latest_installed.get(node) != key:
                            observations.write(json.dumps({'node': node, 'observed_unix_ms': int(time.time()*1000),
                                                            'installed_epoch': epoch, 'installed_state_hash': state.get('state_hash')})+'\n'); observations.flush()
                        latest_installed[node] = key
                        cached = cached_checkpoint(state, checkpoint_cache)
                        if cached is not None:
                            if checkpoints.get(node) is not cached:
                                observations.write(json.dumps({'node': node, 'observed_unix_ms': int(time.time()*1000),
                                                                'installed_checkpoint': state, 'contents_from_matching_root': True})+'\n'); observations.flush()
                            checkpoints[node] = cached
                            seen_epochs[node] = epoch
                        if (epoch is not None and seen_epochs.get(node) != epoch) or force:
                            full = rpc(node, {'action': 'final_state', 'diagnostic': 'true', 'checkpoint_only': 'true'}, timeout=5)
                            diagnostic = full.get('checkpoint_diagnostics')
                            if not diagnostic: raise ValueError('Node lacks diagnostic schema')
                            checkpoints[node] = diagnostic['checkpoint']
                            if diagnostic['checkpoint']:
                                checkpoint_cache[checkpoint_key(diagnostic['checkpoint'])] = diagnostic['checkpoint']
                                latest_installed[node] = checkpoint_key(diagnostic['checkpoint'])
                                seen_epochs[node] = str(diagnostic['checkpoint']['epoch'])
                            else:
                                seen_epochs[node] = epoch
                            file = f'node-{node}-snapshot-{sequence:03}.json'; sequence += 1
                            (d/file).write_text(json.dumps(full)+'\n')
                            observations.write(json.dumps({'node': node, 'observed_unix_ms': int(time.time()*1000),
                                                            'file': file, 'checkpoint_epoch': epoch})+'\n'); observations.flush()
                    except Exception as error:
                        observations.write(json.dumps({'node': node, 'time': time.time(), 'error': str(error)})+'\n')
                if force: forced = True
                rows, _ = evaluate(publications, checkpoints)
                consistent = installed_checkpoints_consistent(latest_installed)
                rows = retain_terminal_witnesses(rows, terminal_witnesses)
                now = int(time.time()*1000)
                for row in rows:
                    if row['terminated_on_all_nodes']:
                        first_terminal.setdefault(row['hash'], now)
                if load_complete and generated and generated.keys() == publications.keys() and consistent and all(r['terminated_on_all_nodes'] for r in rows):
                    result.update(complete=True, stop_reason='all_recorded_fork_branches_terminated'); break
                time.sleep(2)
            result.setdefault('stop_reason', 'diagnostic_deadline')
            snapshots = []
            for node in range(6):
                try:
                    snapshot = {'node': node, 'block_count': rpc(node, {'action': 'block_count'}),
                                'final_state': rpc(node, {'action': 'final_state'}),
                                'epoch_locks': rpc(node, {'action': 'epoch_locks'}, timeout=5)}
                    # Blocks waiting for a parent this node lacks: which parents, for the record
                    try: snapshot['unchecked_keys'] = rpc(node, {'action': 'unchecked_keys', 'key': '0' * 64, 'count': '64'}, timeout=5)
                    except Exception as error: snapshot['unchecked_keys'] = {'error': str(error)}
                    snapshots.append(snapshot)
                except Exception as error: snapshots.append({'node': node, 'error': str(error)})
            (d/'rpc.json').write_text(json.dumps(snapshots, indent=2)+'\n')
            if (d/'data/node-pids').exists():
                (d/'node-pids').write_bytes((d/'data/node-pids').read_bytes())
        finally:
            try:
                stop_run_process_group(process)
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                result['cleanup_error'] = str(error)
            until = time.time()+10
            while occupied_ports() and time.time()<until: time.sleep(.5)
            if occupied_ports(): result['cleanup_error'] = 'Node ports still occupied'
    rows, _ = evaluate(publications, checkpoints)
    consistent = installed_checkpoints_consistent(latest_installed)
    rows = retain_terminal_witnesses(rows, terminal_witnesses)
    for row in rows:
        published = publications[row['primary']]['published_unix_ms']
        row['first_all_node_terminal_observed_unix_ms'] = first_terminal.get(row['hash'])
        row['latency_upper_bound_ms'] = first_terminal[row['hash']]-published if row['hash'] in first_terminal else None
    (d/'fork-manifest.json').write_text(json.dumps({'generated': list(generated.values()), 'published': list(publications.values())}, indent=2)+'\n')
    (d/'fork-outcomes.json').write_text(json.dumps(rows, indent=2)+'\n')
    times = sorted(r['latency_upper_bound_ms'] for r in rows if r['latency_upper_bound_ms'] is not None)
    by_kind = collections.Counter()
    for row in rows:
        for node in row['nodes'].values():
            if node['outcome'] == 'included':
                by_kind['included_' + node['status']] += 1
            elif node['outcome'] == 'safely_discarded':
                by_kind['discarded_by_' + node['witness_origin'] + '_final'] += 1
            else:
                by_kind['unresolved'] += 1
    result.update(wall_secs=time.time()-started, checkpoint_consistent=consistent, input_complete=load_complete,
                  generated_forks=len(generated), published_forks=len(publications), branch_hashes=len(rows),
                  terminal_branch_hashes=sum(r['terminated_on_all_nodes'] for r in rows),
                  unresolved_branch_hashes=sum(not r['terminated_on_all_nodes'] for r in rows),
                  per_node_dispositions=dict(by_kind),
                  finality_semantics='certificate-backed and variant-derived checkpoint finality are counted apart; equal counts are not equivalent outcomes',
                  termination_latency_observation_upper_bounds_ms={str(p): times[math.ceil(len(times)*p/100)-1] if times else None for p in (50,95,99)},
                  latency_population='only observed terminal branch hashes; unresolved are censored, not excluded from completion')
    (d/'result.json').write_text(json.dumps(result, indent=2)+'\n')
    (args.out/'comparison.json').write_text(json.dumps({'verdict': 'TERMINATED' if result['complete'] else 'UNRESOLVED', 'performance_claim': False}, indent=2)+'\n')
    prune_run_data(d, result)
    print(json.dumps(result), flush=True)


if __name__ == '__main__': main()
