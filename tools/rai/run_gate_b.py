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
import traceback

from audit_votes import audit, read_logs


def rpc(index, action, **fields):
    # A node in the middle of a close or an install can hold its locks for
    # seconds; the poll waits rather than failing the run.
    connection = http.client.HTTPConnection('::1', 17076 + index * 10, timeout=60)
    try:
        connection.request('POST', '/', json.dumps(dict(action=action, **fields)))
        result = json.loads(connection.getresponse().read())
        if 'error' in result:
            raise RuntimeError(result['error'])
        return result
    finally:
        connection.close()


def poll(nodes, action, log, **fields):
    """One settle-loop poll of every node; a failed call is reported and the
    poll repeated instead of ending the run, since the nodes outlive the
    client and a slow answer is not a stall."""
    while True:
        try:
            return [rpc(i, action, **fields) for i in range(nodes)]
        except Exception as error:
            print(f'poll {action} failed: {error!r}; retrying', file=log, flush=True)
            time.sleep(1)


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
    held = {int(c['count']) for c in counts}
    # "The same cemented state on every validator": a forked position a
    # split vote left retained may stay uncemented, as long as it does so
    # everywhere
    all_cemented = len(cemented) == 1 and len(held) == 1
    closes = [closes_of(s) for s in states]
    decided = [{e for e, c in node.items() if c.get('value')} for node in closes]
    every = set.intersection(*decided) if decided else set()
    any_ = set.union(*decided) if decided else set()
    values = {e: {node[e].get('closed_value') for node in closes if e in node and node[e].get('value')}
              for e in any_}
    agree = all(len(v) == 1 for v in values.values())
    return dict(all_cemented=all_cemented, cemented=sorted(cemented), held=sorted(held),
                uncemented=sorted({int(c['count']) - int(c['cemented']) for c in counts}),
                decided_everywhere=sorted(every), decided_somewhere=sorted(any_),
                values_agree=agree,
                settled=all_cemented and agree and every == any_ and len(every) >= min_closed)


def quantile(histogram, q):
    """The q-quantile of a {ms: count} histogram"""
    items = sorted((int(ms), count) for ms, count in histogram.items())
    total = sum(count for _, count in items)
    if not total:
        return None
    rank, seen = q * total, 0
    for ms, count in items:
        seen += count
        if seen >= rank:
            return ms
    return items[-1][0]


def client_metrics(text):
    """The client's RAI_BENCH_METRICS line: the non-fork goodput and latency
    the paper's tables use, comparable across fork variants"""
    lines = [line.split('RAI_BENCH_METRICS ', 1)[1] for line in text.splitlines() if 'RAI_BENCH_METRICS ' in line]
    if not lines:
        return None
    m = json.loads(lines[-1])
    histogram = m['nonfork_histogram_ms']
    return dict(nonfork_goodput_cps=round(m['nonfork_confirmed'] / m['duration_secs'], 1),
                nonfork_created=m['nonfork_created'], nonfork_confirmed=m['nonfork_confirmed'],
                nonfork_p50_ms=quantile(histogram, .5), nonfork_p95_ms=quantile(histogram, .95),
                nonfork_p99_ms=quantile(histogram, .99),
                fork_created=m['fork_created'], fork_confirmed=m['fork_confirmed'],
                duration_secs=round(m['duration_secs'], 2))


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


