#!/bin/sh
# Re-run of the RAI and pause arms of the DSN matrix on 0c89c23cb (re-notarization rule and the
# termination rules). Nano is unaffected and keeps the 2026-10-10 runs. Resumable.
cd "$(dirname "$0")/../../.."
python3 tools/rai/run_eval_matrix.py doc/benchmarks/rai-dsn-eval-2026-10-12/matrix --reps 3 --wait-quiet 3600 \
  --arm rai=target/release \
  --arm pause=target/release:--epoch-transition=pause
echo MATRIX_DONE
