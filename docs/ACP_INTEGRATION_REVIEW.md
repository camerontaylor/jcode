# ACP integration staging review

Reviewed on atlas, 2026-11-09. Staging base is `origin/custom` at
`0f9a3e5de`; source commits are preserved through merge commits.

| Lane | Reviewed head | Result |
| --- | --- | --- |
| Release/cache | `204872da2` | No concrete correctness blocker found |
| Installed-binary post-install reload | `b4e71cdcee03f3008f4a10b52d44ca3f6484edce` | No concrete correctness blocker found |
| Shared-session Stop | `39fb7620589835d0c7f1a47653ab74f594b56eaa` | No additional correctness blocker found |

This is a manual source review, including surrounding startup, executable
selection, reload fanout and turn-claim/reset code. CodeRabbit CLI was absent
on atlas, so no CodeRabbit analysis ran. Stop is a self-review of the earlier
fix, not an independent third-party review. There were no accepted findings
requiring an additional production fix.

## Concrete checks and behavior

- Cache warming runs on custom without release provenance or publication.
  Build uses an explicit failure/cancellation condition so skipped preflight
  does not suppress warming. Publication requires successful preflight and
  build, and a release event. Tag builds restore the default-branch caches;
  only custom writes them. The repository default branch is custom.
- Native Linux packages preserve the launcher-plus-binary layout expected by
  the installer. The manually selected portable artifact uses a different
  artifact prefix and is excluded from release artifact download.
- Updater executable selection uses `client_update_candidate(false)` before
  falling back to current_exe. The installed executable's hidden probe exits
  before normal startup side effects. Unsupported-clap exit 2 and spawn
  failures permit old-path fallback. Timeout/crash after reload begins does
  not trigger duplicate fanout. The normal hidden command also exits before
  application/provider startup and calls reload_all from its own executable.
- Stop retains the observed peer identity. Identity-aware peer cancellation
  stays armed while the turn finishes; the next turn claim clears old
  cancellation under the identity lock. Legacy callers retain their prior
  epoch-guarded reset. Stop and post-install changes have no overlapping
  production edits. Workflow edits merge in separate cache and test hunks.

## Evidence and limits

Lane evidence supplied by orchestration: cache Fork CI `42523721421`,
manual release-workflow builds `42523722074` (five builds), and reexec Fork CI
`42525588676`, all successful. Stop Fork CI `42526665518` also passed.
The Stop regression failed before the fix and passed after it; 15 targeted
turn-channel and 127 ACP tests passed. Private raw-client wire evidence and
fixed-binary provenance are in `ACP_E2E_DEFECT_LOCALIZATION.md`.

Compatibility risk: native Linux release binaries require glibc 2.39 rather
than the former 2.17 baseline. The portable build is manual and is not a
release asset. Cache warming adds four native builds on code pushes to
custom. Post-install reload intentionally suppresses a second fanout after
an indeterminate partial reload; it can therefore leave an incomplete reload
for the user to diagnose from stderr. These are visible design choices, not
new findings in this integration.

The separately developed contract suite is excluded. This staging task does
not advance custom, publish a release or deploy binaries. Combined exact-head
Fork CI is reported to orchestration after staging is pushed.

Combined local checks passed: diff whitespace, fork-only formatting (141 clean
files; seven pre-existing upstream-format exclusions), module declarations,
code-size/test-size/panic/swallowed-error budgets using scripts/fork overrides,
dependency boundaries and wildcard-reexport budget. No integration budget was
raised. The three supplied lane CI conclusions and SHA identities were also
verified directly through GitHub, including all five manual workflow builds.
