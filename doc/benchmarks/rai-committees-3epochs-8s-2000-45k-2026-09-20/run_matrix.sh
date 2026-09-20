#!/bin/zsh
# Phase 1 of the comparison: develop vs this branch built without rai_protocol,
# once with and once without forks. Decides which of the two is the baseline the
# RAI runs are compared against: if they agree, develop is the honest anchor; if
# they differ, the branch's own legacy build is the only fair control, because
# the difference is then the non-RAI refactoring, not the consensus rules.
#
# One nanospam (this branch) drives every run, so the workload, the setup ledger
# and the metrics are identical; only the node binary on PATH changes. The pairs
# run adjacent in time so thermal drift does not load onto one build.
set -u
S=/Users/ruimorais/rsnano-node/doc/benchmarks/rai-committees-3epochs-8s-2000-45k-2026-09-20
SCRATCH=/private/tmp/claude-501/-Users-ruimorais-rsnano-node/8422379b-da05-4613-ab2a-4a9572a9fb30/scratchpad
DEVELOP=$SCRATCH/develop-wt/target/release            # develop ff2b32710
LEGACY=$SCRATCH/branch-nofeature-target/release       # this branch, no rai_protocol
mkdir -p $S/compare
# exit 0 ok, 3 nanospam aborted, 4 hard timeout, 5 machine busy: a discarded
# run is named in the summary instead of silently missing
run() {
  echo "\n########## $1 ##########"
  $S/run_compare.sh $2 $S/compare/$1.log ${@:3} 2>&1 | grep -v "^machine busy"
  local code=${pipestatus[1]}
  echo "exit=$code"
  if [ $code -ne 0 ]; then echo "DISCARDED $1 (exit $code)" >> $S/compare/discarded.txt; fi
}

run develop-fork0-1 $DEVELOP
run legacy-fork0-1  $LEGACY
run develop-fork5-1 $DEVELOP --fork-percentage 5
run legacy-fork5-1  $LEGACY  --fork-percentage 5

echo "\n########## summary ##########"
python3 $S/summarize.py $S/compare/*.log
