#!/bin/sh
set -e
chown -R bananocurrency:bananocurrency /home/bananocurrency
exec gosu bananocurrency /usr/bin/rsban "$@"
