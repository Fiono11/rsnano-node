#!/bin/sh
# Rebuild analysis.txt from the archived close-event and result logs.
cd "$(dirname "$0")"
runs="base1 base2 base3 base4 c1 c2 new1 new2 new3 new4 new5 new6 new7 new8 noretry1"
for r in $runs; do cat $r-close-events.log $r-results.log > $r.combined.log; done
python3 analyze.py $(for r in $runs; do printf '%s.combined.log ' $r; done) > analysis.txt 2>&1
rm -- *.combined.log
