# Custom rebase onto upstream preview.199: research handoff

Prepared 2026-11-07. The requested rebase is **not complete**. This report
records the attempted integration and the architectural conflict to investigate.
No source changes from the attempt remain, and nothing was pushed.

## Fixed inputs and current state

| Item | Value |
| --- | --- |
| Fork | `camerontaylor/jcode`, branch `custom` |
| Original custom head | `39c7ecfda1c5067532a60ca69e5f1c1909b4ebee` |
| Upstream | `1jehuang/jcode` |
| Target prerelease | `v1.0.0-preview.199`, published 2026-11-06 15:01:12 UTC |
| Target commit | `7fd42339c907de223021bd63e5bb5df778c43a01` |
| Merge base | `7206efd0c1dbf49ef38d06b63f3a96a3a7cee611` |
| Local recovery ref | `backup/custom-before-preview199-20261107` |

The release was selected from GitHub's published, non-draft prereleases.
The original head had 53 non-merge custom commits beyond the target's history.
`git cherry` marked all 53 as patch-distinct; this does **not** establish that
their behavior is still needed. The custom-side diff from the merge base
covered 86 files, with 8,881 insertions and 1,109 deletions.

The rebase was aborted with `git rebase --abort`. Afterwards, `HEAD`,
`origin/custom`, and the recovery ref all resolved to the original custom head;
`git diff --exit-code` passed and the working tree was clean before this report.
No build or behavioral tests were run during the aborted attempt.

## Observed failure sequence

1. `git rebase v1.0.0-preview.199` stopped at
   `59d948f07` (`feat(acp): add /compact and /subagent; route RunSubagent through swarm spawn`).
   Conflicts were in `server/client_actions.rs` under app-core and
   `src/cli/acp.rs`. Upstream had extracted shell helpers and ACP tests into
   separate files. These require moving the custom additions to the new
   locations, preserving both sides' tests.
2. A provisional resolution continued to `65cd2126b` (`style: cargo fmt`).
   That formatting-only commit conflicted with the ACP test extraction and was
   skipped in this attempt. Formatting must be regenerated after the final port.
   The provisional resolution was not validated or retained.
3. The arm64 release commit and shared-session specification replayed, then
   `a8125eec5` (`feat(protocol): emit daemon turn lifecycle events`) conflicted
   in the ten files below. This revealed incompatible lifecycle designs.
4. The attempt was aborted. Later commits were **not** replayed, so the list
   below is the observed conflict set, not an exhaustive integration forecast.

All paths in this conflict table are relative to `crates/`:

| Conflicted path | Main concern |
| --- | --- |
| `jcode-app-core/src/server.rs` | Server module and lifecycle integration |
| `jcode-app-core/src/server/client_actions.rs` | Turn ownership and subagent execution |
| `jcode-app-core/src/server/client_lifecycle.rs` | Processing functions extracted upstream; acceptance/completion semantics changed |
| `jcode-app-core/src/server/comm_session.rs` | Server-initiated turn integration |
| `jcode-app-core/src/server/jade_relay.rs` | Relay turn integration |
| `jcode-app-core/src/server/live_turn.rs` | Live turn execution/publication |
| `jcode-app-core/src/server/turn_failover.rs` | Turn boundaries through failover |
| `jcode-protocol/src/lib.rs` | Protocol exports |
| `jcode-protocol/src/wire.rs` | Wire exports |
| `jcode-protocol/src/wire/events.rs` | Incompatible event definitions |

## Architectural conflict, with evidence

Read these files at the **target commit** using `git show <ref>:<path>`;
the working tree still contains the original custom implementation.

Upstream `crates/jcode-protocol/src/wire/events.rs` defines:

```rust
TurnIdentity { runtime_generation: String, turn_id: u64 }
TurnState { session_id, runtime_generation, turn_id, latest_turn_id }
TurnStarted { identity: TurnIdentity }
TurnEvent { identity: TurnIdentity, event: Box<ServerEvent> }
```

Custom uses the same `turn_started` wire name for a different payload:

```rust
TurnStarted { turn_id: String, origin: TurnOrigin, user_content: Option<String> }
TurnEnded { turn_id: String, status: TurnEndStatus, message: Option<String> }
```

Custom also adds `History.active_turn`, a `turn_lifecycle` capability, and an
independent turn registry. Its origin metadata includes client instance ID,
request ID, and actor kind. Upstream's start event above has no equivalent
origin or prompt fields. A mechanical union would create duplicate variants
and incompatible serialized payloads; choosing either side wholesale would
discard behavior.

Upstream provides runtime-scoped turn leases in
`crates/jcode-app-core/src/agent/turn_ownership.rs`. Its
`server/state.rs::TurnEventChannel` publishes a start event, wraps output with
the turn identity, and retains a publication pin while draining the event
queue. `server/session_manager.rs` provides canonical runtime ownership and
per-session reservations. Custom's lifecycle registry overlaps with these
mechanisms, but equivalence of cancellation, terminal ordering, attach
snapshots, and peer attribution has not been established.

Important upstream commits to read:

| Commit | Relevant change |
| --- | --- |
| `17a12d1ac` | Fence resumed observer events by runtime generation and turn identity |
| `cc0c333bb` | Serialize runtime turn ownership through execution and terminal publication |
| `d33832674` | Centralize canonical session runtimes and detach shared resume sources safely |
| `6629c4a7c` | Unwrap turn envelopes in legacy CLI adapters |
| `ce840273c` | Preserve ACP tool-input identity and skip empty updates |
| `dd6e7ce38` | Enable server account failover for new and resumed ACP sessions |
| `c5e816490` | Fill client_name in ACP Subscribe requests |

