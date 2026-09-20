#!/bin/zsh
# 45k/2000 bps/5% forks; terminates when all PRs have settled all elections identically
set -u
REPO=/Users/ruimorais/rsnano-node
export PATH=$REPO/target/release:$PATH
OUT=$1
SNAP=$1.snap
: > "$SNAP"
DATA=$HOME/NanoSpam
S=/Users/ruimorais/rsnano-node/doc/benchmarks/rai-committees-3epochs-8s-2000-45k-2026-09-20

for attempt in {1..12}; do
  busy=$(top -l 2 -o cpu -n 5 -stats cpu,command | awk '/^PID|^Processes|^$|^[0-9]{4}\//{next} { if ($1+0 > 25 && $2 != "top") print }' | tail -5)
  if [ -z "$busy" ]; then break; fi
  echo "machine busy, waiting: $busy"; sleep 10
done

which rsnano
for attempt in 1 2 3; do
  rm -rf "$DATA"; mkdir -p "$DATA"
  START=$(date +%s)
  caffeinate -i nanospam --prs 6 --no-prio --blocks 45000 --accounts 45000 --rate 2000 --fork-percentage 5 --epoch-duration-ms 8000 --no-kill > "$OUT" 2>&1 &
  NSPID=$!
  # publishing is over once nanospam reports no confirmations for fifteen
  # consecutive seconds (an epoch switch may stall confirmations for a few)
  until [ "$(grep -a -c '| 0 cps |' "$OUT")" -ge 15 ]; do
    if grep -a -q "never saw the full quorum\|^Error" "$OUT"; then
      echo "nanospam aborted: $(grep -a 'never saw\|^Error' "$OUT" | head -1 | cut -c1-200)" | tee -a "$SNAP"
      for i in 0 1 2 3 4 5; do
        port=$((17076+10*i))
        echo "=== PR$i confirmation_quorum ===" >> "$SNAP"
        curl -sg -m 5 -d '{"action":"confirmation_quorum","peer_details":"true"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
        echo "=== PR$i confirmation_active ===" >> "$SNAP"
        curl -sg -m 5 -d '{"action":"confirmation_active"}' "http://[::1]:$port" | cut -c1-2000 >> "$SNAP"; echo >> "$SNAP"
      done
      pkill -f "rsnano --network test"; sleep 3; pkill -9 -f "rsnano --network test"; pkill -f nanospam
      rm -rf "$DATA"
      exit 3
    fi
    sleep 2
  done
  PUB_END=$(date +%s)
  confirmed=$(grep -a 'Confirmed' "$OUT" | tail -1 | sed 's/.*Confirmed \([0-9,]*\) blocks.*/\1/' | tr -d ,)
  if [ "${confirmed:-0}" -ge 40000 ]; then break; fi
  # a fork on one of the first blocks of the single funded account stalls nanospam's
  # account graph for good under Kudzu (forked roots never finalize): start over
  echo "attempt $attempt stalled at $confirmed blocks, restarting" | tee -a "$SNAP"
  kill $NSPID 2>/dev/null; pkill -f "rsnano --network test"; sleep 3; pkill -9 -f "rsnano --network test"; pkill -f nanospam; sleep 2
done
echo "publishing done after $((PUB_END-START))s" | tee -a "$SNAP"
python3 $S/settle_check.py 5 240 2>&1 | tee -a "$SNAP"
SETTLE_EXIT=${pipestatus[1]}
echo "settle check exit=$SETTLE_EXIT at $(( $(date +%s) - START ))s (settle phase $(( $(date +%s) - PUB_END ))s)" | tee -a "$SNAP"

for i in 0 1 2 3 4 5; do
  port=$((17076+10*i))
  echo "=== PR$i block_count ===" >> "$SNAP"
  curl -sg -d '{"action":"block_count"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
  echo "=== PR$i stats counters ===" >> "$SNAP"
  curl -sg -d '{"action":"stats","type":"counters"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
  echo "=== PR$i stats objects ===" >> "$SNAP"
  curl -sg -d '{"action":"stats","type":"objects"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
done
python3 $S/committee_check.py 2>&1 | tee -a "$SNAP"
COMMITTEE_EXIT=${pipestatus[1]}
python3 $S/safety_check.py "$OUT" 2>&1 | tee -a "$SNAP"
SAFETY_EXIT=${pipestatus[1]}
python3 $S/compare_certs.py >> "$SNAP" 2>&1
if [ "$SETTLE_EXIT" != 0 ] || [ "$COMMITTEE_EXIT" != 0 ] || [ "$SAFETY_EXIT" != 0 ]; then
  # post-mortem over RPC: the nodes stay up, cleanup by hand (pkill rsnano; rm -rf ~/NanoSpam)
  echo "checks failed (settle=$SETTLE_EXIT committees=$COMMITTEE_EXIT safety=$SAFETY_EXIT): nodes kept running" | tee -a "$SNAP"
  exit 4
fi
kill $NSPID 2>/dev/null; pkill -f "rsnano --network test"; sleep 3; pkill -9 -f "rsnano --network test"; pkill -f nanospam
rm -rf "$DATA"
echo "cleanup done" >> "$OUT"
