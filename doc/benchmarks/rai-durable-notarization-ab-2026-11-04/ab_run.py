import json,subprocess,sys,time
from pathlib import Path
W=Path('/private/tmp/claude-501/rai-wan/ab'); OUT=Path('doc/benchmarks/rai-durable-notarization-ab-2026-11-04')
CELLS=[('nofork',2000,[]),
       ('fork10-grace',2000,['--fork-percentage','10','--fork-grace','600']),
       ('fork10-grace-byznode',1000,['--fork-percentage','10','--fork-grace','600','--byzantine-node','equivocate,conflicting,equivocate','--byzantine-leads-close'])]
REPS=5
for k in range(REPS):
  for cell,rate,flags in CELLS:
    seed=1000*rate+sum(map(ord,cell.removesuffix('-byznode')))+k
    arms=['base','durable'] if k%2==0 else ['durable','base']
    for arm in arms:
      run=OUT/cell/f'r{rate}'/f'{arm}-rep{k+1}'
      s=run.with_suffix('.summary.json')
      if s.exists(): continue
      run.parent.mkdir(parents=True,exist_ok=True)
      cmd=[sys.executable,'tools/rai/run_gate_b.py',str(run),'--bin-dir',str(W/arm),'--wait-quiet','1800','--timeout','400','--settle-timeout','120','--stall-abort','45','--rate',str(rate),'--blocks','45000','--accounts','45000','--seed',str(seed),'--epoch-ms=8000','--min-closed=3']+flags
      t=time.time(); p=subprocess.run(cmd,capture_output=True,text=True)
      s.write_text(p.stdout)
      try: st=json.loads(p.stdout).get('status')
      except Exception: st=f'exit {p.returncode}'
      print(time.strftime('%H:%M:%S'),cell,arm,k+1,st,round(time.time()-t),flush=True)
