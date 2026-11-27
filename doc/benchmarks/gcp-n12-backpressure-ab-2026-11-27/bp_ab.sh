#!/bin/bash
# As fiono8199 on the controller, detached: A/B of solicitation backpressure + inbound close lane
# at twelve seats across six regions with the committee change and 5% forks
# (final-wan-2000-n12, variant fork5-grace-reconf: 3 of 3 matrix runs had vote storms, closes 154-202 s).
#   base = ~/bin-bpbase/rai (rai_dsn_anon 5d5709827), fix = ~/bin-bpfix/rai (rai_backpressure 15c7f4693)
# Reps 1-2 on the matrix seeds, interleaved base/fix, the matrix's harness limits.
# Log ~/runs/bp_ab.log (a RUN-DONE line per run), status ~/runs/bp_ab.status on every exit path.
exec >>"$HOME/runs/bp_ab.log" 2>&1
set -x
status=~/runs/bp_ab.status
rm -f $status
trap 'echo "${result:-failed:aborted}" > $status' EXIT
. ~/.cargo/env
cd ~/rsnano-node || { result=failed:no-repo; exit 1; }
for b in bpbase bpfix; do [ -x ~/bin-$b/rai/rsnano ] || { result=failed:no-$b-build; exit 1; }; done
(cd ~ && sha256sum bin-bpbase/rai/* bin-bpfix/rai/*)
out=doc/benchmarks/gcp-n12-backpressure-ab-2026-11-27
H=$HOME/hosts-n14.txt
L="--timeout=480:--stall-abort=90:--drain-limit=15:--settle-timeout=600:--fork-grace-stall=180:--hosts=$H"
seats_ready() {
  local ips=$(grep -o "^10[.0-9]*" $H) t0=$(date +%s) down
  while :; do
    down=""
    for ip in $ips; do
      timeout 20 ssh -n -o BatchMode=yes -o ConnectTimeout=8 $ip "pkill -9 -x rsnano; pkill -9 -f '[r]snano-byzantine'; true" \
        >/dev/null 2>&1 || down="$down $ip"
    done
    [ -z "$down" ] && return 0
    echo "SEATS-DOWN$down $(date -u +%T)"
    [ $(( $(date +%s) - t0 )) -gt 900 ] && return 1
    sleep 30
  done
}
for k in ${REPS:-1 2}; do
  for arm in base fix; do
    case $arm in
      base) a="base=$HOME/bin-bpbase/rai:$L"; snap=~/ledgers-bpbase ;;
      fix)  a="fix=$HOME/bin-bpfix/rai:$L";   snap=~/ledgers-bpfix ;;
    esac
    seats_ready || { result=failed:seats-down; exit 1; }
    python3 tools/rai/run_sweep.py $out --set final-wan-2000-n12 --variant fork5-grace-reconf --arm "$a" \
      --reps 3 --rep $k --blocks 45000 --reps-from 1000 --wait-quiet 120 --snapshot-dir $snap
    echo "RUN-DONE fork5-grace-reconf $arm $k rc=$? $(date -u +%T)"
  done
done
result=done
