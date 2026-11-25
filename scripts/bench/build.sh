#!/usr/bin/env bash
# Builds the benchmark binaries in a scratch directory (default /tmp/srbench, override with BENCH_OUT), from a COPY of
# the repository's backends so nothing in the repository is modified and the harness crate never becomes part of the
# published workspace. Needs cargo, go and python3 (with venv). Takes a few minutes the first time.
#
#   scripts/bench/build.sh            # builds $OUT/rust/target/release/{fake-upstream,router-server,loadgen,super-router-server}, $OUT/gobench, $OUT/pyenv
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=${BENCH_REPO:-$(cd "$HERE/../.." && pwd)}   # BENCH_REPO: another checkout of the repository to build (for before/after runs)
OUT=${BENCH_OUT:-/tmp/srbench}
mkdir -p "$OUT"
copy() { mkdir -p "$2"; (cd "$1" && tar --exclude=target --exclude=node_modules -cf - .) | (cd "$2" && tar -xf -); }

# Rust: the workspace globs crates/*, so the harness crate is dropped into the copy. The copy keeps Cargo.lock, so
# dependency versions are the repository's. A previous target/ is kept for faster rebuilds.
mkdir -p "$OUT/rust"
copy "$REPO/backends/rust" "$OUT/rust"
copy "$HERE/srbench" "$OUT/rust/crates/srbench"
(cd "$OUT/rust" && cargo build --release -p srbench -p super-router-cli)

# Go: the harness main goes into cmd/ of a copy of the module; with DATABASE_URL set it uses the PostgreSQL store.
mkdir -p "$OUT/go"
copy "$REPO/backends/go" "$OUT/go"
mkdir -p "$OUT/go/cmd/gobench"
cp "$HERE/gobench/main.go" "$OUT/go/cmd/gobench/main.go"
(cd "$OUT/go" && go build -o "$OUT/gobench" ./cmd/gobench)

# Python: the harness imports the repository's source directly (SR_PY_SRC), only the server and client libraries are installed.
python3 -m venv "$OUT/pyenv"
"$OUT/pyenv/bin/pip" install -q httpx uvicorn "$REPO/backends/python"
echo "built in $OUT"
