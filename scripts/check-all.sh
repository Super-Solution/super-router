#!/usr/bin/env bash
# The whole local gate: TypeScript, Go and Python against the same PostgreSQL, with the database tests required
# (a skipped database test counts as a failure). Hosted CI is not used, so this is what "green" means.
#
#   npm run db:up                      # or point DATABASE_URL at any PostgreSQL
#   python -m venv .venv && . .venv/bin/activate && pip install -e "backends/python[dev]"
#   scripts/check-all.sh [ts|go|python ...]     # no argument runs all three
set -euo pipefail
cd "$(dirname "$0")/.."
export SR_REQUIRE_POSTGRES=1
export DATABASE_URL="${DATABASE_URL:-postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter}"

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

ts() {
  step "TypeScript: build, typecheck, format, tests"
  npm run typecheck
  npm run format:check
  npx tsx --test --test-concurrency=1 tests/*.test.ts
}
go_() {
  step "Go: gofmt, vet, race tests"
  (cd backends/go
    test -z "$(gofmt -l .)" || { gofmt -l .; echo "gofmt: files need formatting" >&2; exit 1; }
    go vet ./...
    go test -race -count=1 ./...)
}
python_() {
  step "Python: ruff, mypy, pytest"
  (cd backends/python
    python -c "import pytest, anyio, httpx, psycopg, uvicorn" 2>/dev/null ||
      { echo 'missing Python dev dependencies: pip install -e "backends/python[dev]"' >&2; exit 1; }
    ruff check .
    ruff format --check .
    mypy
    python -m pytest -q)
}

targets=("$@")
[ ${#targets[@]} -gt 0 ] || targets=(ts go python)
for t in "${targets[@]}"; do
  case "$t" in
    ts) ts ;;
    go) go_ ;;
    python) python_ ;;
    *) echo "unknown target: $t (use ts, go or python)" >&2; exit 2 ;;
  esac
done
step "all checks passed: ${targets[*]}"
