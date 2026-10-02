#!/bin/sh
# Phase 0 has one supported gate variant: fork0 within a single long epoch.
set -eu
output=$1
shift
mkdir -p "$output"
exec "$(dirname "$0")/run_variant.sh" "$output/fork0" "$@"
