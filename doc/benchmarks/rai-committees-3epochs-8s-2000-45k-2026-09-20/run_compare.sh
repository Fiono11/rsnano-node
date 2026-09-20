#!/bin/zsh
# One benchmark run of a given node build, for the develop-vs-RAI comparison.
# The same nanospam binary (this branch) drives every run, so the workload, the
# setup ledger and the metrics are identical; only the node binary on PATH changes.
#
#   run_compare.sh <node-bin-dir> <out.log> [extra nanospam args...]
#
# develop:    run_compare.sh <develop-wt>/target/release develop-1.log --fork-percentage 5
# branch RAI: run_compare.sh <repo>/target/release rai-1.log --epoch-duration-ms 8000
#
# Exit: 0 ok, 3 nanospam aborted, 4 hard timeout, 5 machine busy. A non-zero
# exit means the run produced no valid data point and must be discarded.
set -u

# Hard per-run timeout in seconds, measured from the first confirmation, i.e.
# over publishing only: setup (node start-up, PR funding, the seed fan-out and
# the quorum wait) takes ~70 s and varies, and it is not what the comparison
# measures. Healthy publishing is 21-25 s, a degraded one 150 s+, so 90 s keeps
# every good run with 3.5x margin and discards the collapsed ones.
HARD=${HARD:-90}
# Backstop for a setup that never reaches the first confirmation
SETUP_HARD=${SETUP_HARD:-180}
# Consecutive seconds without a confirmation that count as a stalled run
STALL=${STALL:-15}

NODE_BIN=$1
OUT=$2
shift 2
REPO=/Users/ruimorais/rsnano-node
# The node is launched as plain `rsnano` from PATH; nanospam comes from the branch
export PATH=$NODE_BIN:$REPO/target/release:$PATH
DATA=$HOME/NanoSpam

# Nothing may outlive this script: an error must not leave six nodes running
# into the next run (it did once, and contaminated it)
cleanup() {
  kill ${NSPID:-0} 2>/dev/null
  pkill -f "rsnano --network test" 2>/dev/null
  sleep 3
  pkill -9 -f "rsnano --network test" 2>/dev/null
  pkill -f nanospam 2>/dev/null
  pkill -f caffeinate 2>/dev/null
  rm -rf "$DATA"
}
trap cleanup EXIT INT TERM

# Idle gate: a run under load is bad data. `top -l 2` and read the second
# sample; the first reports 0% for every process on macOS. 25% is per process,
# where 100% is one core of eight. Quiet machine: ~2 s, the cost of sampling.
# Still busy after ~30 s: discard the run rather than measure under load.
busy=""
for attempt in {1..6}; do
  busy=$(top -l 2 -o cpu -n 5 -stats cpu,command | awk '/^PID|^Processes|^$|^[0-9]{4}\//{next} { if ($1+0 > 25 && $2 != "top") print }' | tail -5)
  if [ -z "$busy" ]; then break; fi
  echo "machine busy, waiting: $busy"; sleep 5
done
if [ -n "$busy" ]; then
  echo "MACHINE BUSY after 6 checks: run discarded ($busy)"
  exit 5
fi

echo "node binary: $(which rsnano)"
# A leftover node from an earlier run would poison this one
pkill -f "rsnano --network test" 2>/dev/null; sleep 1
rm -rf "$DATA"; mkdir -p "$DATA"
START=$(date +%s)
PUB_START=0
caffeinate -i nanospam --prs 6 --no-prio --blocks 45000 --accounts 45000 --rate 2000 \
  --no-kill "$@" > "$OUT" 2>&1 &
NSPID=$!

# The run is over when nanospam confirmed every block (it prints its final rate
# then), or when it reported no confirmation for STALL *consecutive* seconds, or
# at the hard timeout. Counting zero-cps lines anywhere in the log would cut a
# degraded run short while it is still trickling confirmations.
while true; do
  # nanospam prints its final rate once all 45000 blocks are published and it
  # stops; the last ~1000 confirmations land during the RPC dump below
  if grep -a -q "Confirmation rate:" "$OUT"; then
    echo "publishing finished: $(( $(date +%s) - PUB_START ))s publishing, $(( $(date +%s) - START ))s total"
    break
  fi
  status_lines=$(grep -a -c "cps |" "$OUT")
  if [ "$status_lines" -ge $STALL ] && \
     [ "$(grep -a "cps |" "$OUT" | tail -$STALL | grep -a -c '| 0 cps |')" -ge $STALL ]; then
    echo "stalled: $STALL consecutive seconds at 0 cps, publishing $(( $(date +%s) - PUB_START ))s"
    break
  fi
  if grep -a -q "never saw the full quorum\|never held the same ledger\|^Error" "$OUT"; then
    echo "nanospam aborted: $(grep -a 'never saw\|never held\|^Error' "$OUT" | head -1 | cut -c1-200)"
    exit 3
  fi
  # The clock for the hard timeout starts at the first confirmation
  if [ $PUB_START -eq 0 ] && grep -a -q "cps |" "$OUT"; then
    PUB_START=$(date +%s)
    echo "setup took $(( PUB_START - START ))s"
  fi
  if [ $PUB_START -eq 0 ]; then
    if [ $(( $(date +%s) - START )) -gt $SETUP_HARD ]; then
      echo "SETUP TIMEOUT after ${SETUP_HARD}s: run discarded"
      exit 4
    fi
  elif [ $(( $(date +%s) - PUB_START )) -gt $HARD ]; then
    echo "HARD TIMEOUT after ${HARD}s of publishing: run discarded"
    exit 4
  fi
  sleep 2
done

# Evidence that every build ran the same workload: the representative weights
# the ledger ended with, and the block counts
echo "=== PR0 representatives ===" >> "$OUT"
curl -sg -m 10 -d '{"action":"representatives","sorting":"true"}' "http://[::1]:17076" >> "$OUT"; echo >> "$OUT"
for i in 0 1 2 3 4 5; do
  echo "=== PR$i block_count ===" >> "$OUT"
  curl -sg -m 10 -d '{"action":"block_count"}' "http://[::1]:$((17076+10*i))" >> "$OUT"; echo >> "$OUT"
done
python3 $REPO/doc/benchmarks/rai-committees-3epochs-8s-2000-45k-2026-09-20/summarize.py "$OUT"
