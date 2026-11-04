# ACP Shared Sessions

Status: Proposed (2026-11-04)

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
