#!/bin/zsh
# Usage: run_one.sh <label>  -- one nanospam RAI run, log kept, data dir deleted afterwards.
set -u
LABEL=${1:-run}
OUT=/private/tmp/claude-501/-Users-ruimorais-rsnano-node/fab80b18-4aae-406a-84b7-d03ede14bfa6/scratchpad/bench
BIN=${BIN:-/Users/ruimorais/rsnano-node/target/release}
export PATH=$BIN:$PATH
DATA=/tmp/nanospam-rai-$LABEL-$$
# Quiet-machine gate: three consecutive top samples (2 s apart, first discarded)
# with no non-node process above 15% CPU.
quiet=0
for attempt in {1..60}; do
  busy=$(top -l 2 -s 2 -o cpu -n 6 -stats cpu,command | awk 'BEGIN{h=0} /^%CPU/{h++; next} h==2 && $1+0>15 && $0!~/rsnano|nanospam|top/ {print}')
  if [[ -z "$busy" ]]; then quiet=$((quiet+1)); else quiet=0; echo "waiting for quiet machine: $busy"; fi
  if (( quiet >= 3 )); then break; fi
done
mkdir -p "$DATA"; rm -rf "$OUT/$LABEL-trace"; mkdir -p "$OUT/$LABEL-trace"
echo "START $(date +%s) label=$LABEL data=$DATA"
RAI_CLOSE_TRACE_DIR=${TRACE:-$OUT/$LABEL-trace} nanospam --prs 6 --no-prio --fork-percentage 5 --blocks ${BLOCKS:-50000} --accounts ${BLOCKS:-50000} --rate ${RATE:-2000} \
  --epoch-terminated-elections ${PER_EPOCH:-25000} --closed-epochs ${EPOCHS:-2} --close-timeout 240 --data-dir "$DATA" \
  > "$OUT/$LABEL.log" 2>&1
echo "EXIT $? $(date +%s)"
pkill -x rsnano 2>/dev/null
sleep 2
rm -rf "$DATA"
grep -c "EPOCH_CLOSED" "$OUT/$LABEL.log"