The upstream ACP reader and mapper unwrap turn envelopes. Determine where the
ported background router must retain identity **before** this unwrapping.
Preserve upstream's tool-input fixes and tests in `src/cli/acp/tests.rs`.

## What can be removed, and what must be preserved

The following are research conclusions to establish, not approved deletions:

| Custom work | Assessment and next step |
| --- | --- |
| `turn_lifecycle_registry.rs` and `server/turn_lifecycle.rs` | Candidates for replacement by upstream ownership/publication; map every caller and lifecycle guarantee first |
| Restart-unique IDs (`68e7a5931`) | Upstream runtime generation plus numeric identity appears to cover uniqueness; verify reconnect/replay behavior |
| Active-turn publication (`84d61da75`) | Compare custom registration ordering with upstream attachment `TurnState` and publication pin |
| Abort/panic lifecycle completion (`a15483479`) | Verify upstream terminal publication and cleanup; an ownership release alone does not prove clients receive completion |
| Subagent serialization (`4251e5881`) | Compare upstream lease coverage with the custom swarm-spawn path |
| ACP background routing and peer mirroring | Still needs preservation/adaptation; upstream protocol support alone does not implement this ACP feature |
| ACP `session/list` and shared catalog | Target ACP dispatch has no `session/list`; retain this feature |
| Stable message IDs and replay indices | Do not equate message identity with turn identity; preserve unless equivalent behavior is demonstrated |
| Model syntax, provider routing, effort and picker changes | Separate custom work; no redundancy conclusion was reached in this investigation |
| Fork updates and arm64 release workflow | Separate fork behavior; retain unless separately shown redundant |

Read `docs/ACP_SHARED_SESSIONS.md`, especially its implementation deviations;
its header still says “Proposed” despite implementation commits. Also inspect
`src/cli/acp/daemon_router.rs`, `src/cli/acp/session_configuration.rs`, and
`crates/jcode-harness-api-server/src/session_catalog.rs` on the original head.
The specification references Paseo consumer specs
`specs/acp-transformer-turn-lifecycle.md` and `specs/jcode-provider-plugin.md`;
their current contents and compatibility were not checked in this attempt.

## Research questions and suggested implementation direction

Prefer upstream as the turn ownership authority, then adapt custom ACP
features around it. This is a proposed direction, not a completed design.

1. Establish an explicit mapping from custom start/end/origin/active-turn state
   to upstream identity, attachment state, scoped terminal events and leases.
   Identify what metadata is genuinely missing and where it belongs.
2. Determine how an ACP prompt identifies its own accepted turn versus peer
   turns. Connection-local request IDs alone are insufficient. Account for
   queued prompts, cancels, daemon restarts and reattachments.
3. Determine how an attach replay is fenced from live events without losing,
   duplicating or reordering content, including attach-buffer overflow.
4. Establish terminal behavior for normal completion, failure, cancellation,
   panic, disconnect, swarm/subagent work and failover. Preserve the custom
   rule that closing ACP cancels only its own running turn.
5. Determine compatibility with older daemons and existing Paseo consumers;
   do not advertise the old custom capability for a changed event contract.
6. Produce a commit-level keep/adapt/drop inventory for all 53 custom commits.
   Drop custom infrastructure only once its guarantees are covered upstream.

## Reproduction and verification

Read-only starting points:

```bash
git merge-base custom v1.0.0-preview.199
git log --reverse --no-merges --oneline v1.0.0-preview.199..custom
git show a8125eec5
git show cc0c333bb
git show d33832674
git show v1.0.0-preview.199:crates/jcode-app-core/src/server/client_processing.rs
git show custom:src/cli/acp/daemon_router.rs
```

To reproduce the integration, use the fixed original head and target above.
An isolated worktree is justified for this investigation because replaying the
conflicts leaves the checkout unusable while the researcher studies the port.
The exact subsequent conflict sequence depends on how the first conflict and
formatting commit are resolved. No reusable, validated resolution was saved.

Run named tests under `nice` after checking `free -m` (at least 2 GB available).
Follow repository host/memory guidance; full gates belong in CI. Relevant
existing tests and coverage to preserve or adapt include:

- Protocol lifecycle serialization in
  `crates/jcode-protocol/src/protocol_tests/core_events.rs`.
- Custom lifecycle tests in
  `crates/jcode-app-core/src/server/turn_lifecycle_tests.rs`, alongside upstream
  ownership/session-manager tests and their terminal-publication assertions.
- ACP router unit tests, upstream ACP mapper tests, and custom model/effort
  capability/history tests. Account for upstream's extracted test modules.
- `tests/e2e/acp_shared_session.rs`, particularly
  `acp_client_mirrors_peer_turns_and_keeps_its_own_prompt_separate`.

Required behavioral evidence: concurrent ACP/TUI clients, mid-turn attach,
queued own/peer prompts, replay message-ID stability, own versus peer cancel,
close/disconnect, abort/panic cleanup, failover, restart/reconnect fencing,
catalog discovery, and model/effort preservation. Determine the repository's
actual test target and feature flags before invoking the tests above.

Runtime checks must use the new binary on an isolated socket. A build alone
does not update the shared daemon. Do not repoint or restart the shared daemon
for this research. Once the port is ready, run CI on the exact resulting head.

## Expected researcher handoff

Return a recommended protocol/adapter design, evidence for each redundant
custom component, the commit keep/adapt/drop inventory, a concrete port plan,
and remaining compatibility risks. If implementing, deliver a fully replayed
branch with upstream fixes preserved, redundant infrastructure removed,
targeted verification results, and a comparison against the original custom
behavior. Do not treat “all conflicts resolved” as proof of behavioral parity.
