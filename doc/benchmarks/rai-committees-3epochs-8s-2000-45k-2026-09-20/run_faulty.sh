#!/bin/zsh
# Verification after the four fixes: the honest run with every check, then
# --byzantine 1 three times and --offline 1 once, each with every check. A
# failed check keeps its nodes up inside run_fork_settle.sh; they are torn
# down here before the next run, the verdicts collected in compare/verdicts.txt
set -u
S=/Users/ruimorais/rsnano-node/doc/benchmarks/rai-committees-3epochs-8s-2000-45k-2026-09-20
mkdir -p $S/compare
V=$S/compare/verdicts.txt; : > $V
teardown() { pkill -f "rsnano --network test" 2>/dev/null; sleep 3; pkill -9 -f "rsnano --network test" 2>/dev/null; pkill -f nanospam 2>/dev/null; pkill -f caffeinate 2>/dev/null; rm -rf ~/NanoSpam; }
SC=/private/tmp/claude-501/-Users-ruimorais-rsnano-node/8422379b-da05-4613-ab2a-4a9572a9fb30/scratchpad
run() {
  local name=$1 nodes=$2; shift 2
  echo "\n########## $name ##########"
  local restarts=3; [ $nodes -lt 6 ] && restarts=1
  local allow=0; [ $nodes -lt 6 ] && allow=1
  NODES=$nodes RESTARTS=$restarts SETTLE_DEADLINE=480 ALLOW_PENDING=$allow $S/run_fork_settle.sh $S/compare/$name.log "$@" > $S/compare/$name.wrapper 2>&1
  local code=$?
  # a failed check keeps the nodes up: what is still open, from every PR
  if [ $code -ne 0 ]; then PRS=$nodes python3 $SC/investigate3.py > $S/compare/$name.open 2>&1; fi
  local verdict=$(grep -a -h "SETTLED_CONSISTENT\|CLOSED_CONSISTENT\|COMMITTEES_CONSISTENT\|^SAFE\|INCONSISTENT\|TIMEOUT\|SAFETY_VIOLATION\|COMMITTEES_MISSING\|checks failed\|stalled\|nanospam aborted" $S/compare/$name.log.snap 2>/dev/null | tr '\n' ' ')
  local perf=$(python3 $S/summarize.py $S/compare/$name.log 2>/dev/null | sed "s/^[^:]*: //")
  echo "$name exit=$code | $verdict" | tee -a $V
  echo "    $perf" | tee -a $V
  grep -a -c "EPOCH_DISCARDED" $S/compare/$name.log | sed 's/^/    discards: /' | tee -a $V
  teardown
}
run byzantine-1 5 --byzantine 1
run honest      6
run offline-1   5 --offline 1
echo "\n########## verdicts ##########"; cat $V
