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

The initial three-lane stage excluded the contract suite. Fork CI
`42531331138` passed on `87ee16d96ad86d9dd8b815f1e010ea560be38308`.
Orchestration then authorized an ordinary fast-forward of custom to that SHA;
the push and remote identity were verified. No release or deploy occurred.

Combined local checks passed: diff whitespace, fork-only formatting (141 clean
files; seven pre-existing upstream-format exclusions), module declarations,
code-size/test-size/panic/swallowed-error budgets using scripts/fork overrides,
dependency boundaries and wildcard-reexport budget. No integration budget was
raised. The three supplied lane CI conclusions and SHA identities were also
verified directly through GitHub, including all five manual workflow builds.


## Contract and lifecycle integration

A second isolated stage starts from custom at `87ee16d96` and preserves:

- Final contract-suite head `8842425e6e19fef3596b56316f9f920fca3b9d98`.
- Repeated-load fix `1825e3bc3826e72faf2300e5bec7589a0eebb226`.
- Lost-steer fix `96cc79e704850455d0fce1b8cc0c96aa66e4c8c9`.

Repeated load previously opened a replacement daemon connection and emitted
another peer-turn start before dropping the old router. It now retains a live
attachment, router, active bracket and skill selection, gates peer actions
while requesting/replaying history, and opens a new connection after reader EOF.
The extended unit test exercises repeated history requests, captured scoped
Stop, retained skill/identity, and completed-reader reconnect.

Attachment identity intentionally follows the retained connection. Initial
load-fix CI `42532296659` exposed two old expectations that load/resume always
replace it. The corrected metadata test asserts stable identity for repeated
live load/resume and distinct identities for a different session and another
ACP process. The fixture changes only that repeated-load identity. Contract
wording now explicitly distinguishes retained attachments from EOF reconnects.

Lost-steer input was admitted to the daemon queue without transport ownership;
EOF never withdrew it, so it could inject after its adapter died. Busy input now
carries an internal daemon-generated connection owner, separate from opaque
caller IDs. Only accepted admissions capture their original queue controls;
teardown withdraws that owner from those queues and persisted pending input.
Injection drains under the same queue lock and remains untouched. Session
switches cannot redirect cleanup, and reconnecting transports get fresh owners.
Legacy persisted entries default to no owner. No daemon/ACP wire fields change.

Targeted ownership tests cover colliding caller IDs, other transport/session
queues, drained input, reconnection owners and rejected admission. Eleven
transport-filtered app-core tests and two persistence tests passed; the existing
two-producer insertion/withdrawal test also passed. The final retirement module
passed all four cases in 1.10 s, including the original actual-second-call
lost-steer repro, preserved inserted work, atomic busy rejection and intentional
peer configuration serialization. No peer-config production change is required.

Fresh named E2E checks on the combined candidate passed: faithful bash-wait
Stop (0.97 s), repeated active load (0.38 s), attachment identity (0.10 s), and
create/list/load fixture (0.21 s). Their three formerly ignored defect repros are
enabled here in a separate integration test commit. Production lane commits and
contract-generator commits retain their original attribution and history.

The internal queue metadata is a small cross-crate carry; legacy/system input
constructors remain unowned. Cleanup cannot retract input already consumed at a
safe point. Combined exact-head CI is reported separately after this stage is
pushed. This second stage has no authorization to advance custom or deploy.
