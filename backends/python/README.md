# super-router-core (Python)

Exact pricing (`charge`, `bound`, `compile_credit_price`) and offering routing (`route`) for Super Router.
The TypeScript package in `packages/core` is the reference; this port reproduces every case in
[`../../conformance`](../../conformance/README.md). Pure standard library, no dependencies.

```bash
pip install -e ".[dev]"
pytest                    # or: PYTHONPATH=src python -m unittest discover -s tests
ruff check . && mypy
```

Apache-2.0. Copyright SUPER SOLUTION LABS INC.
