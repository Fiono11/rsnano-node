#!/bin/sh
# Safety of Rule 2 keeping the block with the most reporter votes (MAXKEEP).
J=${TLA2TOOLS:-../tla2tools.jar}
run(){ m=$1; c=$2; /usr/bin/time -p java -XX:+UseParallelGC -Xmx8g -cp "$J" tlc2.TLC -deadlock -workers auto -metadir /tmp/tlc-$c -config $c.cfg $m.tla > out_$c.txt 2>&1; echo "$c: $(grep -E 'violated|No error|distinct states found|^real' out_$c.txt | tr '\n' ' ')"; }
for c in Recovery4_same_fixed Recovery4_disjoint_fixed Recovery4_shared1_fixed Recovery4_shared2_fixed; do run RaiRecovery ${c}_maxkeep; done
run RaiClose Close_TRUE_maxkeep
run RaiOverlap Overlap_fixed_maxkeep
run RaiHandoff Handoff_same_rule_maxkeep
run RaiHandoff Handoff_disjoint_rule_maxkeep
