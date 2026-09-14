#!/bin/zsh
# usage: prof_run.sh <name> <rate> [extra args]  -- like run_one.sh but also samples PR0 at workload offsets
S=/private/tmp/claude-501/-Users-ruimorais-rsnano-node/ff407da5-1c3b-4bab-ba7a-a1ea97707882/scratchpad
NAME=$1
OUT=$S/runs/$NAME; mkdir -p $OUT
(
  # wait for workload start: PR0 cemented > 500
  while true; do
    C=$(curl -g -s -m 2 -d '{"action":"block_count"}' "http://[::1]:17076/" | python3 -c 'import sys,json; print(int(json.load(sys.stdin).get("cemented",0)))' 2>/dev/null)
    [ -n "$C" ] && [ "$C" -gt 500 ] && break
    sleep 1
  done
  W=$(date +%s); echo "WORKLOAD_START $W" >> $OUT/prof.log
  PID=$(lsof -iTCP:17076 -sTCP:LISTEN -t | head -1); echo "PR0_PID $PID" >> $OUT/prof.log
  for OFF in 4 18 24; do
    while [ $(( $(date +%s) - W )) -lt $OFF ]; do sleep 0.5; done
    echo "SAMPLE off=$OFF t=$(date +%s) cemented=$(curl -g -s -m 2 -d '{"action":"block_count"}' "http://[::1]:17076/")" >> $OUT/prof.log
    sample $PID 4 -mayDie -file $OUT/sample_off$OFF.txt > /dev/null 2>&1
  done
) &
P=$!
shift
$S/run_one.sh $NAME "$@"
kill $P 2>/dev/null
