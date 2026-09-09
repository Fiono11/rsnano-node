import concurrent.futures,gzip,hashlib,json,os,pathlib,shutil,signal,subprocess,sys,time,urllib.request
ROOT=pathlib.Path(__file__).resolve().parents[2]; OUT=pathlib.Path(__file__).resolve().parent
blocks,rate=map(int,sys.argv[1:3]); label=sys.argv[3] if len(sys.argv)>3 else f'b{blocks}-r{rate}'
p=OUT/label;p.mkdir(exist_ok=True);data=p/'data'
if data.exists():shutil.rmtree(data)
cmd=[str(ROOT/'target/release/nanospam'),'--data-dir',str(data),'--prs','6','--no-prio','--accounts',str(blocks),'--blocks',str(blocks),'--rate',str(rate),'--epoch-length',str(blocks//2),'--fork-percentage','5','--audit-output',str(p/'audit.json')]
if len(sys.argv)>4: cmd.extend(['--vote-generator-delay-ms',sys.argv[4]])
node_dir=pathlib.Path(os.environ.get('NANOSPAM_NODE_DIR', str(ROOT/'target/release')))
env=os.environ.copy();env.update(RUST_LOG='info',NANO_LOG='noansi',PATH=str(node_dir)+':'+env['PATH'])
(p/'command.json').write_text(json.dumps({'argv':cmd,'sha256':{n:hashlib.sha256(((node_dir if n=='rsnano' else ROOT/'target/release')/n).read_bytes()).hexdigest() for n in ['nanospam','rsnano']}},indent=2))
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def rpc(i,payload):
 try:return json.load(opener.open(urllib.request.Request(f'http://[::1]:{17076+10*i}',json.dumps(payload).encode()),timeout=4))
 except Exception as e:return {'error':str(e)}
def snap(i):return {'pr':i,'counts':rpc(i,{'action':'block_count'}),'stats':rpc(i,{'action':'stats','type':'counters'})}
started=time.monotonic(); timed_out=False; profiler=None
with (p/'run.log').open('w') as log,(p/'telemetry.jsonl').open('w') as tele:
 proc=subprocess.Popen(cmd,stdout=log,stderr=subprocess.STDOUT,env=env,start_new_session=True)
 try:
  next_poll=started+15
  while proc.poll() is None:
   if time.monotonic()-started>600:
    timed_out=True;os.killpg(proc.pid,signal.SIGTERM);break
   if os.environ.get('NANOSPAM_PROFILE') and profiler is None and time.monotonic()-started >= 23:
    processes=subprocess.run(['ps','-axo','pid,command'],capture_output=True,text=True).stdout
    match=next((line.split()[0] for line in processes.splitlines() if str(data/'pr0') in line and 'rsnano' in line),None)
    if match:
     profiler=subprocess.Popen(['/usr/bin/sample',match,'3','1','-file',str(p/'sample.txt')],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
   if time.monotonic()>=next_poll:
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool: rows=list(pool.map(snap,range(6)))
    ps=subprocess.run(['ps','-axo','pid,ppid,%cpu,rss,etime,command'],capture_output=True,text=True).stdout
    procs=[l for l in ps.splitlines() if str(data) in l or ('rsnano --network test' in l)]
    tele.write(json.dumps({'elapsed':time.monotonic()-started,'nodes':rows,'processes':procs})+'\n');tele.flush()
    print(json.dumps({'run':label,'elapsed':round(time.monotonic()-started,1),'cemented':[r['counts'].get('cemented') for r in rows],'epoch':[r['counts'].get('current_epoch') for r in rows]}),flush=True)
    next_poll=time.monotonic()+15
   time.sleep(.2)
  proc.wait(timeout=10)
 finally:
  try:os.killpg(proc.pid,signal.SIGKILL)
  except ProcessLookupError:pass
  proc.wait()
  if profiler is not None: profiler.wait(timeout=10)
  if data.exists():shutil.rmtree(data)
  (p/'cleanup.json').write_text(json.dumps({'data_deleted':not data.exists(),'process_group_stopped':True,'returncode':proc.returncode,'watchdog_timeout':timed_out}))
summary={'returncode':proc.returncode,'watchdog_timeout':timed_out,'elapsed':time.monotonic()-started}
for tag in ['BENCHMARK_RESULT','PERFORMANCE_WINDOW_TERMINATION_RESULT','TERMINATION_RESULT','CANONICAL_AGREEMENT_RESULT','ELECTION_RESULT']:
 vals=[]
 for line in (p/'run.log').read_text().splitlines():
  if ': '+tag+' ' in line:
   try:vals.append(json.loads(line.split(tag+' ',1)[1]))
   except ValueError:pass
 summary[tag]=vals if tag=='ELECTION_RESULT' else (vals[-1] if vals else None)
r=summary['TERMINATION_RESULT'];b=summary['BENCHMARK_RESULT'];summary['strict_pass']=bool(proc.returncode==0 and r and r['success'] and r['pending_roots']==0 and b and b['published_blocks']==blocks and b['created_blocks']==blocks)
(p/'summary.json').write_text(json.dumps(summary,indent=2))
if (p/'audit.json').exists():
 with (p/'audit.json').open('rb') as src,gzip.open(p/'audit.json.gz','wb') as dst:shutil.copyfileobj(src,dst)
 (p/'audit.json').unlink()
print(json.dumps(summary),flush=True)

sys.exit(0 if summary['strict_pass'] else 1)
