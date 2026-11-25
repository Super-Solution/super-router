#!/usr/bin/env bash
# Runs the benchmark matrix against a fake OpenAI Responses upstream (3 SSE events per answer, fixed delay).
#
#   DELAY=0  scripts/bench/run.sh            # upstream delay 0 ms: the pure overhead a router adds (c=1 and c=64)
#   DELAY=20 CONFIGS="router 0 64 6000" scripts/bench/run.sh   # one configuration only
#   TARGETS="rust-cli-upstream direct" scripts/bench/run.sh     # which servers
#
# Layout: the fake upstream is pinned to CPU 0, the server under test to CPUs 1 and 2, the load generator to CPU 3, so it
# needs at least four CPUs. Each configuration starts a fresh server, one
# customer, one key, and a unique Idempotency-Key per request.
#
# Targets:
#   rust-nodelay  harness router (own router, memory store, TCP_NODELAY on)
#   rust-stock    the same without TCP_NODELAY
#   rust-cli      the shipped `super-router-server` binary in the repository's build, mock provider (no upstream), as shipped
#   rust-cli-upstream  the same binary with the real OpenAI provider pointed at the fake upstream (`OPENAI_BASE_URL`)
#   go, python    the harness servers (memory store)
#   direct        the load generator straight at the fake upstream (no router): the floor
# A configuration is "MODE STREAM CONCURRENCY REQUESTS" (MODE router, or mock for rust-cli).
#
# Reading the numbers: "added" = a target's p50 minus direct's p50. The machine is shared in the recorded runs, so repeat
# and compare ranges, never single numbers.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${BENCH_OUT:-/tmp/srbench}
B=$OUT/rust/target/release
DELAY=${DELAY:-0}
REPS=${REPS:-2}
TARGETS=${TARGETS:-"rust-nodelay rust-stock rust-cli go python direct"}
CONFIGS=${CONFIGS:-"router 1 1 2000|router 0 1 2000|router 1 64 6000|router 0 64 6000"}
export SR_PY_SRC=${SR_PY_SRC:-$HERE/../../backends/python/src}

DELAY_MS=$DELAY taskset -c 0 "$B/fake-upstream" & UP=$!
trap 'kill $UP 2>/dev/null' EXIT
sleep 0.5
cpu() { awk '{printf "%.3f", ($14+$15)/100}' "/proc/$1/stat"; }
run() { # name port mode stream c n command
  local name=$1 port=$2 mode=$3 stream=$4 c=$5 n=$6
  bash -c "exec $7" & local pid=$!
  sleep 1.5
  local c0 out c1 rss
  c0=$(cpu $pid)
  out=$(MODE=$mode STREAM=$stream C=$c N=$n RUN=$RANDOM TARGET=http://127.0.0.1:$port timeout 150 taskset -c 3 "$B/loadgen" 2>&1 | grep "^mode")
  c1=$(cpu $pid)
  rss=$(grep VmHWM /proc/$pid/status 2>/dev/null | awk '{printf "%.0f", $2/1024}')
  echo "$name | $out | cpu_s=$(echo "$c1 $c0" | awk '{printf "%.2f",$1-$2}') | hwm_mib=$rss"
  kill $pid 2>/dev/null; wait $pid 2>/dev/null
}
CLI_COMMON="SUPERROUTER_API_KEY=sr_$(printf 'a%.0s' $(seq 64)) SUPERROUTER_DIGEST_KEY=bench-digest-key-0123456789abcdef-0123 SUPERROUTER_CUSTOMER=c1 ADDR=127.0.0.1:3413"
CLI_ENV="SUPERROUTER_MOCK=1 $CLI_COMMON"
CLI_UP_ENV="OPENAI_API_KEY=sk-fake OPENAI_BASE_URL=http://127.0.0.1:3499 $CLI_COMMON"
echo "# $(date -u +%FT%TZ) $(nproc) CPUs, load $(cut -d' ' -f1-3 /proc/loadavg), $(git -C "$HERE" rev-parse --short HEAD 2>/dev/null)"
for rep in $(seq "$REPS"); do
  IFS='|' read -ra cfgs <<<"$CONFIGS"
  for cfg in "${cfgs[@]}"; do
    set -- $cfg
    for t in $TARGETS; do
      case $t in
        rust-nodelay) run "rust-nodelay d=$DELAY" 3410 "$1" "$2" "$3" "$4" "env NODELAY=1 taskset -c 1,2 $B/router-server" ;;
        rust-stock)   run "rust-stock d=$DELAY" 3410 "$1" "$2" "$3" "$4" "taskset -c 1,2 $B/router-server" ;;
        rust-cli)     run "rust-cli d=$DELAY" 3413 mock "$2" "$3" "$4" "env $CLI_ENV taskset -c 1,2 $B/super-router-server" ;;
        rust-cli-upstream) run "rust-cli-upstream d=$DELAY" 3413 "$1" "$2" "$3" "$4" "env $CLI_UP_ENV taskset -c 1,2 $B/super-router-server" ;;
        go)           run "go d=$DELAY" 3411 "$1" "$2" "$3" "$4" "taskset -c 1,2 $OUT/gobench" ;;
        python)       run "python d=$DELAY" 3412 "$1" "$2" "$3" "$4" "taskset -c 1,2 $OUT/pyenv/bin/python $HERE/pybench.py" ;;
        direct)       run "direct d=$DELAY" 3499 direct "$2" "$3" "$4" "sleep 100" ;;
      esac
    done
  done
done
