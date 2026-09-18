#!/bin/zsh
# 45k/2000 bps/5% forks; terminates when all PRs have settled all elections identically
set -u
REPO=/Users/ruimorais/rsnano-node
export PATH=$REPO/target/release:$PATH
OUT=$1
SNAP=$1.snap
: > "$SNAP"
DATA=$HOME/NanoSpam
S=/private/tmp/claude-501/-Users-ruimorais-rsnano-node/cd522b07-358f-41f6-82a3-620e4a39b40b/scratchpad

for attempt in {1..12}; do
  busy=$(top -l 2 -o cpu -n 5 -stats cpu,command | awk 'BEGIN{s=0} /^PID|^Processes|^$/{next} { if ($1+0 > 25 && $2 != "top") print }' | tail -5)
  if [ -z "$busy" ]; then break; fi
  echo "machine busy, waiting: $busy"; sleep 10
done

which rsnano
for attempt in 1 2 3; do
  rm -rf "$DATA"; mkdir -p "$DATA"
  START=$(date +%s)
  nanospam --prs 6 --no-prio --blocks 45000 --accounts 45000 --rate 2000 --fork-percentage 5 --no-kill > "$OUT" 2>&1 &
  NSPID=$!
  # publishing is over once nanospam reports no confirmations for five consecutive seconds
  until [ "$(grep -a -c '| 0 cps |' "$OUT")" -ge 5 ]; do sleep 2; done
  PUB_END=$(date +%s)
  confirmed=$(grep -a 'Confirmed' "$OUT" | tail -1 | sed 's/.*Confirmed \([0-9,]*\) blocks.*/\1/' | tr -d ,)
  if [ "${confirmed:-0}" -ge 40000 ]; then break; fi
  # a fork on one of the first blocks of the single funded account stalls nanospam's
  # account graph for good under Kudzu (forked roots never finalize): start over
  echo "attempt $attempt stalled at $confirmed blocks, restarting" | tee -a "$SNAP"
  kill $NSPID 2>/dev/null; pkill -f "rsnano --network test"; sleep 3; pkill -9 -f "rsnano --network test"; pkill -f nanospam; sleep 2
done
echo "publishing done after $((PUB_END-START))s" | tee -a "$SNAP"
python3 $S/settle_check.py 5 600 2>&1 | tee -a "$SNAP"
echo "settle check exit=${pipestatus[1]} at $(( $(date +%s) - START ))s (settle phase $(( $(date +%s) - PUB_END ))s)" | tee -a "$SNAP"

for i in 0 1 2 3 4 5; do
  port=$((17076+10*i))
  echo "=== PR$i block_count ===" >> "$SNAP"
  curl -sg -d '{"action":"block_count"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
  echo "=== PR$i stats counters ===" >> "$SNAP"
  curl -sg -d '{"action":"stats","type":"counters"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
  echo "=== PR$i stats objects ===" >> "$SNAP"
  curl -sg -d '{"action":"stats","type":"objects"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
done
python3 $S/compare_certs.py >> "$SNAP" 2>&1
kill $NSPID 2>/dev/null; pkill -f "rsnano --network test"; sleep 3; pkill -9 -f "rsnano --network test"; pkill -f nanospam
rm -rf "$DATA"
echo "cleanup done" >> "$OUT"
