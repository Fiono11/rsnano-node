#!/bin/sh
set -e
chown -R nanocurrency:nanocurrency /home/nanocurrency
exec gosu nanocurrency /usr/bin/rsnano "$@"
