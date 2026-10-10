#!/bin/sh
# Everything left, in one process kept awake by caffeinate (run as
# `caffeinate -i sh finish.sh`): re-run the matrix runs without a summary,
# build develop ff2b32710 + the link-delay patch, then re-run the Nano arm of
# the delay series with it.
set -u
cd "$(dirname "$0")/../../.."
OUT=doc/benchmarks/rai-dsn-eval-2026-10-10
TMP=/Users/ruimorais/.claude/jobs/e320c0dd/tmp
DEV=$TMP/devbin
python3 tools/rai/run_eval_matrix.py $OUT/matrix --reps 3 --wait-quiet 3600 \
  --arm rai=target/release \
  --arm pause=target/release:--epoch-transition=pause \
  --arm nano=$DEV:--no-epochs:--timeout=240:--settle-timeout=45
echo MATRIX_RESUMED
(cd $TMP/develop-delay && CARGO_TARGET_DIR=$TMP/develop-target cargo build --release -p rsnano_cli) > $TMP/devbuild.log 2>&1 || { echo DEV_BUILD_FAILED; exit 1; }
mkdir -p $TMP/devbin-delay
ln -sf $TMP/develop-target/release/rsnano $TMP/devbin-delay/rsnano
ln -sf "$PWD/target/release/nanospam" $TMP/devbin-delay/nanospam
strings $TMP/devbin-delay/rsnano | grep -q simulated_delay_ms || { echo DEV_BUILD_HAS_NO_DELAY; exit 1; }
echo DEV_BUILT
python3 tools/rai/run_eval_matrix.py $OUT/delay40 --reps 3 --wait-quiet 3600 --variants nofork,fork5 \
  --extra "--network-delay-ms 40" \
  --arm nano=$TMP/devbin-delay:--no-epochs:--timeout=240:--settle-timeout=45
echo FINISH_DONE
