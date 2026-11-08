# Phase 1 cancellation repair verification

## Findings and changes

- **D1**: `server/client_processing.rs:478` snapshots the dispatch target,
  including for unscoped Stop. After publication awaits,
  `agent/turn_ownership.rs:65` revalidates identity and signals under the same
  state lock used by claim, retirement and release. An idle snapshot cannot
  acquire a future turn. `TurnStopped` remains ahead of the signal.
- **D2**: `agent.rs:388` clears preexisting cancellation inside `claim_after`,
  before exposing the identity. The epoch guard preserves a racing unscoped
  fire. The fork also captures its cancellation-classification epoch there,
  rather than after exposing the identity.
- **D3 remains pending R1**: `server/client_lifecycle.rs:85` negotiates only
  identity support; it does not negotiate individual lifecycle variants.
  Suppressing `turn_ended` for identity-aware readers would break the present
  ACP adapter. No new protocol capability was added in this corrective lane.
  R1 removes the variant and this compatibility problem.
- **Terminal audit**: completion/error/panic/abort already drain queued output
  under a publication pin, preventing successor handoff before the terminal.
  Two uniqueness defects were found: repeated producer terminals and a direct
  cancellation `Done` duplicating the ordered channel's `Done` for its requester.
  The drain now tracks terminal publication separately from lifecycle retirement
  (abort retires before its fallback terminal); local cancellation preserves
  `Interrupted` before signalling but leaves `Done` to the channel.

- **SDK ownership follow-up**: `server/client_session.rs:1498` and `:1770`
  release a connection's SDK callback binding before successful live/persisted
  resume publishes completion. The existing SDK socket regression failed on
  this head before the fix; one isolated `origin/custom` run passed. Inspection
  found no release on resume, and adding it fixes the observed failure. This is
  kept separate from D1/D2. Stop notification compatibility was also checked:
  an intermediate omission of `Interrupted` failed two socket tests; restoring
  it before signalling fixes both without reintroducing duplicate `Done`.

Paths above are relative to `crates/jcode-app-core/src/`.

## Branches

- `pr/stale-cancel-flag`: `a646903e4` regression first; `86db359cf` D2 repair.
- `pr/turn-scoped-cancel`: `93bfbccc6` regression first (a shim retains old
  dispatch semantics); `26ff594f2` D1 repair.
- `fork/p1-cancel-race`: merge-forward commits `e75f99079` and `203612818`;
  terminal/steering tests `814a8030f`; terminal repair `21dcdef7c`; preserved
  Stop notification `84bcb3936`; SDK owner release `ffcfeeb42`.

The D2 merge adapts the fork-only classification probe and new message field.
Terminal changes stay on the topic branch because this audit covers the fork's
`TurnEnded` bookkeeping and publication channel together. No changes were
merged into `custom`; no shared daemon was installed or restarted.

## Reproduction commands and evidence

All successful executable builds below use this environment prefix on atlas:

```sh
export PATH=/home/ctaylor/.cache/jcode-p1-race/bin:$PATH
export CARGO_BUILD_RUSTC_WRAPPER=
export CARGO_TARGET_DIR=/home/ctaylor/.cache/jcode-p1-race/target
```

The `bin/cc` wrapper executes `/usr/bin/cc -fuse-ld=mold "$@"`.
An initial build was killed with SIGKILL. A retry with GNU ld reached the
5 GiB scope limit and was killed. The user scope bus subsequently refused
connections; subsequent runs use `nice`, two jobs and memory checks. The cache
was moved off RAM-backed `/tmp`. Neither failed build counts as test evidence.
When changing worktree source roots with the shared target cache, local source
timestamps were invalidated to avoid stale protocol artifacts.

