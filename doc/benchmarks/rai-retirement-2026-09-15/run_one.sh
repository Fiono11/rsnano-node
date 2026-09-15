#!/bin/zsh
# usage: run_one.sh <bindir> <name> <rate> [extra nanospam args...]
# 6 PRs, no prio, 5% forks, 50k blocks/accounts, count epochs (25k), fresh data dir deleted afterwards.
S=/private/tmp/claude-501/-Users-ruimorais-rsnano-node/73cc949b-b1b3-4bb1-a391-89e589580254/scratchpad/runs
BIN=$1; NAME=$2; RATE=$3; shift 3
export PATH=$BIN:$PATH
DATA=$S/data-$NAME
OUT=$S/$NAME; mkdir -p $OUT
pkill -x rsnano 2>/dev/null; sleep 1
rm -rf "$DATA"; mkdir -p "$DATA"
echo "START $(date -u +%FT%TZ) name=$NAME rate=$RATE extra=$* rsnano=$(which rsnano) nanospam=$BIN/nanospam" > "$OUT/nanospam.log"
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
$BIN/nanospam --prs 6 --no-prio --fork-percentage 5 --blocks 50000 --accounts 50000 --rate $RATE \
  --epoch-terminated-elections 25000 --closed-epochs 2 --close-timeout 240 --no-kill --data-dir "$DATA" "$@" >> "$OUT/nanospam.log" 2>&1
STATUS=$?
kill $SAMPLER 2>/dev/null
for i in 0 1 2 3 4 5; do
  p=$((17076 + i*10))
  curl -g -s -m 10 -d '{"action":"block_count"}' "http://[::1]:$p/" > "$OUT/count_pr$i.json"
  curl -g -s -m 30 -d '{"action":"stats","type":"counters"}' "http://[::1]:$p/" > "$OUT/stats_pr$i.json"
  curl -g -s -m 30 -d '{"action":"confirmation_active"}' "http://[::1]:$p/" > "$OUT/active_pr$i.json"
done
pkill -x rsnano 2>/dev/null; sleep 2; pkill -9 -x rsnano 2>/dev/null
echo "NANOSPAM_EXIT=$STATUS END $(date -u +%FT%TZ)" >> "$OUT/nanospam.log"
rm -rf "$DATA"
echo "DONE $NAME exit=$STATUS $(date -u +%FT%TZ)" >> "$S/progress.log"
