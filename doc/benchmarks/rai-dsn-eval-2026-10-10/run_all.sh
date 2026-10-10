#!/bin/sh
# The DSN 2027 evaluation matrix: three arms, nine variants, three repetitions,
# then the link-delay matrix on two variants. Resumable (re-run the script).
cd "$(dirname "$0")/../../.."
python3 tools/rai/run_eval_matrix.py doc/benchmarks/rai-dsn-eval-2026-10-10/matrix --reps 3 --wait-quiet 3600 \
  --arm rai=target/release \
  --arm pause=target/release:--epoch-transition=pause \
  --arm nano=/Users/ruimorais/.claude/jobs/e320c0dd/tmp/devbin:--no-epochs:--timeout=240:--settle-timeout=45
python3 tools/rai/run_eval_matrix.py doc/benchmarks/rai-dsn-eval-2026-10-10/delay40 --reps 3 --wait-quiet 3600 --variants nofork,fork5 \
  --extra "--network-delay-ms 40" \
  --arm rai=target/release \
  --arm pause=target/release:--epoch-transition=pause \
  --arm nano=/Users/ruimorais/.claude/jobs/e320c0dd/tmp/devbin:--no-epochs:--timeout=240:--settle-timeout=45
echo MATRIX_DONE
