#!/bin/zsh
# fork5 on three builds, interleaved: a = 444900536 (before the frontier fix),
# b = current tree, c = current tree without the leader proposal gate.
# run_variant.sh leaves the nodes up on a stall so the run can be examined over
# RPC, so the series tears down before each run rather than trusting the last
# one to have cleaned up after itself.
set -u
REPO=/Users/ruimorais/rsnano-node
SP=/private/tmp/claude-501/-Users-ruimorais-rsnano-node/5a115a25-986f-4e1f-b6f4-959bef236ae6/scratchpad
D=$REPO/doc/benchmarks/rai-paper-2026-09-22/delay-ab
V=$D/verdicts.txt
: > $V

teardown() {
  pkill -f "rsnano --network test" 2>/dev/null; sleep 3
  pkill -9 -f "rsnano --network test" 2>/dev/null
  pkill -f nanospam 2>/dev/null; pkill -f caffeinate 2>/dev/null
  rm -rf $HOME/NanoSpam
  # wait for the RPC ports to come free: a killed node holds them briefly
  for _ in {1..30}; do
    lsof -nP -iTCP:17076 -sTCP:LISTEN >/dev/null 2>&1 || break
    sleep 1
  done
}

for rep in 1 2; do
  for arm in a b c; do
    teardown
    LOG=$D/$arm-$rep.log
    NODE_BIN=$REPO/target/ab/$arm FORKS=5 NODES=6 $REPO/doc/benchmarks/rai-paper-2026-09-22/tools/run_variant.sh $LOG > $D/$arm-$rep.wrapper 2>&1
    rc=$?
    {
      echo "$arm rep$rep exit=$rc | $(grep -ah 'CONSISTENT\|SAFE\|UNSAFE\|aborted\|stalled:' $LOG.snap 2>/dev/null | tr '\n' ' ' | cut -c1-200)"
      echo "    $(grep -ah 'rate=\|no second reached' $LOG.snap 2>/dev/null | head -1)"
      echo "    closes: $(grep -ah 'EPOCH_CLOSED' $LOG | grep -o 'epoch=[0-9]* .*round=[0-9]*' | sed 's/.*\(epoch=[0-9]*\).*\(round=[0-9]*\)/\1 \2/' | sort | uniq -c | tr '\n' ' ')"
    } >> $V
    echo "== $arm rep$rep done rc=$rc" >> $V
  done
done
teardown
echo AB_DONE >> $V
