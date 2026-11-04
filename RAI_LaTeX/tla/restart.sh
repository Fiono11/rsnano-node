#!/bin/sh
# Runs the RaiCloseRestart configurations; TLA2TOOLS is the path of tla2tools.jar.
# usage: TLA2TOOLS=... ./restart.sh [config ...]
: "${TLA2TOOLS:?set TLA2TOOLS to tla2tools.jar}"
configs=${*:-Restart4_durable Restart4_lose1 Restart4_lose1_conflict Restart4_keep_durable Restart4_keep_lose1 Restart4_keep_lose1_conflict Restart4_keep_hold Restart6_durable Restart6_lose1 Restart6_lose2 Restart6_lose2_conflict Restart6_hold Restart6_hold_blocks Restart6_keep_durable Restart6_keep_lose1 Restart6_keep_lose2 Restart6_keep_lose2_conflict Restart6_keep_hold}
for c in $configs; do
  dir=$(mktemp -d)
  java -XX:+UseParallelGC -Xmx10g -cp "$TLA2TOOLS" tlc2.TLC -deadlock -workers 8 -metadir "$dir" -config "$c.cfg" RaiCloseRestart.tla \
    | sed -E 's#[A-Za-z:]*/Users/[^ )]*/#<path>/#g; s#[A-Za-z:]*/private/[^ )]*/#<path>/#g; s#[A-Za-z:]*/var/folders/[^ )]*/#<path>/#g' > "out_$c.txt"
  rm -rf "$dir"
  echo "$c: $(grep -E 'is violated|No error' "out_$c.txt" | head -1) states=$(grep -E 'distinct states found' "out_$c.txt" | tail -1 | sed -E 's/.* ([0-9]+) distinct.*/\1/') steps=$(grep -cE '^State [0-9]+:' "out_$c.txt")"
done
