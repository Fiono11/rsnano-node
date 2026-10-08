#!/bin/sh
# usage: ./run.sh RaiClose Close_TRUE_3
exec java -XX:+UseParallelGC -Xmx6g -cp ../tla2tools.jar tlc2.TLC -deadlock -workers auto -metadir /tmp/tlc-$2 -config $2.cfg $1.tla
