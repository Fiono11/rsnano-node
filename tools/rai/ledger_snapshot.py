"""Funded ledgers saved once and restored by every run: `nanospam --setup-only`
funds the accounts, and each node's ledger and wallets are kept with the account
keys. A run then starts from the same accounts as every other run on the same
binary, without the funding. The configuration is not saved: it is each run's own."""
import json
import os
import shutil
import signal
import subprocess
import tempfile
import time
from pathlib import Path

from remote_hosts import fetch_node_dir, stop_remote_nodes

MANIFEST = 'manifest.json'
SETUP_TIMEOUT_S = 1800
# What a node writes that is not its ledger or wallets
NOT_SAVED = {'config-node.toml', 'config-rpc.toml', 'diagnostics.log', 'signed-votes.log', 'node.pid', 'run-node.sh'}


def check_manifest(found, wanted):
    """A saved ledger only suits the binary and committee it was set up with"""
    differing = sorted(key for key in wanted if found.get(key) != wanted[key])
    if differing:
        raise RuntimeError(f'the ledger snapshot differs from this run in {differing}: {found} / {wanted}')


def prune_node_dir(node_dir):
    """Leaves the ledger and wallets of a copied node directory"""
    for path in Path(node_dir).iterdir():
        if path.is_dir():
            shutil.rmtree(path)
        elif path.name in NOT_SAVED or path.name.endswith('-lock'):
            path.unlink()


def replaced_data_dir(command, data_dir):
    """The command with another --data-dir"""
    index = command.index('--data-dir') + 1
    return [*command[:index], str(data_dir), *command[index + 1:]]


def ensure_ledger_snapshot(snapshot, command, env, hosts, wanted, log_path):
    """Returns the snapshot's manifest, running the setup first if there is none"""
    manifest = snapshot / MANIFEST
    if manifest.exists():
        found = json.loads(manifest.read_text())
        check_manifest(found, wanted)
        return found
    with tempfile.TemporaryDirectory(prefix='rai-ledger-setup-') as temporary:
        data = Path(temporary) / 'data'
        data.mkdir()
        started = time.monotonic()
        try:
            run_setup(replaced_data_dir(command, data), env, log_path)
            stop_remote_nodes(hosts, data, remove=False)
            saved = Path(temporary) / 'saved'
            saved.mkdir()
            save_nodes(data, saved, hosts, wanted['nodes'])
        finally:
            stop_remote_nodes(hosts, data, remove=True)
        found = dict(wanted, setup_seconds=round(time.monotonic() - started, 1))
        (saved / MANIFEST).write_text(json.dumps(found, indent=2) + '\n')
        snapshot.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(saved, snapshot, dirs_exist_ok=True)
    return found


def run_setup(command, env, log_path):
    """Nanospam leaves its nodes running: they stop with its process group,
    so that nothing writes to the ledgers while they are copied"""
    with Path(log_path).open('w') as log:
        process = subprocess.Popen([*command, '--setup-only'], env=env, stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        try:
            if process.wait(timeout=SETUP_TIMEOUT_S):
                raise RuntimeError(f'the ledger setup exited with {process.returncode}')
        finally:
            for signal_number in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.killpg(process.pid, signal_number)
                except (ProcessLookupError, PermissionError):
                    break
                time.sleep(5)


def save_nodes(data, saved, hosts, node_count):
    shutil.copy(data / 'account_keys.txt', saved / 'account_keys.txt')
    for i in range(node_count):
        target = saved / f'pr{i}'
        if hosts and hosts[i]['remote']:
            fetch_node_dir(hosts[i]['address'], data / f'pr{i}', target)
        else:
            shutil.copytree(data / f'pr{i}', target)
        prune_node_dir(target)
