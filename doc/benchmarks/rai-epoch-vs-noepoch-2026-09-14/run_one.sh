#!/bin/zsh
# usage: run_one.sh <name> <rate> [extra nanospam args...]
# Runs nanospam (6 PRs, no prio, 5% forks, 50k blocks/accounts) with a fresh data dir,
# samples PR0 counters + per-node CPU every 2 s, snapshots RPC at the end, kills nodes, deletes data.
S=/private/tmp/claude-501/-Users-ruimorais-rsnano-node/ff407da5-1c3b-4bab-ba7a-a1ea97707882/scratchpad/runs
NAME=$1; RATE=$2; shift 2
export PATH=/Users/ruimorais/rsnano-node/target/release:$PATH
DATA=$S/data-$NAME
OUT=$S/$NAME; mkdir -p $OUT
pkill -x rsnano 2>/dev/null; sleep 1
rm -rf "$DATA"; mkdir -p "$DATA"
echo "START $(date -u +%FT%TZ) name=$NAME rate=$RATE extra=$* rsnano=$(which rsnano)" > "$OUT/nanospam.log"
# sampler
(
  T0=$(date +%s)
  while true; do
    NOW=$(date +%s); REL=$((NOW-T0))
    PIDS=$(pgrep -x rsnano | sort -n | tr '\n' ' ')
    CPU=$(ps -o pid=,%cpu=,rss= -p ${PIDS// /,} 2>/dev/null | tr '\n' ';')
    echo "T=$REL CPU=$CPU" >> "$OUT/cpu.log"
    C=$(curl -g -s -m 2 -d '{"action":"stats","type":"counters"}' "http://[::1]:17076/")
    A=$(curl -g -s -m 2 -d '{"action":"confirmation_active"}' "http://[::1]:17076/")
    B=$(curl -g -s -m 2 -d '{"action":"block_count"}' "http://[::1]:17076/")
    echo "{\"t\":$REL,\"counters\":${C:-null},\"active\":${A:-null},\"count\":${B:-null}}" >> "$OUT/samples.jsonl"
    sleep 2
  done
) &
SAMPLER=$!
nanospam --prs 6 --no-prio --fork-percentage 5 --blocks 50000 --accounts 50000 --rate $RATE --no-kill --data-dir "$DATA" "$@" >> "$OUT/nanospam.log" 2>&1
STATUS=$?
kill $SAMPLER 2>/dev/null
for i in 0 1 2 3 4 5; do
  p=$((17076 + i*10))
  curl -g -s -m 10 -d '{"action":"block_count"}' "http://[::1]:$p/" > "$OUT/count_pr$i.json"
  curl -g -s -m 30 -d '{"action":"stats","type":"counters"}' "http://[::1]:$p/" > "$OUT/stats_pr$i.json"
  curl -g -s -m 30 -d '{"action":"confirmation_active"}' "http://[::1]:$p/" > "$OUT/active_pr$i.json"
done
KEEPIT=""; [ "$KEEP" = "1" ] && KEEPIT=1; [ "$KEEP" = "fail" ] && [ "$STATUS" != "0" ] && KEEPIT=1
[ -z "$KEEPIT" ] && { pkill -x rsnano 2>/dev/null; sleep 2; pkill -9 -x rsnano 2>/dev/null; }
echo "NANOSPAM_EXIT=$STATUS END $(date -u +%FT%TZ)" >> "$OUT/nanospam.log"
[ -z "$KEEPIT" ] && rm -rf "$DATA"
echo "DONE $NAME exit=$STATUS $(date -u +%FT%TZ)" >> "$S/progress.log"
