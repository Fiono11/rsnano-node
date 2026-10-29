#!/bin/sh
# RaiRecover runs.
#   ./recover.sh sim <cfg> <seconds> [module]  random simulation (2 workers), out_<cfg>_sim.txt
#   ./recover.sh bfs <cfg> <seconds> [module]  breadth-first search (all cores), out_<cfg>_bfs.txt
#   ./recover.sh all [simlist] [bfslist]       the simulations of recover_sim.txt, four at a
#                                              time, then the searches of recover_bfs.txt
#                                              (many_sim.txt, many_bfs.txt: RaiRecoverMany)
# A run stops at the first violation, at the end of the search, or after <seconds>.
# Needs tla2tools.jar one directory up (or TLA2TOOLS=<path>) and GNU timeout.
# TRACEDIR=<dir> also saves each error trace as <dir>/trace_<cfg>.json.
J=${TLA2TOOLS:-../tla2tools.jar}
TMP=${TMPDIR:-/tmp}
run() {
  mode=$1 c=$2 t=$3 mod=${4:-RaiRecover}
  case $mode in
    sim) opts="-workers 2 -simulate num=1000000000 -depth 150"; heap=3g ;;
    bfs) opts="-workers ${WORKERS:-auto}"; heap=10g ;;
  esac
  out=out_${c}_$mode.txt
  [ -n "$TRACEDIR" ] && opts="$opts -dumpTrace json $TRACEDIR/trace_$c.json"
  /usr/bin/time -p timeout "$t" java -XX:+UseParallelGC -Xmx$heap -cp "$J" tlc2.TLC -deadlock \
    $opts -metadir "$TMP/tlc-$c-$mode" -config "$c.cfg" "$mod.tla" > "$out" 2>&1
  echo "$c ($mode): $(grep -E 'is violated|No error|traces generated|distinct states found' "$out" | tail -1 | cut -c1-110)"
}
if [ "$1" = all ]; then
  grep -v "^#" ${2:-recover_sim.txt} | grep . | xargs -n4 -P4 sh -c './recover.sh "$0" "$1" "$2" "$3"'
  grep -v "^#" ${3:-recover_bfs.txt} | grep . | xargs -n4 -P1 sh -c './recover.sh "$0" "$1" "$2" "$3"'
else
  run "$@"
fi
