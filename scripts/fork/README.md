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
