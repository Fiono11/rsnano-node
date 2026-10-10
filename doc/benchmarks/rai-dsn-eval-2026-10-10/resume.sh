#!/bin/sh
# Re-run whatever the first pass of run_all.sh left without a summary (the
# matrix runner skips runs that have one). Kept awake with caffeinate.
cd "$(dirname "$0")/../../.."
OUT=doc/benchmarks/rai-dsn-eval-2026-10-10
DEV=/Users/ruimorais/.claude/jobs/e320c0dd/tmp/devbin
caffeinate -i python3 tools/rai/run_eval_matrix.py $OUT/matrix --reps 3 --wait-quiet 3600 \
  --arm rai=target/release \
  --arm pause=target/release:--epoch-transition=pause \
  --arm nano=$DEV:--no-epochs:--timeout=240:--settle-timeout=45
echo RESUME_DONE
