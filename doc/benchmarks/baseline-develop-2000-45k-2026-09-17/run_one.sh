#!/bin/zsh
set -u
REPO=/Users/ruimorais/rsnano-node
export PATH=$REPO/target/release:$PATH
OUT=$1
DATA=$HOME/NanoSpam

# quiet-machine gate: read the 2nd sample of top -l 2, block while any non-node process > 25% CPU
for attempt in 1 2 3 4 5 6 7 8 9 10 11 12; do
  busy=$(top -l 2 -o cpu -n 5 -stats cpu,command | awk 'BEGIN{s=0} /^PID|^Processes|^$/{next} { if ($1+0 > 25 && $2 != "top") print }' | tail -5)
  if [ -z "$busy" ]; then break; fi
  echo "machine busy, waiting: $busy"; sleep 10
done

rm -rf "$DATA"; mkdir -p "$DATA"
which rsnano
START=$(date +%s)
nanospam --prs 6 --no-prio --blocks 45000 --accounts 45000 --rate 2000 --no-kill > "$OUT" 2>&1
RC=$?
END=$(date +%s)
echo "nanospam exit=$RC wall=$((END-START))s" | tee -a "$OUT"

# post-mortem RPC snapshots
for i in 0 1 2 3 4 5; do
  port=$((17076+10*i))
  echo "=== PR$i block_count ===" >> "$OUT"
  curl -sg -d '{"action":"block_count"}' "http://[::1]:$port" >> "$OUT"; echo >> "$OUT"
  echo "=== PR$i confirmation_active ===" >> "$OUT"
  curl -sg -d '{"action":"confirmation_active"}' "http://[::1]:$port" | head -c 400 >> "$OUT"; echo >> "$OUT"
  echo "=== PR$i stats counters ===" >> "$OUT"
  curl -sg -d '{"action":"stats","type":"counters"}' "http://[::1]:$port" >> "$OUT"; echo >> "$OUT"
  echo "=== PR$i stats objects ===" >> "$OUT"
  curl -sg -d '{"action":"stats","type":"objects"}' "http://[::1]:$port" >> "$OUT"; echo >> "$OUT"
done
du -sh "$DATA"/*/ >> "$OUT" 2>&1
pkill -f 'rsnano --network test' ; sleep 3; pkill -9 -f 'rsnano --network test'
rm -rf "$DATA"
echo "cleanup done" >> "$OUT"
