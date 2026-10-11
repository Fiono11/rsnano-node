#!/bin/sh
# Runs every RaiRecovery configuration and prints one result line each.
# usage: ./recovery.sh            (expects ../tla2tools.jar, like run.sh)
J=${TLA2TOOLS:-../tla2tools.jar}
run(){ c=$1; shift; java -XX:+UseParallelGC -Xmx8g -cp "$J" tlc2.TLC -deadlock -workers auto -metadir /tmp/tlc-$c "$@" -config $c.cfg RaiRecovery.tla > out_$c.txt 2>&1; echo "$c: $(grep -E 'violated|No error|distinct states found' out_$c.txt | tr '\n' ' ')"; }
for k in same disjoint shared1 shared2; do run Recovery4_${k}_exempt; run Recovery4_${k}_fixed; done
for p in ProbeFinalVsLock ProbeRecovery ProbeRecoveryContested; do run Recovery4_same_$p; done
run Recovery6_same_exempt_sim -simulate num=2000000 -depth 60
run Recovery6_same_ProbeRecovery -simulate num=2000000 -depth 60
run Recovery6_same_fixed
