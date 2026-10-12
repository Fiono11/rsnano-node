#!/bin/sh
# The 40 ms link-delay series for the RAI and pause arms on the same build, after the matrix.
cd "$(dirname "$0")/../../.."
until grep -q MATRIX_DONE doc/benchmarks/rai-dsn-eval-2026-10-12/run_all.log 2>/dev/null; do sleep 30; done
python3 tools/rai/run_eval_matrix.py doc/benchmarks/rai-dsn-eval-2026-10-12/delay40 --reps 3 --wait-quiet 3600 --variants nofork,fork5 \
  --extra "--network-delay-ms 40" \
  --arm rai=target/release \
  --arm pause=target/release:--epoch-transition=pause
echo DELAY_DONE
