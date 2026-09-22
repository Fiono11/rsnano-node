#!/bin/zsh
# One benchmark run: 45k blocks, 2000 bps, 6 PRs, three epochs of 8 s. Publishing
# ends when nanospam reports no confirmation for fifteen consecutive seconds;
# then the settle, committee and safety checks run over RPC.
#   run_variant.sh <out.log> [extra nanospam args...]
# FORKS: fork percentage (5). NODES: representatives running a node (6; 5 with
# --byzantine 1 or --offline 1), which the checkers query.
# Exit: 0 ok, 3 nanospam aborted, 4 checks failed (nodes kept up), 6 stalled.
set -u
REPO=/Users/ruimorais/rsnano-node
# NODE_BIN: the directory the node binary comes from (a worktree build, to
# compare against an older commit). nanospam always comes from this tree.
export PATH=${NODE_BIN:-$REPO/target/release}:$REPO/target/release:$PATH
OUT=$1
shift
SNAP=$OUT.snap
NODES=${NODES:-6}
FORKS=${FORKS:-5}
export PRS=$NODES
LAST=$((NODES - 1))
# Attempts before giving up on a run whose confirmations stall below 40k: a
# fork on one of the first blocks freezes nanospam's account graph. One for a
# run with faulty representatives, where a stall is the result.
RESTARTS=${RESTARTS:-3}
SETTLE_DEADLINE=${SETTLE_DEADLINE:-240}
: > "$SNAP"
DATA=$HOME/NanoSpam
T=$REPO/doc/benchmarks/rai-paper-2026-09-22/tools

teardown() {
  kill ${NSPID:-0} 2>/dev/null
  pkill -f "rsnano --network test" 2>/dev/null; sleep 3
  pkill -9 -f "rsnano --network test" 2>/dev/null
  pkill -f nanospam 2>/dev/null; pkill -f caffeinate 2>/dev/null
  rm -rf "$DATA"
}

for attempt in {1..60}; do
  busy=$(top -l 2 -o cpu -n 5 -stats cpu,command | awk '/^PID|^Processes|^$|^[0-9]{4}\//{next} { if ($1+0 > 25 && $2 != "top") print }' | tail -5)
  if [ -z "$busy" ]; then break; fi
  echo "machine busy, waiting: $busy"; sleep 10
done

which rsnano
stalled=0
for attempt in $(seq 1 $RESTARTS); do
  rm -rf "$DATA"; mkdir -p "$DATA"
  START=$(date +%s)
  caffeinate -i nanospam --prs 6 --no-prio --blocks 45000 --accounts 45000 --rate 2000 --fork-percentage $FORKS --epoch-duration-ms 8000 --no-kill "$@" > "$OUT" 2>&1 &
  NSPID=$!
  zeros() { local n=$(grep -a -c "cps |" "$OUT"); [ "$n" -ge 15 ] && [ "$(grep -a "cps |" "$OUT" | tail -15 | grep -a -c '| 0 cps |')" -ge 15 ]; }
  until zeros; do
    if grep -a -q "never saw the full quorum\|^Error" "$OUT"; then
      echo "nanospam aborted: $(grep -a 'never saw\|^Error' "$OUT" | head -1 | cut -c1-200)" | tee -a "$SNAP"
      teardown
      exit 3
    fi
    if ! kill -0 $NSPID 2>/dev/null && ! grep -a -q "Confirmation rate" "$OUT"; then
      echo "nanospam died: $(tail -1 "$OUT" | cut -c1-200)" | tee -a "$SNAP"
      teardown
      exit 3
    fi
    sleep 2
  done
  PUB_END=$(date +%s)
  confirmed=$(grep -a 'Confirmed' "$OUT" | tail -1 | sed 's/.*Confirmed \([0-9,]*\) blocks.*/\1/' | tr -d ,)
  if [ "${confirmed:-0}" -ge 40000 ]; then stalled=0; break; fi
  stalled=1
  echo "attempt $attempt stalled at $confirmed blocks" | tee -a "$SNAP"
  if [ "$attempt" -lt "$RESTARTS" ]; then teardown; sleep 2; fi
done
echo "publishing done after $((PUB_END-START))s" | tee -a "$SNAP"
if [ "$stalled" = 1 ]; then
  # keep the nodes up for a post-mortem over RPC
  for i in $(seq 0 $LAST); do
    port=$((17076+10*i))
    echo "=== PR$i confirmation_active ===" >> "$SNAP"
    curl -sg -m 5 -d '{"action":"confirmation_active"}' "http://[::1]:$port" | cut -c1-3000 >> "$SNAP"; echo >> "$SNAP"
    echo "=== PR$i final_state ===" >> "$SNAP"
    curl -sg -m 5 -d '{"action":"final_state"}' "http://[::1]:$port" | cut -c1-3000 >> "$SNAP"; echo >> "$SNAP"
  done
  echo "stalled: nodes kept running" | tee -a "$SNAP"
  exit 6
fi
python3 $T/settle_check.py 5 $SETTLE_DEADLINE 2>&1 | tee -a "$SNAP"
SETTLE_EXIT=${pipestatus[1]}
echo "settle check exit=$SETTLE_EXIT at $(( $(date +%s) - START ))s (settle phase $(( $(date +%s) - PUB_END ))s)" | tee -a "$SNAP"

for i in $(seq 0 $LAST); do
  port=$((17076+10*i))
  echo "=== PR$i block_count ===" >> "$SNAP"
  curl -sg -d '{"action":"block_count"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
  echo "=== PR$i stats counters ===" >> "$SNAP"
  curl -sg -d '{"action":"stats","type":"counters"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
  echo "=== PR$i stats objects ===" >> "$SNAP"
  curl -sg -d '{"action":"stats","type":"objects"}' "http://[::1]:$port" >> "$SNAP"; echo >> "$SNAP"
done
python3 $T/committee_check.py 2>&1 | tee -a "$SNAP"
COMMITTEE_EXIT=${pipestatus[1]}
python3 $T/safety_check.py "$OUT" 2>&1 | tee -a "$SNAP"
SAFETY_EXIT=${pipestatus[1]}
python3 $T/compare_certs.py >> "$SNAP" 2>&1
if [ "$SETTLE_EXIT" != 0 ] || [ "$COMMITTEE_EXIT" != 0 ] || [ "$SAFETY_EXIT" != 0 ]; then
  echo "checks failed (settle=$SETTLE_EXIT committees=$COMMITTEE_EXIT safety=$SAFETY_EXIT): nodes kept running" | tee -a "$SNAP"
  exit 4
fi
teardown
echo "cleanup done" >> "$OUT"
