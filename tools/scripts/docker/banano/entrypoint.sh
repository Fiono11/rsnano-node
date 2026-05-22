#!/bin/sh
set -e

DATA_DIR=/home/bananocurrency/Banano

chown -R bananocurrency:bananocurrency "$DATA_DIR"

exec gosu bananocurrency /usr/bin/rsban "$@"
