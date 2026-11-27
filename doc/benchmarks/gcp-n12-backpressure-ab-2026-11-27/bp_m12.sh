#!/bin/bash
# As fiono8199, detached: after bp_ab.sh ends, the matrix build (~/bin-m12b/rai, node e4601219e,
# rsnano a0181c9b) on the same cell and seeds (reps 2 then 1), as a same-time control: does the
# vote storm of the matrix's fork5-grace-reconf runs still happen on tonight's fleet?
# Log ~/runs/bp_ab.log (RUN-DONE lines), status ~/runs/bp_m12.status on every exit path.
status=~/runs/bp_m12.status
rm -f $status
while [ ! -s ~/runs/bp_ab.status ]; do sleep 20; done
exec >>"$HOME/runs/bp_ab.log" 2>&1
set -x
trap 'echo "${result:-failed:aborted}" > $status' EXIT
. ~/.cargo/env
cd ~/rsnano-node || { result=failed:no-repo; exit 1; }
(cd ~ && sha256sum bin-m12b/rai/*)
out=doc/benchmarks/gcp-n12-backpressure-ab-2026-11-27
H=$HOME/hosts-n14.txt
L="--timeout=480:--stall-abort=90:--drain-limit=15:--settle-timeout=600:--fork-grace-stall=180:--hosts=$H"
for k in 2 1; do
  for ip in $(grep -o "^10[.0-9]*" $H); do
    timeout 20 ssh -n -o BatchMode=yes -o ConnectTimeout=8 $ip "pkill -9 -x rsnano; pkill -9 -f '[r]snano-byzantine'; true" \
      >/dev/null 2>&1 || { result=failed:seat-down-$ip; exit 1; }
  done
  python3 tools/rai/run_sweep.py $out --set final-wan-2000-n12 --variant fork5-grace-reconf \
    --arm "matrix=$HOME/bin-m12b/rai:$L" --reps 3 --rep $k --blocks 45000 --reps-from 1000 --wait-quiet 120 \
    --snapshot-dir ~/ledgers-m12b
  echo "RUN-DONE fork5-grace-reconf matrix $k rc=$? $(date -u +%T)"
done
result=done