def restart_events(text):
    """nanospam's RAI_RESTART_* lines: what each --restart did"""
    events = []
    for kind, pr, rest in re.findall(r'RAI_RESTART_(KILLED|UP|RECONNECTED|SKIPPED|MISSED|FAILED) pr=(\d+)(.*)$', text, re.M):
        events.append(dict(event=kind.lower(), pr=int(pr), detail=rest.strip()))
    return events


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
    parser.add_argument('--bounded-drift', type=int, default=None, help='bounded-weight committees f = p = 1: the six genesis members re-weighted per epoch within this many permille of the equal share')
    parser.add_argument('--weight-shift', type=int, default=0, help='percent of one representative balance moved to the next one every epoch (nanospam --weight-shift-percent)')
    parser.add_argument('--vote-delay-ms', type=int, default=None, help='vote generator batching delay of every node (nanospam --vote-generator-delay-ms; node default 100)')
    parser.add_argument('--byzantine', type=int, default=0, help='representatives played by the client with random votes, no node')
    parser.add_argument('--offline', type=int, default=0, help='representatives funded but never started')
    parser.add_argument('--allow-busy', action='store_true', help='Correctness run only; does not satisfy the performance gate')
    parser.add_argument('--wait-quiet', type=int, default=0, help='seconds to wait for a quiet host before giving up')
    parser.add_argument('--keep', action='store_true', help='leave the nodes running and their data for a post-mortem; print the process group')
    parser.add_argument('--settle-timeout', type=int, default=90, help='seconds to wait for settlement after the client finished or timed out')
    parser.add_argument('--prs', type=int, default=6, help='representatives; more than the 3f + 2p + 1 seats leaves the lightest (ties by key) outside the committee')
    parser.add_argument('--restart', action='append', default=[], help='nanospam --restart PR:at:SECS or PR:close:EPOCH[:ROUND] (SIGKILL, then start again); repeatable')
    parser.add_argument('--restart-down-ms', type=int, default=0, help='how long a killed node stays down')
    parser.add_argument('--signing-sync', choices=['none', 'fsync', 'full'], default='fsync', help='how the signing records reach the disk (nanospam --signing-sync)')
    parser.add_argument('--audit-votes', action='store_true', help='every node logs the votes it signs; the run fails on any equivocation (implied by --restart)')
    args = parser.parse_args()
    args.audit_votes = args.audit_votes or bool(args.restart)
    args.output.mkdir(parents=True, exist_ok=False)
    binary_dir = args.bin_dir.resolve()
    result = dict(gate='B', source_revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                  node_sha256=sha256(binary_dir / 'rsnano'), client_sha256=sha256(binary_dir / 'nanospam'))
    top_args = ['top', '-l', '2', '-n', '10', '-o', 'cpu', '-stats', 'pid,cpu,command'] if os.uname().sysname == 'Darwin' else ['top', '-b', '-n', '2', '-d', '1']
    waited_until = time.monotonic() + args.wait_quiet
    while True:
        host = subprocess.check_output(top_args, text=True)
        busy = busy_processes(host)
        if not busy or args.allow_busy or time.monotonic() >= waited_until:
            break
        time.sleep(10)
    (args.output / 'host.txt').write_text(host)
    result['busy_processes'] = busy
    result['performance_eligible'] = not busy and not args.allow_busy
    if busy and not args.allow_busy:
        result['status'] = 'blocked_busy_host'
        (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2))
        return 7
    nodes = args.prs - args.byzantine - args.offline
    result['nodes'] = nodes
    for i in range(args.prs):
        try:
            rpc(i, 'version')
        except OSError:
            continue
        raise RuntimeError(f'Benchmark RPC port for PR{i} is already in use')
    with tempfile.TemporaryDirectory(prefix='rai-gate-b-') as temporary:
        data = Path(tempfile.mkdtemp(prefix='rai-gate-b-kept-')) if args.keep else Path(temporary)
        command = [str(binary_dir / 'nanospam'), '--data-dir', str(data), '--prs', str(args.prs), '--no-prio',
                   '--blocks', str(args.blocks), '--accounts', str(args.accounts), '--rate', str(args.rate),
                   '--fork-percentage', str(args.fork_percentage), '--epoch-duration-ms', str(args.epoch_ms),
                   '--byzantine', str(args.byzantine), '--offline', str(args.offline), '--no-kill']
        for spec in args.restart:
            command += ['--restart', spec]
        if args.restart:
            command += ['--restart-down-ms', str(args.restart_down_ms)]
        if args.audit_votes:
            command += ['--audit-votes']
        command += ['--signing-sync', args.signing_sync]
        if args.vote_delay_ms is not None:
            command += ['--vote-generator-delay-ms', str(args.vote_delay_ms)]
        if args.weight_shift:
            command += ['--weight-shift-percent', str(args.weight_shift)]
        if args.bounded_drift is not None:
            command += ['--committee-model', 'bounded_weight', '--committee-f', '1', '--committee-p', '1',
                        '--committee-drift', str(args.bounded_drift)]
        elif not args.weighted:
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
                # A fork run settles without every conflicting position
                # finalizing: the client may still be waiting for some of
                # them. Settlement is judged on the nodes either way.
                result['client_finished'] = process.poll() is not None
                if result['client_finished'] and process.returncode:
                    raise RuntimeError(f'nanospam exited with {process.returncode}')
                result['client_finished_s'] = round(args.timeout - (deadline - time.monotonic()), 1)
                deadline = time.monotonic() + args.settle_timeout
                while True:
                    counts = poll(nodes, 'block_count', log)
                    states = poll(nodes, 'final_state', log)
                    settled = settlement(counts, states, args.min_closed)
                    if settled['settled'] or time.monotonic() >= deadline:
                        break
                    time.sleep(1)
                result['settled_after_s'] = round(args.timeout - (deadline - time.monotonic()), 1)
                result['settlement'] = settled
                result['block_counts'] = counts
                result['final_states'] = states
                result['stats'] = [rpc(i, 'stats', type='objects') for i in range(nodes)]
                result['counters'] = [rpc(i, 'stats', type='counters') for i in range(nodes)]
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
                streams = [dict(entries=int(e), symbols=int(n)) for e, n in
                           re.findall(r'^EPOCH_RECONCILED .*complete=\S+ entries=(\d+) total=\d+ symbols=(\d+) dropped=false', text, re.M)
                           if int(n) > 0]
                result['streams'] = dict(count=len(streams),
                                         dropped=len(re.findall(r'^EPOCH_RECONCILED .*dropped=true', text, re.M)),
                                         symbols=sum(x['symbols'] for x in streams),
                                         recovered=sum(x['entries'] for x in streams))
                if result['streams']['recovered']:
                    result['streams']['symbols_per_item'] = round(result['streams']['symbols'] / result['streams']['recovered'], 2)
                result['client'] = client_metrics(text)
                rates = re.findall(r'Confirmation rate: ([\d.]+) cps', text)
                result['confirmation_rate_cps'] = float(rates[-1]) if rates else None
                confirmed = re.findall(r'Confirm(?:ed|ing) ([\d,]+) blocks', text)
                result['confirmed_primary'] = int(confirmed[-1].replace(',', '')) if confirmed else 0
                result['correctness_passed'] = (settled['settled']
                                                and (args.fork_percentage > 0
                                                     or (result['confirmed_primary'] >= args.blocks
                                                         and int(settled['cemented'][0]) >= args.blocks)))
                if args.restart:
                    events = restart_events(text)
                    result['restarts'] = events
                    # Every requested restart must have killed its node and
                    # brought it back; a skipped or missed one tested nothing
                    up = sum(e['event'] == 'up' for e in events)
                    result['restarts_completed'] = up == len(args.restart)
                    result['correctness_passed'] = result['correctness_passed'] and result['restarts_completed']
                if args.audit_votes:
                    vote_audit = audit(read_logs(data))
                    (args.output / 'vote_audit.json').write_text(json.dumps(vote_audit, indent=2) + '\n')
                    result['vote_audit'] = {k: v for k, v in vote_audit.items() if k != 'all_conflicts'}
                    result['correctness_passed'] = result['correctness_passed'] and vote_audit['conflicts'] == 0
                result['status'] = 'settled' if result['correctness_passed'] else 'failed'
                for i in range(nodes):
                    saved = args.output / f'pr{i}'
                    saved.mkdir()
                    for config in (data / f'pr{i}').glob('config-*'):
                        shutil.copy2(config, saved / config.name)
        except Exception as error:
            result['status'] = 'failed'
            result['error'] = repr(error)
            result['traceback'] = traceback.format_exc()
        finally:
            # The record first: cleanup must not lose the reason a run ended.
            (args.output / 'result.json').write_text(json.dumps(result, indent=2, default=list) + '\n')
            # Only this run's process group; never signal unrelated node processes.
            if process is not None and args.keep:
                result['kept'] = dict(pgid=process.pid, data=str(data))
                print(f'KEPT pgid={process.pid} data={data}', flush=True)
            elif process is not None:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=5)
                    time.sleep(1)
                except (ProcessLookupError, PermissionError, subprocess.TimeoutExpired):
                    pass
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
    summary = {k: v for k, v in result.items() if k not in ('stats', 'counters', 'final_states', 'block_counts')}
    print(json.dumps(summary, indent=2, default=list))
    return 0 if result.get('correctness_passed') else 1


if __name__ == '__main__':
    raise SystemExit(main())
