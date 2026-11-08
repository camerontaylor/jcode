# Fork-owned CI budget baselines

Upstream's ratchet scripts live in `scripts/check_*_budget.py` and
`scripts/check_warning_budget.sh`, and read upstream's baseline files
(`scripts/code_size_budget.json`, `scripts/test_size_budget.json`,
`scripts/panic_budget.json`, `scripts/swallowed_error_budget.json`,
`scripts/warning_budget.txt`).

Those baselines were recorded on upstream's tree. On this fork's `custom` head
several of them already fail before any fork change, so `.github/workflows/fork-ci.yml`
runs the same scripts against the baselines in this directory instead. Upstream's
JSON files are never edited: the fork carries its debt here, so upstream merges
stay clean.

## How the override works

Each budget script reads its baseline path from a variable that defaults to
upstream's file, so the scripts behave identically for upstream and for anyone
running them locally without the variable set:

| Script | Variable | Fork baseline |
| --- | --- | --- |
| `check_code_size_budget.py` | `JCODE_CODE_SIZE_BUDGET` | `scripts/fork/code_size_budget.json` |
| `check_test_size_budget.py` | `JCODE_TEST_SIZE_BUDGET` | `scripts/fork/test_size_budget.json` |
| `check_panic_budget.py` | `JCODE_PANIC_BUDGET` | `scripts/fork/panic_budget.json` |
| `check_swallowed_error_budget.py` | `JCODE_SWALLOWED_ERROR_BUDGET` | `scripts/fork/swallowed_error_budget.json` |

`fork-ci.yml` sets these at the job level for the `quality` and `build` jobs.
Locally:

```bash
JCODE_CODE_SIZE_BUDGET=scripts/fork/code_size_budget.json \
  python3 scripts/check_code_size_budget.py
```

`check_wildcard_reexport_budget.py` has no fork baseline because it already
passes on `custom` under upstream's file. If it ever starts failing, add the
same two-line variable to that script rather than editing upstream's JSON.

## Formatting: `fmt-check.sh`

Upstream's `cargo fmt --all -- --check` is not usable here: the upstream base
`7fd42339c907de223021bd63e5bb5df778c43a01` is not rustfmt-clean, and
reformatting files the fork never touched turns them into fork carries that
conflict at every upstream base bump. `scripts/fork/fmt-check.sh <base>` checks
only the `.rs` files the fork adds or modifies (`git diff --diff-filter=AM
<base> HEAD`), running rustfmt standalone on each and skipping files that were
already unformatted at the base. `fork-ci.yml` runs it with
`$FORK_UPSTREAM_BASE`, set once at workflow level; **update that value when the
Desvio base moves**. The quality job checks out with `fetch-depth: 0` because
the script needs `git show <base>:<file>`.

Locally:

```bash
bash scripts/fork/fmt-check.sh 7fd42339c907de223021bd63e5bb5df778c43a01
```

## Upstreamable carries

`src/cli/cloud_managed_tests.rs` carries a one-line fix: `use std::path::Path`
was at module scope while every use sits inside the `x86_64-linux`-gated
`mod shell`, so clippy's `unused_imports` (denied in the quality job) failed.
The fix scopes the import to the module that uses it. It is a candidate to
send upstream rather than carry.

## What the baselines record

Each fork baseline is upstream's baseline with **one change per currently
failing entry**: the ceiling for that file becomes its current size on `custom`.
Entries that already passed keep upstream's tighter ceiling, so the fork does
not silently loosen the ratchet anywhere it did not have to.

To regenerate after intentional cleanup (this rewrites only the file named by
the variable, never upstream's):

```bash
JCODE_CODE_SIZE_BUDGET=scripts/fork/code_size_budget.json \
  python3 scripts/check_code_size_budget.py --update
```

## Ratchet semantics

Unchanged. A file that grows beyond its recorded ceiling fails, and a new
oversized file fails. The baselines are a ceiling for pre-existing debt, not a
licence to grow.

## Fork-owned debt

Two files are the fork's own oversized code, not tolerated upstream growth.
They must not ratchet upward; every change that adds to them is a change that
has to come back out:

- `src/cli/acp.rs` (3444 LOC). To be split into transport, session and
  presentation modules as the first step of the Phase 4 version boundary.
- `src/cli/acp/daemon_router.rs` (1617 LOC). To be split when R1 lands.

If either grows, raise it with the orchestrator instead of running `--update`:
the ceiling here is the size the fork intends to shrink from, and re-running
`--update` would erase that signal.

Core files that grew only slightly under fork hunks stay tolerated in these
baselines until the corresponding reshaping item (R1 to R6) or upstream
acceptance removes the hunk.
