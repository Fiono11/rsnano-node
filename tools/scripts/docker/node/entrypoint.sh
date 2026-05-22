#!/bin/sh
set -e

DATA_DIR=/home/nanocurrency/Nano

chown -R nanocurrency:nanocurrency "$DATA_DIR"

exec gosu nanocurrency /usr/bin/rsnano "$@"
