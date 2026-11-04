"""Runs across machines: the --hosts file shared with nanospam, and the
ssh/scp steps the harness takes around a run.

The hosts file has one line per running representative: `ADDRESS`, a node
nanospam starts over ssh, or `ADDRESS local`, a node on this machine that the
others reach at ADDRESS. Blank lines and `#` comments are skipped. A node's
data directory has the same path on its machine as here."""
import ipaddress
import math
import re
import shlex
import subprocess
from pathlib import Path

SSH_OPTIONS = ['-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=accept-new', '-o', 'ConnectTimeout=10']


def parse_hosts(text):
    """[dict(address, remote)] in node order"""
    hosts = []
    for number, line in enumerate(text.splitlines(), 1):
        fields = line.split('#', 1)[0].split()
        if not fields:
            continue
        try:
            ipaddress.ip_address(fields[0])
        except ValueError:
            raise ValueError(f'line {number}: {fields[0]} is not an IP address') from None
        if fields[1:] not in ([], ['local']):
            raise ValueError(f'line {number}: expected `ADDRESS` or `ADDRESS local`, not {line.strip()!r}')
        hosts.append(dict(address=fields[0], remote=fields[1:] != ['local']))
    return hosts


def remote_hosts(hosts):
    """The distinct machines nodes are started on over ssh"""
    return sorted({h['address'] for h in hosts if h['remote']})


def ssh(address, command, **kwargs):
    return subprocess.run(['ssh', '-n', *SSH_OPTIONS, address, command], text=True, capture_output=True, **kwargs)


def scp_target(address, path):
    host = f'[{address}]' if ':' in address else address
    return f'{host}:{path}'


def push_binaries(hosts, binary_dir, names, remote_dir):
    """Copies the node binaries to every remote machine once: remote_dir is
    named after the build, so a machine that has it is skipped"""
    for address in remote_hosts(hosts):
        present = ssh(address, ' && '.join(f'test -x {shlex.quote(f"{remote_dir}/{n}")}' for n in names))
        if present.returncode == 0:
            continue
        made = ssh(address, f'mkdir -p {shlex.quote(remote_dir)}')
        if made.returncode:
            raise RuntimeError(f'ssh {address}: {made.stderr.strip()}')
        subprocess.run(['scp', '-q', *SSH_OPTIONS, *[str(binary_dir / n) for n in names],
                        scp_target(address, remote_dir + '/')], check=True)


def ping_ms(output):
    """The average round trip of `ping -q`'s summary line, None if lost"""
    match = re.search(r'= [\d.]+/([\d.]+)/', output)
    return float(match[1]) if match else None


def rtt_matrix(hosts):
    """rtt[i][j]: average round trip from PR i's machine to PR j's, in ms"""
    matrix = []
    for source in hosts:
        row = []
        for target in hosts:
            if source['address'] == target['address']:
                row.append(0.0)
                continue
            command = f'ping -q -c 5 -i 0.2 {shlex.quote(target["address"])}'
            if source['remote']:
                result = ssh(source['address'], command, timeout=30)
            else:
                result = subprocess.run(shlex.split(command), text=True, capture_output=True, timeout=30)
            row.append(ping_ms(result.stdout))
        matrix.append(row)
    return matrix


def longest_round_trip_ms(matrix):
    """The longest measured round trip of an rtt_matrix, whole ms rounded up;
    0 when nothing was measured"""
    measured = [rtt for row in matrix for rtt in row if rtt]
    return math.ceil(max(measured)) if measured else 0


def fetch_node_files(hosts, data, names):
    """Copies what remote nodes wrote in their data directories (logs, vote
    records) into the local ones, for the audit and the saved record"""
    for i, host in enumerate(hosts):
        if not host['remote']:
            continue
        local = data / f'pr{i}'
        local.mkdir(parents=True, exist_ok=True)
        for name in names:
            # a file the node never wrote is not an error
            subprocess.run(['scp', '-q', *SSH_OPTIONS, scp_target(host['address'], f'{local}/{name}'), str(local)],
                           capture_output=True)


def fetch_node_dir(address, remote_dir, local_dir):
    """Copies a remote node's whole data directory to local_dir, which must not exist"""
    subprocess.run(['scp', '-rq', *SSH_OPTIONS, scp_target(address, str(remote_dir)), str(local_dir)], check=True)


def stop_remote_nodes(hosts, data, remove):
    """SIGKILLs every remote node of the run by the pid it recorded; with
    remove, deletes the run's data directory on each machine"""
    for i, host in enumerate(hosts):
        if not host['remote']:
            continue
        pid_file = shlex.quote(str(Path(data) / f'pr{i}' / 'node.pid'))
        ssh(host['address'], f'test -f {pid_file} && kill -9 "$(cat {pid_file})" 2>/dev/null; true', timeout=30)
    if remove:
        for address in remote_hosts(hosts):
            ssh(address, f'rm -rf {shlex.quote(str(data))}', timeout=60)
