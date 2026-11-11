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
from ledger_snapshot import ensure_ledger_snapshot
from remote_hosts import cached_rtt_matrix, fetch_node_files, longest_round_trip_ms, parse_hosts, push_binaries, stop_remote_nodes

# --hosts: node i's address; without it every node is on this machine
HOSTS = []


def rpc(index, action, **fields):
    # A node in the middle of a close or an install can hold its locks for
    # seconds; the poll waits rather than failing the run.
    address = HOSTS[index]['address'] if index < len(HOSTS) else '::1'
    connection = http.client.HTTPConnection(address, 17076 + index * 10, timeout=60)
    try:
        connection.request('POST', '/', json.dumps(dict(action=action, **fields)))
        result = json.loads(connection.getresponse().read())
        if 'error' in result:
            raise RuntimeError(result['error'])
        return result
    finally:
        connection.close()


def poll(nodes, action, log, deadline=None, **fields):
    """One settle-loop poll of every node; a failed call is reported and the
    poll repeated instead of ending the run, since the nodes outlive the
    client and a slow answer is not a stall. Past the deadline it gives up
    and returns None, so that a node that never answers cannot hold the
    harness forever."""
    while True:
        try:
            return [rpc(i, action, **fields) for i in range(nodes)]
        except Exception as error:
            print(f'poll {action} failed: {error!r}; retrying', file=log, flush=True)
            if deadline is not None and time.monotonic() >= deadline:
                return None
            time.sleep(1)


def rpc_or(index, default, action, **fields):
    """A post-mortem RPC that must not hang the run: one attempt."""
    try:
        return rpc(index, action, **fields)
    except Exception:
        return default


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
    # Decided: the close finalized a value here. `value` is the candidate
    # this node derived or validated itself, absent on a node that took the
    # decided state from another replica (checkpoint catch-up)
    decided = [{e for e, c in node.items() if c.get('closed_value')} for node in closes]
    every = set.intersection(*decided) if decided else set()
    any_ = set.union(*decided) if decided else set()
    values = {e: {node[e].get('closed_value') for node in closes if e in node and node[e].get('closed_value')}
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
                duration_secs=round(m['duration_secs'], 2),
                nonfork_histogram_ms=histogram,
                # per second since the first publication: [second, published,
                # nonfork confirmed, p50 ms, p95 ms]; the start as unix ms, so
                # that the epoch events of the nodes can be placed on the axis
                series_start_unix_ms=m.get('series_start_unix_ms'),
                series_columns=m.get('series_columns'),
                nonfork_series=m.get('nonfork_series'),
                # [[publish second, [[latency ms, count], ...]], ...]: split by
                # whether a close was in progress when the block was published
                nonfork_by_publish=m.get('nonfork_by_publish'))


def epoch_events(text):
    """Every node's epoch lifecycle events with their unix ms, for the
    time axis of the evaluation figures"""
    events = []
    for kind, epoch, rest, t in re.findall(
            r'^EPOCH_(ENDED|ADVANCED|CLOSED|INSTALL|GATE) epoch=(\d+)(.*?) t=(\d+)$', text, re.M):
        node = re.search(r'node=(\S+)', rest)
        events.append(dict(kind=kind.lower(), epoch=int(epoch), node=node[1] if node else None, t_unix_ms=int(t)))
    return events


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


def client_confirmed(path):
    """The client's latest 'Confirmed N blocks' count, read from the log's tail"""
    with open(path, 'rb') as f:
        f.seek(0, os.SEEK_END)
        f.seek(max(0, f.tell() - 65536))
        tail = f.read().decode(errors='replace')
    counts = re.findall(r'Confirmed ([\d,]+) blocks \|', tail)
    return int(counts[-1].replace(',', '')) if counts else None


def save_node_records(output, data, nodes):
    """Copies each node's configs and its own diagnostics (also those after
    nanospam ended) into the run record"""
    for i in range(nodes):
        node_dir = data / f'pr{i}'
        saved = output / f'pr{i}'
        saved.mkdir(exist_ok=True)
        for path in [*node_dir.glob('config-*'), node_dir / 'diagnostics.log']:
            if path.exists():
                shutil.copy2(path, saved / path.name)


def restart_events(text):
    """nanospam's RAI_RESTART_* lines: what each --restart did"""
    events = []
    for kind, pr, rest in re.findall(r'RAI_RESTART_(KILLED|UP|RECONNECTED|SKIPPED|MISSED|FAILED) pr=(\d+)(.*)$', text, re.M):
        events.append(dict(event=kind.lower(), pr=int(pr), detail=rest.strip()))
    return events


