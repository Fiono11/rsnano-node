#!/bin/zsh
# The four variants of one build, in the order the checks get harder:
# no forks, forks, forks + one offline representative, forks + one Byzantine
# representative. A failed check keeps its nodes up inside run_variant.sh for
# a post-mortem; they are torn down here before the next variant.
#   run_matrix.sh <phase-dir>
set -u
T=/Users/ruimorais/rsnano-node/doc/benchmarks/rai-paper-2026-09-22/tools
D=$1
mkdir -p $D
V=$D/verdicts.txt; : > $V
teardown() { pkill -f "rsnano --network test" 2>/dev/null; sleep 3; pkill -9 -f "rsnano --network test" 2>/dev/null; pkill -f nanospam 2>/dev/null; pkill -f caffeinate 2>/dev/null; rm -rf ~/NanoSpam; }
run() {
  local name=$1 nodes=$2 forks=$3; shift 3
  echo "\n########## $name ##########"
  local restarts=3; [ $nodes -lt 6 ] && restarts=1
  local allow=0; [ $nodes -lt 6 ] && allow=1
  # Every passing run settled within a second (six nodes) or within 31 s (a
  # faulty representative, pending instances allowed); a longer wait only
  # delays the verdict of a run that failed
  local deadline=30; [ $nodes -lt 6 ] && deadline=60
  FORKS=$forks NODES=$nodes RESTARTS=$restarts SETTLE_DEADLINE=$deadline ALLOW_PENDING=$allow $T/run_variant.sh $D/$name.log "$@" > $D/$name.wrapper 2>&1
  local code=$?
  local verdict=$(grep -a -h "SETTLED_CONSISTENT\|CLOSED_CONSISTENT\|COMMITTEES_CONSISTENT\|^SAFE\|INCONSISTENT\|TIMEOUT\|SAFETY_VIOLATION\|COMMITTEES_MISSING\|COMMITTEES_OVERWEIGHT\|checks failed\|stalled\|nanospam aborted\|nanospam died" $D/$name.log.snap 2>/dev/null | cut -c1-120 | tr '\n' ' ')
  local perf=$(python3 $T/summarize.py $D/$name.log 2>/dev/null | sed "s/^[^:]*: //")
  echo "$name exit=$code | $verdict" | tee -a $V
  echo "    $perf" | tee -a $V
  echo "    discards: $(grep -a -c "EPOCH_DISCARDED" $D/$name.log) closes: $(grep -a -o 'EPOCH_CLOSED[^\n]*round=[0-9]*' $D/$name.log | grep -o 'round=[0-9]*' | sort | uniq -c | tr '\n' ' ')" | tee -a $V
  teardown
}
teardown
run fork0     6 0
run fork5     6 5
run offline1  5 5 --offline 1
run byz1      5 5 --byzantine 1
echo "\n########## verdicts ##########"; cat $V
