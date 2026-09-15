#!/usr/bin/env python3
"""Run isolated local RAI benchmarks; always remove generated node/account data."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

p = argparse.ArgumentParser()
p.add_argument('name')
p.add_argument('--bin-dir', type=Path, required=True)
p.add_argument('--profile', action='store_true')
p.add_argument('--no-epochs', action='store_true', help='diagnostic control with epoch closure disabled')
p.add_argument('--delay', type=int)
p.add_argument('--block-processor-threads', type=int)
p.add_argument('--vote-processor-threads', type=int)
p.add_argument('--output-dir', type=Path, default=Path.cwd()/'target'/'nanospam-stability-20260915')
a = p.parse_args()
base = a.output_dir.resolve()
base.mkdir(parents=True, exist_ok=True)
out = base / a.name
out.mkdir(exist_ok=False)
bins = a.bin_dir.resolve()
identities = {n: hashlib.sha256((bins/n).read_bytes()).hexdigest()
              for n in ('nanospam', 'rsnano')}
for i in range(6):
    for port in (17075 + 10*i, 17076 + 10*i, 17078 + 10*i):
        with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(('::1', port))
data = Path(tempfile.mkdtemp(prefix=a.name + '-data-', dir=base))
cmd = [str(bins / 'nanospam'), '--prs', '6', '--no-prio', '--blocks', '50000',
       '--accounts', '50000', '--rate', '2000', '--fork-percentage', '5',
       '--data-dir', str(data)]
if not a.no_epochs:
    cmd += ['--epoch-terminated-elections', '25000', '--closed-epochs', '2',
            '--close-timeout', '240']
if a.delay is not None:
    cmd += ['--vote-generator-delay-ms', str(a.delay)]
env = dict(os.environ, PATH=str(bins) + os.pathsep + os.environ['PATH'], RUST_LOG='info', NO_COLOR='1')
for k in list(env):
    if k.startswith('NANOSPAM_'):
        del env[k]
meta = {'command': cmd, 'profile': a.profile, 'binaries': identities,
        'block_processor_threads': a.block_processor_threads,
        'vote_processor_threads': a.vote_processor_threads, 'no_epochs': a.no_epochs}
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
def rpc(body):
    req = urllib.request.Request('http://[::1]:17076/', json.dumps(body).encode(),
                                 {'Content-Type': 'application/json'})
    with opener.open(req, timeout=1) as response:
        return json.load(response)
proc = None
samplers = []
started = time.monotonic()
def interrupted(signum, frame):
    raise KeyboardInterrupt
signal.signal(signal.SIGTERM, interrupted)
try:
    overrides = ''
    for component, count in [('block_processor', a.block_processor_threads),
                             ('vote_processor', a.vote_processor_threads)]:
        if count is not None:
            if count < 1:
                raise ValueError(component + ' threads must be positive')
            overrides += '\n[node.'+component+']\nthreads = '+str(count)+'\n'
    if overrides:
        launcher = data / 'launcher'
        launcher.mkdir()
        script = launcher / 'rsnano'
        script.write_text('#!/usr/bin/env python3\nimport os,sys\nfrom pathlib import Path\n'
            'p=Path(sys.argv[sys.argv.index("--data-path")+1])/"config-node.toml"\n'
            'p.write_text(p.read_text()+' + repr(overrides) + ')\n'
            'os.execv('+repr(str(bins/'rsnano'))+', ["rsnano"]+sys.argv[1:])\n')
        script.chmod(0o755)
        env['PATH'] = str(launcher) + os.pathsep + env['PATH']
    with (out/'run.log').open('w') as log, (out/'nodes.log').open('w') as node_log, (out/'samples.jsonl').open('w') as samples:
        proc = subprocess.Popen(cmd, stdout=log, stderr=node_log, env=env, start_new_session=True)
        profiled = set()
        while proc.poll() is None:
            elapsed = time.monotonic() - started
            if elapsed > 380:
                meta['timeout'] = True
                break
            record = {'elapsed_seconds': elapsed, 'unix_ms': int(time.time()*1000)}
            children = subprocess.run(['pgrep','-P',str(proc.pid)], capture_output=True, text=True).stdout.split()
            if children:
                record['node_cpu'] = subprocess.run(
                    ['ps','-o','pid=,pcpu=','-p',','.join(children)], capture_output=True, text=True).stdout.strip()
            try:
                record['counts'] = rpc({'action':'block_count'})
                record['stats'] = rpc({'action':'stats','type':'counters'})
            except Exception as e:
                record['error'] = str(e)
            samples.write(json.dumps(record)+'\n')
            samples.flush()
            if not (out/'config-node.toml').exists() and 'Starting with' in (out/'run.log').read_text():
                shutil.copyfile(data/'pr0'/'config-node.toml', out/'config-node.toml')
            if a.profile:
                text = (out/'run.log').read_text()
                anchor = re.search(r'EPOCH_SCHEDULE (\{[^\n]+)', text)
                if anchor:
                    offset = time.time() - json.loads(anchor.group(1))['start_unix_ms']/1000
                    for t in (7, 18, 24):
                        if offset >= t and t not in profiled:
                            children = subprocess.check_output(['pgrep','-P',str(proc.pid)], text=True).split()
                            if children:
                                dest = out/f'pr0-{t}s.sample.txt'
                                samplers.append(subprocess.Popen(['/usr/bin/sample',children[0],'1','1','-file',str(dest)],stdout=log,stderr=log))
                            profiled.add(t)
            time.sleep(2)
        meta['exit_code'] = proc.poll()
finally:
    if proc:
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
        # Also reap any children left behind by early startup failure.
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    for sampler in samplers:
        try:
            sampler.wait(timeout=10)
        except subprocess.TimeoutExpired:
            sampler.kill()
            sampler.wait()
    shutil.rmtree(data)
    meta['data_deleted'] = not data.exists()
    meta['nodes_by_pr'] = {}
    for line in (out/'run.log').read_text().splitlines():
        if 'NANOSPAM_NODE ' in line:
            node = json.loads(re.sub(r'\x1b\[[0-9;]*m', '', line.split('NANOSPAM_NODE ', 1)[1]))
            meta['nodes_by_pr'][str(node['pr'])] = node['pid']
    meta['wall_seconds'] = time.monotonic() - started
    (out/'metadata.json').write_text(json.dumps(meta, indent=2)+'\n')
records = {}
ansi = re.compile(r'\x1b\[[0-9;]*m')
for line in (out/'run.log').read_text().splitlines():
    line = ansi.sub('', line)
    for label in ('BENCHMARK_RESULT', 'EPOCH_PERFORMANCE_RESULT', 'EPOCH_CLOSE_RESULT'):
        if label+' ' in line:
            records[label] = json.loads(line.split(label+' ',1)[1])
(out/'results.json').write_text(json.dumps(records, indent=2)+'\n')
print(json.dumps(meta, indent=2))
for row in records.get('EPOCH_PERFORMANCE_RESULT',{}).get('rows',[]):
    print(json.dumps(row))
print(json.dumps(records.get('EPOCH_CLOSE_RESULT')))
raise SystemExit(meta['exit_code'] if meta['exit_code'] is not None else 124)
