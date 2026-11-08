#!/usr/bin/env bash
# Fork-owned formatting gate: check only the .rs files the fork adds or
# modifies relative to the upstream base.
#
# Upstream's own base is not rustfmt-clean (including include!d test files that
# `cargo fmt` never reaches), so upstream's `cargo fmt --all -- --check` fails on
# this tree before any fork change. Reformatting those files would turn
# never-modified upstream files into fork carries, which collides at every
# upstream base bump. Instead: run rustfmt standalone (no child modules) over
# each .rs file in `git diff --diff-filter=AM <base> HEAD`, and skip any file
# that was already unformatted at the base, so the gate is exactly "the fork's
# own hunks stay formatted".
#
# Usage: scripts/fork/fmt-check.sh <base>   (run from the repository root)
#   <base> is the upstream commit the fork is assembled against; fork-ci.yml
#   passes $FORK_UPSTREAM_BASE. The checkout needs that commit locally
#   (actions/checkout fetch-depth: 0).
set -uo pipefail
base=${1:?usage: fmt-check.sh <base> (run inside the tree)}
fail=0
hunks() { rustfmt --edition 2024 --check 2>&1 | grep -c '^Diff in'; }
checked=0
skipped=0
while IFS= read -r f; do
  [ -f "$f" ] || continue
  head=$(hunks < "$f")
  [ "$head" = 0 ] && { checked=$((checked + 1)); continue; }
  basehunks=$(git show "$base:$f" 2>/dev/null | hunks)
  if [ -z "$(git cat-file -t "$base:$f" 2>/dev/null)" ] || [ "$basehunks" = 0 ]; then
    echo "rustfmt: $f ($head hunk(s))"
    fail=1
  else
    skipped=$((skipped + 1))
  fi
done < <(git diff --name-only --diff-filter=AM "$base" HEAD -- '*.rs')
if [ "$fail" = 0 ]; then
  echo "rustfmt OK: fork-touched files are formatted ($checked clean, $skipped already unformatted at the base)"
fi
exit $fail
