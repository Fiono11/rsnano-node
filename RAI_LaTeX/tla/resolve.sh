#!/bin/sh
# Termination at a forked position (RaiResolve), N = 4.
#  *_sim: random simulation of Spec (every pattern of good epochs)
#  *_23:  Spec23, exhaustive, an arbitrary fork epoch followed by two good epochs
# The _23 runs use deadlock checking: a state where a good epoch can never be left would be reported.
J=${TLA2TOOLS:-../tla2tools.jar}
run(){ c=$1; shift; /usr/bin/time -p java -XX:+UseParallelGC -Xmx10g -cp "$J" tlc2.TLC -workers auto -metadir /tmp/tlc-$c "$@" -config $c.cfg RaiResolve.tla > out_$c.txt 2>&1; echo "$c: $(grep -E 'violated|No error|Deadlock|distinct states found|^real' out_$c.txt | tr '\n' ' ')"; }
run Resolve4_norules_sim -simulate num=3000000 -depth 120
run Resolve4_norules_23_sim -simulate num=2000000 -depth 100
run Resolve4_rules_sim -simulate num=20000000 -depth 150
run Resolve4_rules_23_ProbeOneGood
run Resolve4_rules_23_ProbeGood23
run Resolve4_rules_23
