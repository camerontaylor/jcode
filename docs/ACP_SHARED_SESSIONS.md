# ACP Shared Sessions

Status: Implemented (2026-11-07, ported onto upstream turn identity at
`v1.0.0-preview.199`; see [Port onto upstream turn identity](#port-onto-upstream-turn-identity-2026-11-07))

The ACP-facing contract (`_jcode/turn`, `session/list`, `messageId`s) is as
designed below. The daemon wire contract is not: D1's fork-specific
`turn_started { turn_id: String }`, `History.server_features` and
`History.active_turn` were replaced by upstream's `TurnIdentity` envelopes plus
an additive origin extension. Deviations 34–47 describe the contract as it now
stands and take precedence over the design sections and earlier deviations.

Let an ACP client (Paseo, Zed, …) and any number of jcode TUI windows attach to
the same live daemon session at once. Every attached client sees every turn,
whichever client started it.

Companion Paseo specs (in the Paseo fork): `specs/acp-transformer-turn-lifecycle.md`
(core hook) and `specs/jcode-provider-plugin.md` (plugin that consumes this
work).

See also: [`SERVER_ARCHITECTURE.md`](./SERVER_ARCHITECTURE.md),
[`MULTI_SESSION_CLIENT_ARCHITECTURE.md`](./MULTI_SESSION_CLIENT_ARCHITECTURE.md).

## Summary

The daemon already supports several clients on one session. The ACP adapter
(`src/cli/acp.rs`) does not. It reads daemon events only while one of its own
prompts is running, so events from a turn another client started sit in the
socket and are then reported as part of the adapter's next prompt.

This spec covers three things:

1. Add explicit, origin-tagged turn lifecycle events to the daemon protocol
   (additive).
2. Restructure the ACP adapter around a background reader that sends each event
   either to the adapter's own prompt or to a peer-turn mirror.
3. Implement ACP `session/list` so clients can find a session to attach to.

## Current behaviour (verified 2026-11-04)

A probe with two raw protocol clients on an isolated daemon
(`jcode --socket … --provider cerebras serve`) found:

| Scenario | Prompting client | Watching client |
|---|---|---|
| B subscribes with A's live `target_session_id` | — | `done`; same `session_id`; no takeover |
| A prompts | full stream, `done 10` | `peer_user_message`, the same stream, **`done 10`** |
| B prompts during A's turn | both complete, run one after the other | both streams reach both clients, with no boundary between them |

Relevant code:

- **Live attach adds a client.** If the target session is live,
  `handle_resume_session` (`crates/jcode-app-core/src/server/client_session.rs`,
  via `claim_live_target_agent` at :1355) registers one more event sender and
  does not reject the attach. The sender list is `SwarmMember::event_txs`
  (`server/state.rs:212`, `register_session_event_sender` at :385).
- **Turn streams fan out.** `start_processing_message` streams through
  `session_event_fanout_sender_with_fallback` (`server/state.rs:511`), so every
  attachment receives the turn. The terminal `Done`/`Error` is sent on the same
  ordered channel on purpose (comment above `tx.send(terminal_event)` in
  `server/client_lifecycle.rs`), so peers receive it **with the originator's
  connection-local request id**.
- **Peer prompts.** Peers receive `PeerUserMessage { content }`
  (`client_lifecycle.rs:1236`). It is sent when the request is *accepted*, not
  when the turn acquires the agent, so it doesn't mark a turn boundary when
  turns overlap.
- **Cancel is session-wide.** A `Cancel` from a connection that doesn't own the
  running turn still fires the session's cancel signals through the
  turn-cancel registry (`client_lifecycle.rs`, `SERVER_INTERRUPT_CANCEL_NO_LOCAL_TASK`
  branch; `SessionControlHandle::request_cancel`, `state.rs:750`).
- **The ACP adapter only reads during its own prompt.** `DaemonSession::read_event`
  (`src/cli/acp.rs:256`) is called only from request/response helpers and
  `stream_prompt_events`. It also fails on any line it can't decode, whereas the
  TUI skips such lines (`crates/jcode-tui/src/tui/backend.rs:1378`).
- **Shared ACP instance id.** ACP uses the fixed `client_instance_id: "acp"`
  (`src/cli/acp.rs:811`, `:870`).
- **No `session/list`.** The ACP method table (`src/cli/acp.rs:340`) has
  new/load/resume/prompt/cancel/close and no list.

## Goals

- G1. An ACP client attached to a live session shows turns started by other
  clients (TUI, another ACP client, swarm wakes), with correct start and end.
- G2. An ACP `session/prompt` result covers exactly its own turn.
- G3. ACP clients can find sessions to attach to (`session/list`).
- G4. Existing clients keep working against both new and old daemons.

## Non-goals

- TUI changes. The TUI already handles `PeerUserMessage` and turns started by
  other clients.
- Session-scoped MCP servers for ACP, or per-client tool sets.
- Paseo-side work (see the companion specs).

## Design

### D1. Daemon: turn lifecycle events (protocol, additive)

> Superseded by upstream turn identity; see Deviations 34–37. Kept for the
> rationale.

Add two variants to `ServerEvent` (`crates/jcode-protocol/src/wire/events.rs`):

```rust
#[serde(rename = "turn_started")]
TurnStarted {
    /// Daemon-unique, opaque. Stable for the lifetime of the turn.
    turn_id: String,
    origin: TurnOrigin,
    /// The user-visible prompt that started the turn, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_content: Option<String>,
},

#[serde(rename = "turn_ended")]
TurnEnded {
    turn_id: String,
    status: TurnEndStatus, // completed | failed | cancelled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
},
```

```rust
pub struct TurnOrigin {
    pub kind: TurnOriginKind, // client | swarm | background | relay | debug
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_instance_id: Option<String>,
    /// The originating connection's request id (connection-local).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
}
```

Emission rules:

- Emit at the **logical turn** level, after the agent lock is held. That way,
  turns that queue on the agent mutex emit `TurnStarted` only when they actually
  start, and lifecycle pairs never overlap within a session. Do not emit from
  `process_locked_message_streaming_mpsc` itself: failover retries call it more
  than once per logical turn.
- Call sites (use one shared helper, e.g. `with_turn_lifecycle(event_tx, origin,
  user_content, |agent| …)`):
  - `turn_failover::process_message_with_server_failover`, after the
    `agent.lock()` at `turn_failover.rs:176`;
  - `turn_failover::process_locked_message_with_server_failover`
    (`turn_failover.rs:183`; used by `live_turn.rs:143` wake turns);
  - `jade_relay.rs:1222`, `:1253`; `client_actions.rs:1350`.

  The implementer must confirm this list is complete. Every path that streams a
  model turn to `event_tx` must emit the pair. Add a test that fails for any
  unwrapped path, for example a debug assertion on `TurnStarted` before the
  first stream event of a turn.
- Send both events on the **same ordered sender** as the stream. Send
  `TurnEnded` before the existing terminal `Done`/`Error`, and before
  `TurnStopped` and `SoftInterruptQueue`. Keep `Done`/`Error` and
  `PeerUserMessage` unchanged so existing clients see no difference.
- `TurnEndStatus::Cancelled` when the turn's stop reason is `Interrupted`.

Feature detection and mid-turn attach:

- Add `#[serde(default)] server_features: Vec<String>` to `ServerEvent::History`
  and include `"turn_lifecycle"`. Clients use this flag, not version
  comparisons, because the shared-server daemon can lag the client binary.
- Add `#[serde(default)] active_turn: Option<ActiveTurn { turn_id, origin,
  user_content }>` to `History`, so a client that attaches mid-turn can open
  the turn that is already running.

Compatibility: the TUI skips undecodable lines, so older TUIs ignore the new
variants. Older ACP adapters fail on unknown lines (see Current behaviour), but
they ship in the same binary as the matching daemon, so they never see these
events before D2 lands. The exception is a newer daemon serving an older
`jcode acp` process. D2's unknown-line tolerance closes that gap from now on.

### D2. ACP adapter: background reader and event routing

Replace pull-based reading in `DaemonSession` with a reader task, one per daemon
connection:

1. **Decoding.** Read newline-delimited events. Log lines that don't decode as
   `ServerEvent` at debug level and skip them. Never let one bad line end the
   session.
2. **Request ids.** Start each connection's request ids at a random 32-bit
   offset shifted into the high half of `u64`. Peer `Done`/`Error` events carry
   the *originator's* request id, and TUI ids start near zero, so this avoids
   collisions. As a second safeguard, ignore any `Done`/`Error` whose id isn't
   pending (log at debug).
3. **Reply routing.** A pending-request table maps `id → sender`. `Ack`, `Done`
   and `Error` with a pending id go to the waiter. Replies without an id that
   current helpers wait for (`History`, model catalog, `ModelChanged`, compact
   result, …) go to the oldest waiter that registered for that event kind. If
   no waiter matches, they are session-state updates (D2.6).
4. **Turn tracking**, when `server_features` contains `turn_lifecycle`:
   - On `TurnStarted`, the turn is **own** when `origin.kind == client`,
     `origin.client_instance_id` equals this adapter's instance id, and
     `origin.request_id` is one of its pending prompt ids. Otherwise it is a
     **peer** turn.
   - Stream events between `TurnStarted` and `TurnEnded` belong to that turn.
     Own-turn events go to the waiting `session/prompt` (the existing
     `stream_prompt_events` logic, now fed from a channel). Peer-turn events go
     to the peer mirror (D3).
   - Stream events with no open turn (should not happen) are logged and
     dropped.
   - Ignore `PeerUserMessage`; `TurnStarted.user_content` replaces it.
   - On attach, open `History.active_turn` as a peer turn when present.
5. **Legacy daemons** (no `turn_lifecycle`): keep today's behaviour for own
   prompts, but also **drain** events that arrive while no prompt is running,
   and drop them. This fixes the misattribution bug without mirroring peer
   turns.
6. **Session state from peers.** Apply `ModelChanged`, effort changes and
   `SessionRenamed` to `SessionUiState` whenever they arrive, then send the
   existing `config_option_update` / `session_info_update`. Today these updates
   are only seen when the adapter itself made the change.
7. **Instance id.** Use `client_instance_id = "acp-<uuid v4>"`, generated once
   per adapter process, in place of the shared `"acp"`.

### D3. ACP adapter: mirroring peer turns

For each peer turn, send to the ACP client in wire order:

1. The extension notification `_jcode/turn`:
   ```json
   {"sessionId": "...", "turnId": "...", "state": "started",
    "origin": {"kind": "client", "clientInstanceId": "..."}}
   ```
2. If `user_content` is non-empty: `session/update` with `user_message_chunk`.
3. The turn's stream, mapped through the existing `EventMapper`
   (`agent_message_chunk`, `agent_thought_chunk`, `tool_call`,
   `tool_call_update`, `usage_update`, …), exactly as for own turns.
4. On `TurnEnded`: `_jcode/turn` with `state` set to `completed`, `failed`
   (with `error.message`) or `canceled`.

Rules:

- Send `_jcode/turn` in **all** profiles. ACP extension notifications
  (`_`-prefixed) are ignored by clients that don't recognise them, and standard
  clients still get the content through `session/update`.
- Send `_jcode/turn` **only for peer turns**. The `session/prompt`
  request/response already brackets the client's own turns.
- The `extended`/`full` profile firehose (`_jcode/server_event`) is unchanged.

### D4. Prompt and cancel semantics on a shared session

- **`session/prompt` while a peer turn is running.** Send the `Message`
  immediately. The daemon queues it on the agent mutex, and its `TurnStarted`
  arrives after the peer's `TurnEnded`. The existing per-ACP-session
  `prompt_running` guard (`acp.rs:541`) is unchanged.
- **`session/cancel` stops the whole session.** Send `Cancel`. That interrupts
  the session's running turn even if a peer owns it (current daemon behaviour),
  and aborts this adapter's own prompt if it is still waiting for the agent.
  Reason: in a client that mirrors peer turns, a stop button that leaves a
  visibly running turn untouched is broken UX. A TUI user sees the usual
  "interrupted" notice.
- **`session/close` does not stop a peer's turn.** Closing the ACP view is not
  a user cancel: send `Cancel` only when *this connection's own turn is
  running* (`router.own_turn` set). A prompt that is merely queued behind a
  peer's turn does not qualify — the peer holds the daemon's agent, so a
  session-wide `Cancel` would interrupt it, which is the bug this rule exists
  to prevent. With a queued prompt the connection is simply closed: the daemon
  detaches this connection without touching the peer's turn, and our queued
  turn is aborted by disconnect cleanup on a solo session or detached (its
  output discarded) when another client keeps the session alive. Dropping the
  session also stops its reader, so no further peer output is written for a
  closed view.

### D5. `session/list`

- Advertise `agentCapabilities.sessionCapabilities.list = {}` in
  `initialize_result` (`acp.rs:1702`).
- Request: `{ cwd?: string, cursor?: string }`. Response:
  `{ sessions: [{ sessionId, cwd, title?, updatedAt? , _meta?: { jcode: { live:
  bool } } }], nextCursor? }`.
- Data source: persisted sessions plus live ones, as in the harness API's
  `list_sessions` (`crates/jcode-harness-api-server/src/translate.rs:772`).
  Factor the stored-id and metadata loading into a helper that both callers
  share, rather than duplicating it.
- When `cwd` is given, keep sessions whose working directory matches it after
  canonicalisation. Sort by `updatedAt` descending. Pages hold 50 entries;
  `cursor` is opaque (encode the last `(updatedAt, sessionId)` pair).
- Exclude headless swarm workers by default. `_meta.jcode.live` is
  best-effort: if the adapter can't cheaply ask the daemon which sessions are
  live, omit it (see Open questions).

## Implementation phases

| Phase | Scope | Can ship alone |
|---|---|---|
| 1 | D1 daemon lifecycle events, `server_features`, `active_turn` | Yes; invisible to current clients |
| 2 | D2 reader/router, legacy draining, id offset, unique instance id | Yes; fixes misattribution even on old daemons |
| 3 | D3 mirroring plus D4 semantics | Needs 1 and 2 |
| 4 | D5 `session/list` | Independent of 1–3 |

## Testing

- **Protocol:** round-trip and backward-compatibility tests for the new events
  and `History` fields in `crates/jcode-protocol/src/protocol_tests/`. History
  without `server_features` or `active_turn` must still decode.
- **Daemon:** two connections on one session, one prompting. Assert that both
  receive `TurnStarted → stream → TurnEnded → Done`, in that order. With two
  overlapping prompts, assert the lifecycle pairs don't nest and each pair's
  `origin.request_id` matches its originator.
- **ACP router unit tests** (scripted `ServerEvent` sequences, no daemon):
  - own turn;
  - peer turn while idle;
  - peer turn queued behind an own turn;
  - own prompt queued behind a peer turn;
  - peer `Done` whose id collides with a pending own id;
  - an undecodable line;
  - a legacy daemon (no feature flag): idle events are drained;
  - mid-turn attach via `active_turn`.
- **End-to-end** (`tests/e2e/`, reusing `test_support` subscribe helpers): port
  the three probe scenarios above with an ACP client in place of client B.
  Assert that the ACP `session/prompt` result contains only its own text, and
  that peer turns produce `_jcode/turn` started/ended around their
  `session/update`s.
- **Manual:** TUI plus Paseo (jcode plugin) on one session (see the plugin
  spec's acceptance criteria).

## Open questions

1. **Live flag for `session/list`.** Can the adapter get the live set cheaply
   over the daemon socket, or does this need a small request (for example
   reusing the comm/swarm session list)? `_meta.jcode.live` is optional until
   this is settled.
2. **Swarm and auto-poke turns.** Confirm these should appear in ACP clients as
   peer turns (`origin.kind = swarm | background`). The alternative is
   filtering them out. Current proposal: mirror them, since they change the
   transcript the ACP client shows.
3. **Leaking `user_content` to peers.** It holds the full prompt text sent to
   every attachment. That matches `PeerUserMessage` today; confirm no prompt
   source (for example a hidden system reminder) should be redacted.

## Deviations

Recorded while implementing phases 1–4. Each is the smallest sound change the
real code allowed; none of them weakens the goals.

1. **The D1 call-site list was incomplete.** Besides
   `process_message_with_server_failover`,
   `process_locked_message_with_server_failover`, `jade_relay.rs` and
   `client_actions.rs`, two more paths stream a turn:
   `live_turn.rs` runs `run_once_streaming_mpsc_with_display_role` directly for
   display-role wake turns (bypassing the failover wrapper), and
   `client_actions::handle_agent_task` (`agent_task`) goes through
   `process_message_streaming_mpsc`. All four entry points now emit the pair;
   `process_message_with_server_failover` takes the agent lock itself and calls
   the locked primitive, so a logical turn emits one pair and failover retries
   stay inside it.
2. **Detecting a cancelled turn.** The streaming turn loop `break`s instead of
   returning an error when its cancel signal fires, so the result is `Ok`. The
   pair reports `cancelled` when the agent's cancel-signal *epoch* advanced
   during the turn — the same signal the loop checks. A graceful daemon
   shutdown fires that signal too and would also read as `cancelled`; today
   only a debug command does that.
3. **Cancel ordering.** `cancel_processing_message` publishes its
   `TurnStopped { Interrupted }` notice *before* signalling the turn (by
   design, so a cooperative finish cannot race it). A cancelled turn's stream is
   therefore `TurnStarted … TurnStopped … TurnEnded(cancelled) … Done`.
   `TurnEnded` still precedes the terminal `Done`/`Error` and
   `SoftInterruptQueue`; moving it before the pre-cancel notice would mean
   changing existing cancel behaviour.
4. **Wire field names.** `TurnOrigin` uses snake_case on the daemon wire
   (`client_instance_id`, `request_id`) like every other protocol field, while
   the ACP `_jcode/turn` extension uses the spec's camelCase
   (`clientInstanceId`).
5. **Peer terminal events with colliding ids.** The spec says to ignore a
   `Done`/`Error` whose id is not pending. That is not sufficient on its own: a
   peer turn's completion carries the *originator's* connection-local id, which
   can collide with one of ours. The router remembers the last peer turn's
   request id and consumes the first terminal event carrying it, so a peer
   completion cannot finish this adapter's prompt while the adapter's own
   terminal event with the same id still lands.
6. **`session/list` liveness** comes from the `all_sessions` snapshot in the
   `History` events the adapter already receives, not from a new daemon
   request (Open question 1). `_meta.jcode.live` is omitted until such a
   snapshot exists.
7. **Headless swarm workers are not excluded** from `session/list`. The
   persisted catalog cannot distinguish them from any other stored session, and
   the daemon protocol exposes no cheap headless flag to the adapter (Open
   question 1). This is the one piece of D5 left unimplemented.
8. **Shared catalog helper location.** The stored-id/metadata/title loading
   lives in a new public module
   `jcode_harness_api_server::session_catalog`, not inside `translate.rs`,
   because the ACP adapter lives in the `jcode` binary crate and must call it.
   `translate.rs`'s `list_sessions` handler now uses
   `load_session_metadata`/`stored_session_ids`/`saved_session_ids` from that
   module instead of its own copy.
9. **Mid-turn attach ordering.** `History.active_turn` opens the peer turn as
   soon as the attach `History` arrives, so `_jcode/turn` `started` can precede
   the `session/load` response on the wire. Buffering it until after the
   response would instead drop peer stream events that arrive in between.
10. **Prompts that produce no turn.** A `session/prompt` that never streams a
    model turn (a slash command, `RunSubagent`) has no lifecycle pair, so its
    `Done`/`Error` is routed to the prompt by pending request id in every mode,
    not only on legacy daemons.
11. **No new dependency for randomness.** `client_instance_id` (`acp-<uuid
    v4>`) and the request-id base are derived from `RandomState` hashing, which
    is OS-seeded, rather than adding `rand`/`uuid` to the binary.
12. **No debug assertion for unwrapped turn paths.** The spec suggested one;
    it would fire for agent-internal turns (`jcode run`, agent tests) that never
    touch the daemon stream. Coverage is instead the two-connection daemon test
    (ordering, non-nesting, per-origin request ids) plus the fact that the
    events are emitted from one shared helper at four entry points.

### Review follow-up (2026-11-05)

A review of the implementation requested changes. These deviations supersede or
extend the numbered list above where they overlap.

13. **The lifecycle pair is closed from `Drop`, not only from `finish`.** A turn
    aborted after its cancel grace period, or one that unwinds through a panic,
    never reached `finish`, so every attachment kept a turn that never ended and
    a peer mirror kept its `_jcode/turn` bracket open. `TurnLifecycle` now owns
    the ordered sender and emits `TurnEnded` from `Drop`: `cancelled` when the
    future is dropped (abort), `failed` when it is unwinding (panic). This
    supersedes deviation 2 for the aborted case: an aborted turn is now reported
    as `cancelled` rather than not reported at all.
14. **`/subagent` launches carry their own lifecycle pair.** `RunSubagent`
    streamed `ToolStart`/`ToolInput`/`ToolExec`/`ToolDone` on the requesting
    connection with no turn boundaries, so a lifecycle-aware client dropped them
    as events belonging to no open turn. The launch now runs inside a
    *connection-local* pair (`start_connection_local`): it is emitted on that
    connection only and is deliberately not published as the session's active
    turn, because a `/subagent` launch is not the session's logical turn.
15. **Peer turns are closed defensively.** A new `TurnStarted` (own or peer)
    closes any peer turn that never reported its end, and a peer's terminal
    `done`/`error` closes it too. This supersedes the marker scheme in
    deviation 5: peer request ids are tracked per turn
    (`(turn_id, request_id)`) and expire when that turn's terminal event is
    consumed, or when one of our own turns completes. A peer terminal is never
    cleared early by our own `TurnStarted`, which is what made the old
    single-marker scheme racy when the daemon reported a finished turn after the
    queued turn had already started.
16. **`session/close` no longer cancels a peer's turn.** See D4. Only a turn
    this connection is actually running is cancelled on close; a prompt that is
    only queued behind a peer's turn is left to the daemon's disconnect
    handling.
17. **The reader no longer keeps its session alive.** The reader task holds the
    shared router and UI state instead of an `Arc<DaemonSession>`; previously
    `Drop` (which aborts the reader) could never run, so closing a session
    leaked its socket, its daemon attachment and its peer mirroring. `close()`
    on the router also ends an open peer turn and refuses new waiters.
18. **Per-connection client instance ids.** The instance id is
    `{process_instance_id}/{n}`, not per process: one adapter process attaching
    the same session twice would otherwise recognise its other connection's turn
    as its own. `client_instance_id` plus `request_id` remains enough to
    identify a turn's origin; no protocol change was needed.
19. **Peer output is held during an attach handshake.** A mid-turn attach
    replayed history while the reader was already writing live peer events, so
    the two interleaved. Peer actions are buffered in a bounded gate (512
    actions, oldest dropped) and flushed after the replay and before the
    `session/load` response; session state is still applied immediately. This
    supersedes the ordering note in deviation 9, which only considered the
    `_jcode/turn`-before-response question.
20. **Peer turns report `usage_update`.** `TokenUsage` from a mirrored turn was
    dropped because only the own-prompt loop translated it. One helper now
    serves both paths.
21. **`session/list` requires a working directory.** Entries without one are
    omitted (`cwd` must be a string), and the cwd filter canonicalises at most
    once per distinct directory, skipping the syscall entirely on an exact
    match. `_meta.jcode.live` remains a best-effort snapshot of
    `History.all_sessions`, taken when the adapter last saw a `History`; the
    daemon protocol has no cheaper live-set request (Open question 1).
22. **Reply routing is id-based; the "oldest waiter by kind" rule is not
    needed.** Every reply the adapter waits for (`Ack`, `Done`, `Error`,
    `History`, `ModelChanged`, `ReasoningEffortChanged`, `CompactResult`)
    carries the request id it answers, so the pending-request table alone routes
    them. There is no id-less reply to disambiguate by kind.
23. **The attach flush is atomic with respect to live peer output.** Marking
    the gate ready and draining it happen under the same guard, and live
    dispatch takes that guard, so a live peer event can only render after the
    last held one. Previously the lock was released before rendering, and a
    live delta could land between held actions.
24. **Gate overflow never drops a `_jcode/turn` bracket.** Only streamed
    `PeerTurnEvent` entries are evicted; a `PeerTurnStarted`/`PeerTurnEnded` is
    kept, so a saturated mid-turn attach still yields a balanced bracket (while
    the buffer holds brackets only, new events are dropped instead).
25. **Peer terminal markers expire with their turn.** A peer turn closed by
    synthesis (the next turn started before its end) or by a cancelled
    `turn_ended` will never deliver its own terminal event to this connection —
    the daemon sends a cancelled turn's terminal only to the cancelling
    connection — so its `(turn_id, request_id)` marker is dropped then. A marker
    that survived would swallow a later terminal event reusing that
    connection-local id.
26. **`/subagent` launches are serialised with model turns.** The launch holds
    the agent guard for its whole duration, so a queued or peer model turn
    cannot start in the middle and interleave its fanned-out stream with the
    launch's connection-local tool events (which the adapter would otherwise
    mirror as the peer turn's output). The `swarm` tool never re-enters this
    session's agent, and the spawn-path test confirms it. An invalid launch is
    validated before the lock and answers immediately. Peers get no live view of
    a launch: the pair and its tool events go only to the requesting
    connection, though the tool call is persisted in the shared transcript.
27. **`TurnLifecycle::finish` takes no agent.** It captures the agent's cancel
    signal at start, so a turn that does not hold the lock (a `/subagent`
    launch) reports its end without waiting for a queued model turn and without
    borrowing that turn's cancel epoch. The `Drop`-without-`finish` message now
    covers both an aborted turn and an early return; the status stays
    `cancelled` for the non-panicking case, which is the abort path. The daemon
    can emit two terminal `done`s for one cancelled request id (the turn task's
    own terminal plus the cancel path's); the router tolerates the duplicate.
28. **Cancel-path coverage.** The cancel path has two orderings to survive: the
    turn can stop cooperatively so its own terminal `done` reaches the
    cancelling connection before the cancel path's `interrupted` + `done`, and
    the relayed `turn_ended` travels through a separate forwarder task. A
    daemon test drives the real path with a peer attached and a router test
    replays both sequences, asserting consistent state and no misrouting.
29. **The active turn is registered before `turn_started` is announced.** The
    end side already cleared the registry before `turn_ended`; the start side
    did not. A client that registered its event sender and then built `History`
    in that window saw `active_turn: None` and dropped every event of the turn
    as belonging to no open turn. `TurnLifecycle::start_with` registers first
    and hands the announcement to the caller, and a test samples the registry
    from inside the announcement.
30. **Turn ids carry a per-boot nonce.** `turn-N` alone restarted at 1 with the
    daemon, so a long-lived client keying turn state by id saw `turn-3` again
    after a reload (Paseo namespaces it as `acp-vendor:turn-N`). Ids are now
    `turn-{16 hex digit per-boot nonce}-{counter}`, with the nonce drawn once
    per process from `RandomState` (the same OS-seeded source as the ACP
    instance ids).
31. **Every replayed and streamed message carries a stable `messageId`.**
    Clients group chunks by that id and fall back to positional grouping without
    it, which merged consecutive same-role messages (two assistant messages
    separated only by tool calls that are not replayed). Streamed ids are scoped
    per turn — `{session}:own:{request id}:{n}` for this adapter's own turns and
    `{session}:peer:{turn id}:{n}` for mirrored ones — and `n` advances on each
    message boundary, so chunks of one message share an id. The peer prompt
    chunk is `{session}:{turn id}:user`; command responses and skill notices
    carry `{session}:command:{rpc id}` and `{session}:skill-notice:{rpc id}`.
    Replay ids are `{session}:msg:{stored index}`, falling back to
    `{session}:history:{position}` for synthetic rows (Deviation 33).
32. **A mid-turn attach does not show the running prompt twice.** The daemon's
    `GetHistory` falls back to the *persisted* session while a turn holds the
    agent lock, and a turn persists its user message before streaming, so the
    snapshot a mid-turn attacher receives already ends with that turn's prompt
    (verified end-to-end). Since Deviation 31 gives the replayed copy and the
    live `user_message_chunk` different ids, the prompt appeared as two items.
    The router now checks the replayed tail (last 16 messages) for the active
    turn's `user_content` and suppresses the live user chunk when it is already
    there. A live peer turn is always announced: this connection has replayed
    nothing for it.
33. **Replay ids use the stored message's own index.** `{session}:history:{index}`
    was positional: rendering skips stored messages (tool-only rounds), a delta
    request changes the base, and compaction rewrites the transcript, so any of
    those shifts the position of an otherwise unchanged message and a reload
    would overwrite the wrong timeline item. `HistoryMessage` now carries the
    additive `stored_index` the daemon already tracks for rewind targets, and
    replay ids are `{session}:msg:{stored_index}`, falling back to the positional
    form only for synthetic rows that have no stored message. Stored indices are
    still renumbered by a compaction that rewrites the transcript, so ids for
    messages older than the retained tail can still move across a compaction;
    every other shift (render skipping, delta bases) no longer affects them.

### Port onto upstream turn identity (2026-11-07)

The fork was rebuilt on upstream `v1.0.0-preview.199`, whose daemon already
fences turns with `TurnIdentity { runtime_generation, turn_id }`. The fork's
parallel lifecycle (`turn_lifecycle_registry`, `TurnLifecycle`, string turn
ids, `History.server_features`, `History.active_turn`) was dropped in favour of
upstream's model plus a small additive origin extension
(`docs/ACP_UPSTREAM_PORT_PLAN.md`). The ACP-facing contract is unchanged except
where noted. These entries supersede the design sections and Deviations 1–33
wherever they overlap.

34. **Daemon wire contract.** Turns are announced and scoped by upstream's
    events, extended additively: `turn_started { identity, origin?,
    user_content? }`, every event of the turn (including its terminal) inside
    `turn_event { identity, event }`, and the new `turn_ended { identity,
    status: completed|failed|cancelled, message? }`, published exactly once per
    turn immediately before the scoped terminal, on every exit path (normal,
    error, panic, abort). `turn_state` (sent before every `History`) carries
    the running turn's `active_origin` and `active_user_content`; it replaces
    `History.active_turn`. Readers that do not subscribe with
    `supports_turn_identity` get unwrapped events and never see
    `turn_started`/`turn_ended`/`turn_state`. Supersedes D1, Deviations 1–3,
    13, 27–29.
35. **Capability via `Pong`.** The daemon advertises `"turn_origin"` only in
    `Pong::capabilities`; there is no `History.server_features`. The adapter
    sends `Ping` on each fresh connection before `Subscribe` (the daemon keeps
    the connection open after a pre-subscribe `Ping`) and waits up to 5 s for
    the answer. With the capability it subscribes with
    `supports_turn_identity: true`; without it (or without an answer) it
    subscribes flat and uses the legacy routing of D2.5. This was the least
    disruptive choice: no protocol change, one extra round trip per
    connection. Supersedes the `server_features` parts of D1 and D2.4.
36. **`turnId` is `"{runtime_generation}:{turn_id}"`.** `_jcode/turn.turnId`
    and the turn-scoped `messageId`s use this canonical string. The runtime
    generation is a fresh UUID per agent runtime, so ids are unique across
    daemon restarts without a per-boot nonce; it also changes when a runtime
    is replaced (clear, cold resume), which a client must treat as a new turn.
    Turn ids are monotonic per runtime but may have gaps (debug paths claim
    ids without publishing). Supersedes Deviation 30. The `_jcode/turn` shape
    (`sessionId`, `turnId`, `state`, `origin { kind, clientInstanceId }`,
    `error { message }`) is unchanged.
37. **Own vs peer, and fencing.** A turn is this connection's own when
    `origin.kind == client`, `origin.client_instance_id` is the connection's
    `acp-{uuid}/{n}` and `origin.request_id` is a pending prompt id
    (Deviation 18 unchanged). The router classifies `turn_started`,
    `turn_event`, `turn_ended` and `turn_state` by identity before anything is
    unwrapped. It ports the TUI's fence: ids at or below the completed
    watermark and anything from a retired generation are dropped, and a scoped
    event is accepted only for the open own or peer turn. Unlike the TUI, a
    `turn_started` from a generation the connection has never seen is adopted
    (retiring the old one) rather than dropped, because the adapter has no
    other way to recover a prompt the daemon accepted. An active turn in
    `turn_state` without an origin (claimed but never announced) is not
    mirrored.
38. **Peer terminals are told apart by identity.** A peer's scoped terminal
    carries its turn's identity, so the request-id marker scheme of
    Deviations 5, 15 and 25 is gone: a terminal for a turn that is not our open
    own turn is never delivered to a prompt. Upstream fans a turn's terminal
    out to every attachment, including a cancelled turn's (the canceller's
    connection additionally gets the cancel path's own `interrupted` and
    `done`, and after a forced abort a requester can see two scoped `done`s);
    the router drops the duplicates of a closed turn. Unscoped `done`/`error`
    still route by pending request id (Deviation 10): a rejected prompt and
    slash-command replies have no turn.
39. **No queueing: a prompt during another turn is rejected.** Upstream turn
    ownership is a non-blocking lease, so the daemon answers a `Message` sent
    while any turn of the session runs with an unscoped `error` ("Already
    processing a message") instead of queueing it on the agent mutex. The
    adapter returns that error for the `session/prompt`; the running peer turn
    keeps mirroring. Supersedes D4's first rule; the "queued prompt behind a
    peer turn" cases of the router tests and the e2e scenario became
    "rejected prompt during a peer turn".
40. **Scoped cancel.** `session/cancel` sends `Cancel { turn: Some(identity) }`
    for the turn this view shows running: its own, or the mirrored peer turn
    (a stop in Paseo during a TUI turn still cancels it for every client). The
    daemon ignores a scoped cancel whose turn already ended, so a late stop
    cannot hit the next turn. With no turn known (a flat connection, or a
    prompt not yet accepted as a turn) it is session-wide as before.
    `session/close` sends a cancel only for this connection's own running
    turn, scoped to it (D4, Deviation 16); a peer's turn is never cancelled by
    a close. A cancelled own turn answers its prompt with stopReason
    `cancelled`: the router hands the prompt an `interrupted` when its turn
    ends `cancelled`, because the cancel path's own `interrupted` reaches the
    canceller only after the turn's terminal.
41. **Mid-turn attach.** The peer turn opens from `turn_state` (Deviation 9's
    `History.active_turn` is gone). The re-announce check of Deviation 32 runs
    when the first `History` arrives: if the replayed tail already contains the
    prompt of a peer turn opened before it, the router emits
    `PeerPromptReplayed` and the adapter drops the user chunk from the
    announcement still held in the attach gate (Deviations 19, 23, 24
    unchanged).
42. **`/subagent` runs as a turn.** `RunSubagent` claims a regular turn lease,
    so ownership serialises it with model turns (a launch during another turn
    is rejected like a prompt) instead of holding the agent mutex
    (Deviation 26). It publishes `turn_started` (client origin with the
    requester's instance and request id), its scoped tool events, `turn_ended`
    and the scoped terminal on the requesting connection only, and is
    withdrawn from the attach snapshot so a peer attaching mid-launch does not
    open a turn it will never see end. Validation happens before the lease.
    The agent mutex is held only around transcript updates, and the swarm
    spawn claims its own lease on the worker's runtime, so the launch cannot
    deadlock with the spawn path. Supersedes Deviations 14 and 26.
43. **Prompt text for peers.** The daemon still sends upstream's unscoped
    `peer_user_message` to legacy readers; the adapter ignores it in favour of
    `turn_started.user_content`, which has no ordering race with the turn.
44. **Abort ends the turn everywhere.** A turn whose task is aborted after the
    cancel grace period is still closed by the daemon's turn channel with
    `turn_ended(cancelled)` and a scoped `done` for every attachment, so a
    mirrored peer turn ends as `canceled` instead of running forever
    (Deviation 13's guarantee, now in `TurnEventChannel`).
45. **Cancel then prompt.** A stop aimed at another connection's turn arms the
    session's cancel signal and clears it 500 ms later (upstream behaviour). A
    prompt sent inside that window can start already cancelled. The e2e test
    waits that window out; clients that send a prompt immediately after a stop
    of a peer's turn may see it end at once as cancelled.
46. **Router tests.** The router's scripted-event tests live in
    `src/cli/acp/daemon_router_tests.rs`; the adapter tests (gate, usage,
    close, instance ids, message ids, session/list) are in
    `src/cli/acp/tests.rs` next to upstream's ACP tests. Expected ids use the
    new `turnId` format.
47. **Update channel unchanged.** The fork's switch of `GITHUB_REPO` to
    `camerontaylor/jcode` (part of the original `/subagent` commit) was not
    carried into this port; `jcode update` still follows upstream releases.

