# t

`t` runs only the tests that cover a change. It is for coding agents and for people. Leave the full suite to CI, or start it on purpose.

```
t                      tests for the files changed against the base branch
t <file>...            a test file runs as is; a source file runs the tests that cover it
t --for <file>...      list the covering tests and why; run nothing
t --dry <file>...      print the commands; run nothing
t --max N              run up to N test files (default 5)
t <file> -- <args>     pass arguments to the test runner
```

## How it finds tests

| Language | Runner | Covering tests |
|---|---|---|
| Python | `uv run pytest` (with `uv.lock`), `.venv/bin/pytest` or `python3 -m pytest` | Tests that import the module, then tests of the modules that import it |
| Rust | `cargo test -p <crate>` | Tests in the file, `<name>_tests.rs`, and test files in the crate that name the module |
| Go | `go test ./<pkg>` | The tests of the same package |
| TypeScript, JavaScript | vitest, jest or playwright from the nearest `package.json` | The test file next to it, and test files that import it |

For Python and TypeScript/JavaScript, `t` also walks an import graph up to three imports away from the change. The graph resolves relative imports, tsconfig `paths`, jest `moduleNameMapper` aliases and `index` barrel files. `t --for` shows the chain, for example `imports src/components/index.ts → src/components/Button.tsx`. The parsed imports are cached in `~/.cache/t/` (or `$XDG_CACHE_HOME/t/`). `t` parses a file again only when its modification time changes.

For Django, `t` also follows two links that imports miss:

- A view (or a module that a view file imports) runs the tests that request its URL. That is the URL name from `path(..., name=...)` or `router.register(...)` in quotes, such as `reverse("invoice-list")`, or the fixed start of the route, such as `"invoices/"`.
- A module with `@receiver(..., sender=Model)` or `.connect(..., sender=Model)` runs the tests that use `Model`.

A test file whose name shares a distinctive word with the source file is added last.

Python test files are `test_*.py`, `*_test.py`, `tests.py` and `*_tests.py`. A changed `conftest.py` runs the tests at or below it that use its fixtures. With an autouse fixture, it runs all of them.

A changed test file always runs, even past `--max`. A changed data file (`.json`, `.jsonl`, `.yaml`, `.csv` and similar) runs the tests that name it. When no test covers the change, `t` says so and exits with code 4, so "nothing ran" is never read as a pass.

## Hints

After a failed run, `t` names a failure that comes from the environment: a missing Python package, a missing `node_modules`, a refused port, or Postgres out of lock slots or shared memory. In a linked worktree, it names an `.env` file that the primary checkout has and the worktree lacks. It does not copy that file.

## Settings

A repo can add `.agents/test.toml`:

```toml
[env]
PYTHONPATH = "{root}/shared"   # {root} is the repo root

[runner]
python = "uv run --all-packages pytest"
```

## Install

```
cargo install --git https://github.com/mrwick1/t --locked
```