| Command (after the prefix above) | Before | After |
| --- | --- | --- |
| `nice -n 10 cargo test -j 2 -p jcode-app-core cancel_after_turn_claim_is_preserved --lib` | Fails: cancellation after claim erased | Passes on PR branch and merged fork |
| `nice -n 10 cargo test -j 2 -p jcode-app-core cancellation_dispatch_cannot_signal_a_successor --lib` | Fails: successor signal set, mode 0 | Passes scoped, unscoped and idle modes |
| `nice -n 10 cargo test -j 2 -p jcode-app-core duplicate_producer_terminals_are_published_once --lib` | Fails: duplicate terminal | Passes in module run |
| `nice -n 10 cargo test -j 2 -p jcode-app-core turn_event_channel_tests --lib` | 9 pass, 3 fail: duplicate success/error and requester Done | All 12 pass in lifecycle module run |
| `nice -n 10 cargo test -j 2 -p jcode-app-core server::client_lifecycle::tests --lib` | PR branch baseline | D2 branch: 37 pass |
| `nice -n 10 cargo test -j 2 -p jcode-app-core server::client_lifecycle --lib` | Expanded fork check | Final: 56 pass (after separate SDK follow-up) |
| `nice -n 10 cargo test -j 2 -p jcode-app-core server::client_session::tests --lib` | — | 44 pass |
| `nice -n 10 cargo test -j 2 -p jcode-app-core agent::turn_ownership::tests --lib` | — | 3 pass |
| `nice -n 10 cargo test -j 2 -p jcode-app-core sdk_socket_configure_list_callback_and_busy_rejection --lib` | Final-source before SDK follow-up: fails callback ownership assertion; isolated origin/custom: passes | Included in final 56 passing lifecycle tests |
| `nice -n 10 cargo test -j 2 -p jcode daemon_router::tests --lib` | — | 37 pass (`src/cli/acp/daemon_router_tests.rs`) |
| `nice -n 10 cargo test -j 2 -p jcode --test e2e acp_shared_session` | — | 1 pass, 7.13 seconds |



The D1 before-run used the committed regression's delegating shim on the fork
with the original cancellation implementation, before merging D1. D2's
before-run used the regression-only `pr/stale-cancel-flag` commit. Terminal
before-runs used `814a8030f`, before terminal fixes.

The gate-based `two_steering_producers_withdraw_on_either_side_of_insertion`
uses daemon queue/withdrawal dispatch and drops attachment receivers before and
after insertion. It verifies one producer's pending input is removed, the
other producer's inserted input survives withdrawal and observer departure,
and the remaining observer receives a completed terminal. Existing router
close tests and the shared-session socket e2e cover the adapter boundary.
New tests contain no sleeps; existing timeouts are failure bounds.

## Recorded pre-fix failures

```text
cancel_after_turn_claim_is_preserved:
  a cancellation after claim must survive preparation
  FAILED. 0 passed; 1 failed

cancellation_dispatch_cannot_signal_a_successor:
  dispatch must not signal the successor (mode=0)
  FAILED. 0 passed; 1 failed

duplicate_producer_terminals_are_published_once:
  exactly one terminal is published
  FAILED. 0 passed; 1 failed

turn_event_channel_tests before terminal repairs:
  cancel_turn_stopped_is_scoped_to_the_active_turn_for_every_attachment:
    the requester and observers each receive exactly one terminal
    left: 2, right: 1
  duplicate_producer_terminals_are_published_once: exactly one terminal is published
  failed_producer_terminal_is_published_once_after_output: exactly one terminal is published
  FAILED. 9 passed; 3 failed
```

The first router command used the filename filter `daemon_router_tests` and
matched zero tests; it is not counted. The corrected module filter shown in
the table executes all 37 tests.

Final named verification: 56 lifecycle, 44 session, 3 ownership, 37 ACP router
and 1 shared-session e2e tests pass. No full suite was run locally. D3 is the
remaining compatibility item for R1. The receiver-drop steering harness is
not a claim of a new real-network transport-loss test; the existing socket
e2e and router close tests cover that adapter boundary separately.
