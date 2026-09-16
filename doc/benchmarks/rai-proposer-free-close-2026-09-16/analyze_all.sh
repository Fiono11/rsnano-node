#!/bin/sh
# Rebuild analysis.txt from the archived close-event and result logs.
cd "$(dirname "$0")"
runs="base1 base2 base3 new2 new3 new4 new8 new9 new10 a12 a14 a17 a19 a20 a21 a22"
for r in $runs; do cat $r-close-events.log $r-results.log > $r.combined.log; done
python3 analyze.py $(for r in $runs; do printf '%s.combined.log ' $r; done) > analysis.txt 2>&1
rm -- *.combined.log
