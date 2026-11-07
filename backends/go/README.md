# Super Router, Go

Exact pricing (`pricing.Charge`, `pricing.Bound`, `pricing.CompileCreditPrice`) and offering routing
(`routing.Route`). The TypeScript package in `packages/core` is the reference; both packages reproduce every
case in [`../../conformance`](../../conformance/README.md). Standard library only.

```bash
go test ./...                              # conformance cases
go test ./routing -bench . -benchmem       # about 4 µs and 38 allocations per route over the conformance mix
go vet ./... && gofmt -l .
```

Apache-2.0. Copyright SUPER SOLUTION LABS INC.
