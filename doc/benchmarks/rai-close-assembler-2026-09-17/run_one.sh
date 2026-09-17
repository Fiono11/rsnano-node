#!/bin/sh
# usage: run_one.sh <bindir> <label>
BIN="$1"; LABEL="$2"
S=/private/tmp/claude-501/-Users-ruimorais-rsnano-node/f57d2fe1-1cc0-4da0-a2aa-763f099dee0e/scratchpad
DATA="$S/data-$LABEL"
LOG="$S/bench/$LABEL.log"
quiet=0
while [ $quiet -lt 3 ]; do
  busy=$(top -l 2 -o cpu -n 5 -stats command,cpu | tail -5 | awk '$NF+0 > 25 && $1 != "rsnano" {print}')
  if [ -z "$busy" ]; then quiet=$((quiet+1)); else quiet=0; echo "busy: $busy"; sleep 5; fi
done
rm -rf "$DATA"; mkdir -p "$DATA"
echo "RUN $LABEL bin=$BIN start=$(date -u +%FT%TZ)" > "$LOG"
PATH="$BIN:$PATH" "$BIN/nanospam" --prs 6 --no-prio --fork-percentage 5 --blocks 45000 --accounts 45000 --rate 2000 \
  --epoch-terminated-elections 15000 --closed-epochs 3 --close-timeout 240 --no-kill --data-dir "$DATA" >> "$LOG" 2>&1
echo "RUN $LABEL exit=$? end=$(date -u +%FT%TZ)" >> "$LOG"
for i in 0 1 2 3 4 5; do
  port=$((17076 + 10 * i))
  curl -sg -m 5 -d '{"action":"stats","type":"counters"}' "http://[::1]:$port" > "$S/bench/$LABEL-stats-pr$i.json" 2>/dev/null
done
pkill -f "$DATA" 2>/dev/null; sleep 2; pkill -9 -f "$DATA" 2>/dev/null
rm -rf "$DATA"
grep -E "BENCHMARK_RESULT|RUN " "$LOG" | cut -c1-300