def interrupt(signum, frame):
    raise KeyboardInterrupt


def main():
    # A run started in the background of a non-interactive shell inherits
    # SIGINT ignored; restore it, and treat SIGTERM alike, so that ending a run
    # still runs the finally block: the record, the node logs, remote cleanup
    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGTERM, interrupt)
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
    parser.add_argument('--min-request-interval-ms', type=int, default=None, help='the earliest an election asks a representative again (nanospam --min-request-interval-ms); default: the longest measured round trip with --hosts, the node default (0) otherwise')
    parser.add_argument('--byzantine', type=int, default=0, help='representatives played by the client with random votes, no node')
    parser.add_argument('--offline', type=int, default=0, help='representatives funded but never started')
    parser.add_argument('--allow-busy', action='store_true', help='Correctness run only; does not satisfy the performance gate')
    parser.add_argument('--wait-quiet', type=int, default=0, help='seconds to wait for a quiet host before giving up')
    parser.add_argument('--keep', action='store_true', help='leave the nodes running and their data for a post-mortem; print the process group')
    parser.add_argument('--settle-timeout', type=int, default=90, help='seconds to wait for settlement after the client finished or timed out')
    parser.add_argument('--stall-abort', type=int, default=45, help='end the client phase or the settle wait after this many seconds without progress (0: never)')
    parser.add_argument('--drain-limit', type=int, default=60, help='end the client phase as stalled once it is still waiting for confirmations this many seconds after the load (blocks / rate after its first progress line) should have ended (0: never)')
    parser.add_argument('--prs', type=int, default=6, help='representatives; more than the 3f + 2p + 1 seats leaves the lightest (ties by key) outside the committee')
    parser.add_argument('--restart', action='append', default=[], help='nanospam --restart PR:at:SECS or PR:close:EPOCH[:ROUND] (SIGKILL, then start again); repeatable')
    parser.add_argument('--restart-down-ms', type=int, default=0, help='how long a killed node stays down')
    parser.add_argument('--committee-f', type=int, default=1, help='f of the equal-weight or bounded-weight committee: N = 3f + 2p + 1 seats')
    parser.add_argument('--committee-p', type=int, default=1, help='p of the equal-weight or bounded-weight committee: N = 3f + 2p + 1 seats')
    parser.add_argument('--no-fast-path', action='store_true', help='RAI_FAST_PATH=0 on every node: no one-round finality, so with --committee-p 0 --committee-seats 6 a block needs five of six votes in two rounds, as in Nano')
    parser.add_argument('--committee-seats', type=int, default=None, help='members of the equal-weight committee, from 3 + 2p + 1 on (nanospam --committee-seats); 6 with --committee-p 0 counts five of six, as Nano')
    parser.add_argument('--standby', type=int, default=0, help='running representatives that start outside the committee with an eighth of a share (nanospam --standby)')
    parser.add_argument('--rotation', default=None, help='committee rotations EPOCH:FROM>TO,...;EPOCH:... (nanospam --rotation)')
    parser.add_argument('--signing-sync', choices=['none', 'fsync', 'full'], default='fsync', help='how the signing records reach the disk (nanospam --signing-sync)')
    parser.add_argument('--audit-votes', action='store_true', help='every node logs the votes it signs; the run fails on any equivocation (implied by --restart)')
    parser.add_argument('--epoch-transition', choices=['pipelined', 'pause'], default='pipelined',
                        help='pipelined (the protocol) or pause (the stop-and-drain baseline: no voting in the next epoch until the previous checkpoint is decided)')
    parser.add_argument('--network-delay-ms', type=int, default=0, help='simulated one-way link latency every node adds to each outbound message')
    parser.add_argument('--seed', type=int, default=None, help='seed of the workload choices (nanospam --seed)')
    parser.add_argument('--fork-grace', type=int, default=0, help='after settling, keep the nodes running until no forked position is open on any node, up to this many seconds')
    parser.add_argument('--fork-grace-stall', type=int, default=45, help='end the fork grace once the open positions have not dropped for this many seconds')
    parser.add_argument('--byzantine-leads-close', action='store_true', help='the first Byzantine representative leads round 0 of every close (nanospam --byzantine-leads-close)')
    parser.add_argument('--byzantine-node', default=None, metavar='VOTES,REPORTS,LEADER',
                        help='the last running representative is a Byzantine member running a full node with these behaviours '
                             '(nanospam --byzantine-node); its binary is BIN_DIR/rsnano-byzantine, a rai_byzantine build')
    parser.add_argument('--no-epochs', action='store_true',
                        help='the unmodified-node baseline: no epochs, no checkpoints, no committee model; settled means identical cemented state')
    parser.add_argument('--ledger-snapshot', type=Path, default=None,
                        help='the funded ledgers to start from: saved in this directory by a setup run if it is empty')
    parser.add_argument('--hosts', type=Path, default=None,
                        help='one line per running representative, ADDRESS or ADDRESS local (nanospam --hosts): '
                             'remote nodes are started over ssh, the binaries copied to ~/rai-bin/<build> on their machines')
    args = parser.parse_args()
    # The unmodified-node baseline runs no epochs; its rotations are timed by
    # the epoch length the RAI arms use
    rotation_epoch_ms = args.epoch_ms
    if args.no_epochs:
        args.epoch_ms, args.min_closed, args.weighted = 0, 0, True
    args.audit_votes = args.audit_votes or bool(args.restart)
    args.output.mkdir(parents=True, exist_ok=False)
    binary_dir = args.bin_dir.resolve()
    result = dict(gate='B', source_revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                  node_sha256=sha256(binary_dir / 'rsnano'), client_sha256=sha256(binary_dir / 'nanospam'))
    nodes = args.prs - args.byzantine - args.offline
    if args.hosts:
        HOSTS.extend(parse_hosts(args.hosts.read_text()))
        if len(HOSTS) < nodes:
            raise SystemExit(f'--hosts lists {len(HOSTS)} nodes, the run has {nodes} running representatives')
        del HOSTS[nodes:]
        # Named after the build: every machine runs exactly the node binary here
        remote_bin_dir = f"rai-bin/{result['node_sha256'][:16]}"
        node_binaries = ['rsnano'] + (['rsnano-byzantine'] if args.byzantine_node else [])
        push_binaries(HOSTS, binary_dir, node_binaries, remote_bin_dir)
        result['hosts'] = HOSTS
        result['rtt_ms'] = cached_rtt_matrix(args.hosts, len(HOSTS))
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
        if args.epoch_transition != 'pipelined':
            command += ['--epoch-transition', args.epoch_transition]
        if args.network_delay_ms:
            command += ['--network-delay-ms', str(args.network_delay_ms)]
        if args.seed is not None:
            command += ['--seed', str(args.seed)]
        if args.byzantine_leads_close:
            command += ['--byzantine-leads-close']
        if args.byzantine_node:
            command += ['--byzantine-node', args.byzantine_node,
                        '--byzantine-node-bin', str(binary_dir / 'rsnano-byzantine')]
        if args.standby:
            command += ['--standby', str(args.standby)]
        if args.rotation:
            command += ['--rotation', args.rotation]
            if args.no_epochs:
                command += ['--rotation-epoch-ms', str(rotation_epoch_ms)]
        if args.vote_delay_ms is not None:
            command += ['--vote-generator-delay-ms', str(args.vote_delay_ms)]
        # Across wide-area links no request repeats before its reply could
        # arrive: the dev network's 25 ms intervals assume one host. The
        # unmodified node has no such setting
        min_request_interval = None if args.no_epochs else args.min_request_interval_ms
        if min_request_interval is None and args.hosts and not args.no_epochs:
            min_request_interval = longest_round_trip_ms(result['rtt_ms'])
        if min_request_interval:
            command += ['--min-request-interval-ms', str(min_request_interval)]
        result['min_request_interval_ms'] = min_request_interval or 0
        if args.weight_shift:
            command += ['--weight-shift-percent', str(args.weight_shift)]
        if args.bounded_drift is not None:
            command += ['--committee-model', 'bounded_weight', '--committee-f', str(args.committee_f), '--committee-p', str(args.committee_p),
                        '--committee-drift', str(args.bounded_drift)]
        elif not args.weighted:
            command += ['--committee-model', 'equal_weight', '--committee-f', str(args.committee_f), '--committee-p', str(args.committee_p)]
            if args.committee_seats is not None:
                command += ['--committee-seats', str(args.committee_seats)]
        if args.hosts:
            command += ['--hosts', str(args.hosts.resolve()), '--remote-bin-dir', remote_bin_dir]
        result['command'] = command
        result['status'] = 'running'
        env = dict(os.environ, PATH=str(binary_dir) + os.pathsep + os.environ['PATH'], RUST_LOG='nanospam=info', NANO_LOG='noansi')
        if args.no_fast_path:
            env['RAI_FAST_PATH'] = '0'
        result['fast_path'] = env.get('RAI_FAST_PATH', '1') != '0'
        process = None
        try:
            if args.ledger_snapshot:
                result['ledger_snapshot'] = ensure_ledger_snapshot(
                    args.ledger_snapshot.resolve(), command, env, HOSTS,
                    dict(node_sha256=result['node_sha256'], client_sha256=result['client_sha256'], nodes=nodes, prs=args.prs,
                         accounts=args.accounts),
                    args.output / 'snapshot-setup.log')
                command += ['--ledger-snapshot', str(args.ledger_snapshot.resolve())]
            with (args.output / 'run.log').open('w') as log:
                process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                deadline = time.monotonic() + args.timeout
                # A stalled run is ended, not waited out: no confirmation for
                # --stall-abort seconds once the spam has started
                last_count, last_progress = None, time.monotonic()
                # A trickle of confirmations is not progress: once the load
                # has had its time (blocks / rate from its first progress
                # line), the client gets --drain-limit seconds more to see
                # every confirmation, as healthy runs do within ~20 s
                drain_deadline = None
                while process.poll() is None and time.monotonic() < deadline:
                    time.sleep(1)
                    count = client_confirmed(args.output / 'run.log')
                    if count is not None and drain_deadline is None and args.drain_limit:
                        drain_deadline = time.monotonic() + args.blocks / args.rate + args.drain_limit
                    if count != last_count:
                        last_count, last_progress = count, time.monotonic()
                    elif (args.stall_abort and count is not None
                          and time.monotonic() - last_progress > args.stall_abort):
                        result['stalled'] = dict(phase='client', confirmed=count)
                        print(f'stall: client at {count} confirmed for {args.stall_abort}s', file=log, flush=True)
                        break
                    if drain_deadline is not None and time.monotonic() > drain_deadline:
                        result['stalled'] = dict(phase='client', confirmed=count, reason='drain limit')
                        print(f'stall: client at {count} confirmed {args.drain_limit}s after the load', file=log, flush=True)
                        break
                # A fork run settles without every conflicting position
                # finalizing: the client may still be waiting for some of
                # them. Settlement is judged on the nodes either way.
                result['client_finished'] = process.poll() is not None
                if result['client_finished'] and process.returncode:
                    raise RuntimeError(f'nanospam exited with {process.returncode}')
                result['client_finished_s'] = round(args.timeout - (deadline - time.monotonic()), 1)
                settle_start = time.monotonic()
                deadline = settle_start + args.settle_timeout
                last_view, last_progress = None, time.monotonic()
                hard_deadline = deadline + 120
                open_trace = []
                while True:
                    counts = poll(nodes, 'block_count', log, hard_deadline)
                    # the unmodified node has no epochs and no final_state RPC
                    states = (poll(nodes, 'final_state', log, hard_deadline) if not args.no_epochs
                              else [{} for _ in range(nodes)])
                    if counts is None or states is None:
                        result['stalled'] = dict(phase='settle', reason='nodes stopped answering')
                        raise RuntimeError('nodes stopped answering during the settle wait')
                    settled = settlement(counts, states, args.min_closed)
                    # open positions over wall-clock time, for the fork
                    # resolution figure: [unix ms, most uncemented on a node]
                    open_trace.append([int(time.time() * 1000), max(settled['uncemented'])])
                    if settled['settled'] or time.monotonic() >= deadline:
                        break
                    # Nothing cemented and no epoch decided anywhere for a while
                    view = (tuple(c['cemented'] for c in counts), tuple(settled['decided_somewhere']))
                    if view != last_view:
                        last_view, last_progress = view, time.monotonic()
                    elif args.stall_abort and time.monotonic() - last_progress > args.stall_abort:
                        result['stalled'] = dict(phase='settle', cemented=settled['cemented'],
                                                 decided=settled['decided_somewhere'])
                        break
                    time.sleep(1)
                # seconds since the client phase began, like client_finished_s
                # (the deadline now counts down the settle wait, not --timeout)
                result['settled_after_s'] = round(result['client_finished_s'] + time.monotonic() - settle_start, 1)
                # Forked positions resolve after the load: keep the nodes
                # running until none is open anywhere, until none has resolved
                # for --fork-grace-stall seconds, or until the cap. A run that
                # did not settle has failed already: a trickle of resolutions
                # would keep it waiting for the cap
                if args.fork_grace and counts is not None and settled['settled']:
                    before = settled['uncemented']
                    started = time.monotonic()
                    grace_end = started + args.fork_grace
                    lowest, dropped_at, outcome = max(before), started, 'cap'
                    trace = [[0.0, lowest]]
                    resolved_at = None
                    while True:
                        # Resolved; the replicas still deciding an epoch
                        # (a lagging one catching up) get up to
                        # --fork-grace-stall seconds more to agree
                        if max(settled['uncemented']) == 0:
                            outcome = 'resolved'
                            resolved_at = resolved_at or time.monotonic()
                            if settled['settled'] or time.monotonic() - resolved_at > args.fork_grace_stall:
                                break
                        elif time.monotonic() - dropped_at > args.fork_grace_stall:
                            outcome = 'stalled'
                            break
                        if time.monotonic() >= grace_end:
                            break
                        time.sleep(2)
                        polled = poll(nodes, 'block_count', log, grace_end + 30)
                        polled_states = (poll(nodes, 'final_state', log, grace_end + 30) if not args.no_epochs
                                         else [{} for _ in range(nodes)])
                        if polled is None or polled_states is None:
                            outcome = 'nodes_stopped_answering'
                            break
                        counts, states = polled, polled_states
                        settled = settlement(counts, states, args.min_closed)
                        open_now = max(settled['uncemented'])
                        open_trace.append([int(time.time() * 1000), open_now])
                        if open_now < lowest:
                            lowest, dropped_at = open_now, time.monotonic()
                            trace.append([round(dropped_at - started, 1), open_now])
                    result['fork_grace'] = dict(seconds=args.fork_grace, stall_s=args.fork_grace_stall,
                                                outcome=outcome, uncemented_before=before,
                                                uncemented_after=settled['uncemented'],
                                                last_drop_s=round(dropped_at - started, 1),
                                                waited_s=round(time.monotonic() - started, 1),
                                                trace=trace)
                result['open_trace'] = open_trace
                result['settlement'] = settled
                result['block_counts'] = counts
                result['final_states'] = states
                result['stats'] = [rpc_or(i, {}, 'stats', type='objects') for i in range(nodes)]
                result['counters'] = [rpc_or(i, {}, 'stats', type='counters') for i in range(nodes)]
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
                result['epoch_events'] = epoch_events(text)
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
                # A running node keeps appending to the logs being copied
                if HOSTS and not args.keep:
                    stop_remote_nodes(HOSTS, data, remove=False)
                fetch_node_files(HOSTS, data, ['diagnostics.log', 'signed-votes.log'])
                if args.audit_votes:
                    vote_audit = audit(read_logs(data))
                    (args.output / 'vote_audit.json').write_text(json.dumps(vote_audit, indent=2) + '\n')
                    result['vote_audit'] = {k: v for k, v in vote_audit.items() if k != 'all_conflicts'}
                    result['correctness_passed'] = result['correctness_passed'] and vote_audit['conflicts'] == 0
                result['status'] = 'settled' if result['correctness_passed'] else 'failed'
        except Exception as error:
            result['status'] = 'failed'
            result['error'] = repr(error)
            result['traceback'] = traceback.format_exc()
        finally:
            # The record first: cleanup must not lose the reason a run ended.
            (args.output / 'result.json').write_text(json.dumps(result, indent=2, default=list) + '\n')
            # Every node's config and diagnostics, of a failed or stalled run
            # too: a remote node's are fetched before its data is deleted, and
            # once it stopped, since it keeps appending to them
            if HOSTS and not args.keep:
                stop_remote_nodes(HOSTS, data, remove=False)
            fetch_node_files(HOSTS, data, ['diagnostics.log'])
            save_node_records(args.output, data, nodes)
            # Only this run's process group; never signal unrelated node processes.
            if process is not None and args.keep:
                result['kept'] = dict(pgid=process.pid, data=str(data))
                print(f'KEPT pgid={process.pid} data={data}', flush=True)
                if HOSTS:
                    print(f'KEPT remote nodes: same data path on {sorted({h["address"] for h in HOSTS if h["remote"]})}', flush=True)
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
            # Remote nodes outlive their ssh clients; they and their data go
            # unless kept for a post-mortem
            if HOSTS and not args.keep:
                stop_remote_nodes(HOSTS, data, remove=True)
    summary = {k: v for k, v in result.items() if k not in ('stats', 'counters', 'final_states', 'block_counts', 'epoch_events')}
    if summary.get('client'):
        summary['client'] = {k: v for k, v in summary['client'].items()
                             if k not in ('nonfork_series', 'nonfork_histogram_ms', 'series_columns')}
    print(json.dumps(summary, indent=2, default=list))
    return 0 if result.get('correctness_passed') else 1


if __name__ == '__main__':
    raise SystemExit(main())
