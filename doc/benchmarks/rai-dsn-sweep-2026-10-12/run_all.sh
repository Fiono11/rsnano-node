#!/bin/sh
# Throughput-latency sweep for the DSN evaluation (build de99424d4).
# Main figures: one run per point below 1500/s, three at 1500/s and above
# (median with min-max); Byzantine-leader and reconfiguration sets: one run
# per point. develop = unmodified develop ff2b32710 (main checkout
# doc/benchmarks/paper-eval-2026-09-24/bin/develop) with this branch's
# nanospam; DEVBIN holds the two links. Resumable.
cd "$(dirname "$0")/../../.."
DEVBIN=${DEVBIN:-/Users/ruimorais/.claude/jobs/43c953df/tmp/devbin}
ARMS="--arm rai=target/release --arm pause=target/release:--epoch-transition=pause --arm develop=$DEVBIN:--no-epochs:--timeout=240:--settle-timeout=45"
python3 tools/rai/run_sweep.py doc/benchmarks/rai-dsn-sweep-2026-10-12 --set main --reps 3 --reps-from 1500 $ARMS
python3 tools/rai/run_sweep.py doc/benchmarks/rai-dsn-sweep-2026-10-12 --set byz --set reconf $ARMS
echo SWEEP_DONE
