#!/bin/sh
# Rebuild analysis.txt from the archived close-event and result logs.
cd "$(dirname "$0")"
runs="base1 base2 base3 new1 new2 new3 loaded-base1 loaded-new1"
for r in $runs; do cat $r-close-events.log $r-results.log > $r.combined.log; done
python3 analyze.py $(for r in $runs; do printf '%s.combined.log ' $r; done) > analysis.txt 2>&1
rm -- *.combined.log
