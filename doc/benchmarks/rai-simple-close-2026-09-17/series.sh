#!/bin/zsh
# Alternate baseline and new builds, no trace dir, N pairs.
N=${1:-2}
NEW=/Users/ruimorais/rsnano-node/target/release
BASE=/Users/ruimorais/rsnano-node/target-baseline/release
cd /private/tmp/claude-501/-Users-ruimorais-rsnano-node/fab80b18-4aae-406a-84b7-d03ede14bfa6/scratchpad/bench
for i in $(seq 1 $N); do
  for pair in new:$NEW base:$BASE; do
    label=${pair%%:*}$i; bin=${pair#*:}
    mkdir -p /tmp/notrace-$label
    BIN=$bin TRACE=/tmp/notrace-$label ./run_one.sh $label > /dev/null 2>&1
    rm -rf /tmp/notrace-$label
    echo "== $label ($bin)"
    python3 analyze.py $label.log 2>&1 | grep -E "close after|forked=False"
  done
done
