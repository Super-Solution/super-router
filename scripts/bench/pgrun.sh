#!/usr/bin/env bash
# PostgreSQL-backed Rust and Go servers against the fake upstream, one customer. A fresh database `srb` per run.
#
#   PG_URL_ADMIN=postgres://postgres@127.0.0.1:25433/postgres scripts/bench/pgrun.sh    # any local PostgreSQL you may drop `srb` in
#
# Throwaway server only: start it on a temporary data directory and delete it afterwards. Never point this at a real database.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${BENCH_OUT:-/tmp/srbench}
B=$OUT/rust/target/release
ADMIN=${PG_URL_ADMIN:-postgres://postgres@127.0.0.1:25433/postgres}
BENCH_DB=${PG_URL_BENCH:-${ADMIN%/*}/srb}
DELAY=${DELAY:-0}
DELAY_MS=$DELAY taskset -c 0 "$B/fake-upstream" & UP=$!
trap 'kill $UP 2>/dev/null' EXIT
sleep 0.5
run() { local name=$1 port=$2 stream=$3 c=$4 n=$5 cmd=$6
  psql "$ADMIN" -qAt -c "drop database if exists srb" -c "create database srb" >/dev/null
  export DATABASE_URL=$BENCH_DB
  bash -c "exec $cmd" & local pid=$!
  sleep 2
  local out rss
  out=$(MODE=router STREAM=$stream C=$c N=$n RUN=$RANDOM TARGET=http://127.0.0.1:$port timeout 150 taskset -c 3 "$B/loadgen" 2>&1 | grep "^mode")
  rss=$(grep VmHWM /proc/$pid/status 2>/dev/null | awk '{printf "%.0f", $2/1024}')
  echo "$name+pg d=$DELAY | $out | hwm_mib=$rss"
  kill $pid 2>/dev/null; wait $pid 2>/dev/null
}
for cfg in "1 1 500" "1 12 1500"; do
  set -- $cfg
  run rust 3410 "$1" "$2" "$3" "env NODELAY=1 taskset -c 1,2 $B/router-server"
  run go 3411 "$1" "$2" "$3" "taskset -c 1,2 $OUT/gobench"
done
