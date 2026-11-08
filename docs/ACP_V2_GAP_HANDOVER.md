# ACP v2, steering, and SDK adoption: research handover

Prepared 2026-11-08. Research only; no code was changed.

Implementation direction: [Fork ACP interface contract](ACP_FORK_INTERFACE.md)
defines the phase 1–5 target for the fork and its consuming client, including
decisions that supersede recommendations below. This report remains the
pinned gap analysis and SDK research.

This compares the ACP interface jcode publishes (what a client sees on the
wire, not daemon internals) against the ACP v2 draft and the steering
proposal, then evaluates replacing the hand-rolled adapter with the official
Rust SDK.

## Sources

| What | Version / ref | Where |
|---|---|---|
| jcode ACP adapter | `ae0f09de3` (this worktree, off `custom`) | `src/cli/acp.rs`, `src/cli/acp/*.rs` |
| ACP spec repo | `7441d10` (2026-11-07) | `github.com/agentclientprotocol/agent-client-protocol` |
| v2 schema | `2.0.0-alpha.13` (2026-11-02) | `schema/v2/schema.json`, `meta.json`, `meta.unstable.json` |
| v2 migration guide | same commit | `docs/protocol/v2/migration.mdx` (the best v1→v2 checklist) |
| v2 prompt lifecycle | same commit | `docs/protocol/v2/draft/prompt-lifecycle.mdx` |
| Steering RFD | PR #1261, open, last revised 2026-10-24 | `docs/rfds/v2/prompt-delivery.mdx` on the PR branch |
| Rust SDK | `agent-client-protocol` 5.0.0, `050cd94` | `github.com/agentclientprotocol/rust-sdk` |
| Schema crate | `agent-client-protocol-schema` 1.15.1 (2026-11-02) | `agent-client-protocol-schema/` in the spec repo |

To reproduce: shallow-clone both repos, and use `gh pr diff 1261 --repo
agentclientprotocol/agent-client-protocol` for the RFD text.

## 1. Steering: the proposal changed shape

**PR #1261 no longer defines `session/inject`.** The 2026-10-24 revision
replaced `session/inject`, `session/revoke_inject` and
`session/replace_inject` with a field on the ordinary prompt. Advice that
predates that revision (for example "build `_x/inject` with `{sessionId,
content, mode}` and rename later") points at the abandoned design.

The current proposal:

- `session/prompt` gains optional `delivery: "steer" | "queue"`. No new
  methods, notifications, error codes or stop reasons.
- Agent advertises `capabilities.session.prompt.delivery: { steer: {}, queue:
  {} }`. A client MUST NOT send an unadvertised value. The agent rejects an
  unadvertised or unknown value with `-32602` before insertion.
- A prompt without `delivery` keeps plain v2 behaviour and does not join the
  queue. The RFD explicitly rejects implicit steering (PR #2255): only the
  user knows whether "stop, wrong file" or "when you're done, run the tests"
  was meant.
- **steer**: inserted at the agent's next safe point (typically after running
  tool calls finish, before the next model request). The agent MAY stop a
  streaming model response to insert sooner. It MUST NOT cancel started tool
  calls or alter already-streamed messages. No `idle` between the earlier work
  and the steer.
- **queue**: held until the turn ends. The agent reports `idle`, then starts a
  new turn with it.
- Response is the v2 "inserted" response (`{messageId}`), possibly long after
  the request. Insertion is echoed as `user_message` with the same
  `messageId`. Response and echo may arrive in either order.
- Idle session: a `delivery` prompt is inserted as a normal prompt (no
  end-of-turn race).
- Turn ends before a steer reaches a safe point: all waiting steers start the
  next turn together, in received order, then queued prompts each start their
  own turn.
- `requires_action` (for example a permission wait): the prompt is accepted
  and inserted after work resumes. A prompt never answers a permission request
  or elicitation.
- **Withdraw** a pending prompt with `$/cancel_request`: `-32800` before
  insertion, success with `messageId` after.
- **`session/cancel` stops the turn, not pending prompts.** After `idle` with
  `cancelled`, waiting steers start the next turn immediately. This is how
  "Stop and act on my message" works. To discard them too, withdraw each with
  `$/cancel_request` first.
- Connection closes: the agent SHOULD drop that connection's pending prompts.
- Rust SDK work: rust-sdk PR #339 ("add session injection support", open since
  2026-09-29) predates the `delivery` redesign.

### jcode today vs #1261

jcode steers implicitly: on a daemon whose `Pong` advertises `turn_origin` and
`soft_interrupt_ids`, any `session/prompt` during a running turn becomes a
`SoftInterrupt` (`src/cli/acp.rs:913-926`, `docs/ACP_SHARED_SESSIONS.md` §48).

| #1261 rule | jcode | Status |
|---|---|---|
| Explicit `delivery` chosen by the client | Implicit, every concurrent prompt | Missing |
| `session.prompt.delivery` capability | Nothing advertised. Depends on hidden daemon capabilities | Missing: a client cannot tell whether a busy prompt is steered or rejected |
| `queue` mode | Not offered | Missing |
| Unknown or unadvertised `delivery` → `-32602` | Field ignored | Missing |
| Every insertion echoed as `user_message` | Peer steers echoed as `user_message_chunk` (`acp.rs:1666`). **Own steers not echoed**: joined silently (`daemon_router.rs:1146-1157`) | Missing for own steers |
| Response = inserted, `{messageId}` | All prompts resolve at turn end with `stopReason` (v1) | Needs v2 lifecycle |
| Stranded steer starts the next turn | Adapter resends via `CancelSoftInterrupts` (§48) | Equivalent |
| `session/cancel` keeps pending steers; they go next | Cancel **withdraws** waiting steers and answers them `cancelled` (`acp.rs:971-975`) | Opposite semantics |
| Withdraw via `$/cancel_request` | Not implemented (unknown method) | Missing |
| Hold during `requires_action` | Adapter never sends `session/request_permission` | N/A today |
| Drop pending prompts on connection close | Daemon-queue behaviour; not checked | Unverified |

The cancel semantics deserve a deliberate decision. The RFD's argument (a
Stop from one client must not erase another user's queued message) applies
directly to jcode's shared sessions.

## 2. What jcode publishes today

- Protocol version pinned to 1 regardless of the request (`acp.rs:15`,
  `initialize_result` at `acp.rs:2817-2864`).
- Methods (`acp.rs:722-775`): `initialize`, `session/new`, `session/load`,
  `session/resume` (attach without replay), `session/prompt`,
  `session/cancel`, `session/close`, `session/list`,
  `session/set_config_option`, plus unprefixed custom `session/set_model` and
  `session/set_reasoning_effort`. Other `_`-prefixed methods get `-32601`.
- Extensions: `_jcode/turn` (peer-turn bracket, all profiles, consumed by
  Paseo) and `_jcode/server_event` (extended profile). `_meta.jcode` is
  advertised only in the extended profile.
- Updates emitted: `agent_message_chunk`, `user_message_chunk` (replay and
  peer messages), `tool_call`, `tool_call_update`, `config_option_update`,
  `available_commands_update`, `session_info_update`, `usage_update`.
- jcode never calls `session/request_permission`, `elicitation/*`, `fs/*` or
  `terminal/*`.

## 3. v2 core gaps (stable `schema/v2/schema.json`)

Stable v2 methods (`meta.json`): agent: `initialize`, `auth/login`,
`auth/logout`, `session/{new,list,resume,close,delete,prompt,cancel,set_config_option}`,
`tool_call/cancel`, `terminal/cancel`. Client: `session/update`,
`session/request_permission`, `elicitation/{create,complete}`. Protocol:
`$/cancel_request`.

**Negotiation and initialization**
- No negotiation. v2 selects v1 or v2 per connection and rejects with
  `-32022` (`requested`/`supported` data) when no version fits. Upstream
  says to keep v1 and gate v2 behind negotiation and a feature flag.
- v1 field names (`agentCapabilities`, `agentInfo`). v2 uses `capabilities`
  and required `info` on both sides.
- Boolean markers. v2 uses object markers nested under `session.prompt` and
  `session.mcp`, and drops `loadSession` and the `list`/`resume`/`close`
  markers. Advertising `session` commits to the baseline (`new`, `list`,
  `resume`, `close`, `prompt`, `cancel`, `update`).
- No `-32600` for requests before `initialize` or a repeated `initialize`.

**Prompt lifecycle (largest change)**
- Response is `{stopReason, usage}` at turn end (`acp.rs:407`). v2 returns
  `{messageId}` at insertion.
- No `state_update` (`running`, `idle` + `stopReason`, `requires_action`).
  `_jcode/turn` is the closest analogue, for peer turns only.
- No `user_message` echo of the client's own prompt.
- Post-insertion failure is a JSON-RPC error (`acp.rs:1642-1646`). v2 uses
  idle `state_update` with `stopReason: "error"` and `error`. A prompt error
  response then always means "not inserted".
- Usage on the prompt response moves to the idle `state_update` (unstable).
- `-32000 "already processing"` for non-steering daemons and slash commands
  (`acp.rs:918-926`). v2: MUST NOT reject only because work is active.

**Session lifecycle**
- `session/load` is removed. Use `session/resume` with `replayFrom: {type:
  "start"}`. Unsupported cursors → `-32602`.
- A successful resume MUST be preceded by a current-state snapshot.
- Chunked replay (`acp.rs:2325-2355`) needs an empty-content primer per
  message, or whole-message upserts.
- `session/close` answers `{}` immediately (`acp.rs:997-1023`). v2 requires
  final updates and idle `cancelled` before the close response. jcode's
  shared-session close deliberately does not cancel an adopted peer turn;
  reconcile this with v2 (possibly raise upstream).
- Cancel and close answer never-inserted prompts with stopReason `cancelled`.
  v2 uses error `-32800` (subject to #1261's carve-out for `delivery`
  prompts).

**Updates** (`EventMapper`, `acp.rs:2661-2782`)
- `tool_call` is removed. The first `tool_call_update` creates the call. Add
  `tool_call_content_chunk` for streaming.
- `TextReplace` and `TextSuffixReplace` are emitted as appended chunks
  (`acp.rs:2671-2676`). v2 `agent_message` whole-message upserts can express
  replacement properly.
- Compaction is emitted as agent text (`acp.rs:2767`). v2 has
  `compaction_update`.
- Config options use `id` (`session_configuration.rs:152`). v2 uses
  `configId`, and `session/set_config_option` requires `type: "id" |
  "boolean"`.
- `available_commands_update` entries need `type: "command"`. Their `input`
  needs `type: "text"`.
- Three-state patch semantics (omitted = unchanged, `null` = clear, value =
  replace), including key-level `_meta` patches on upserts.

**MCP, errors, extensibility**
- `mcpServers` is accepted and ignored (`acp.rs:3070-3078`), which is already
  misleading in v1. v2 needs honest `session.mcp.stdio`/`http` markers and
  `-32602` for unsupported transports or duplicate names.
- Unknown session → `-32602` (`acp.rs:904-910`). v2 MUST use `-32002` with
  `data.sessionId`.
- Unprefixed `session/set_model` and `session/set_reasoning_effort` violate
  the `_` rule. The config-option route already covers both.
- Unknown prompt content type is a hard error (`acp.rs:3285`). v2 content
  types are open, so this needs a safe fallback.

## 4. Gaps independent of v2 (affect v1 today)

- Daemon emits `ReasoningDelta` (`crates/jcode-protocol/src/wire/events.rs:277`).
  The mapper drops it, so there is no `agent_thought_chunk`.
- Daemon emits `Todos` (`events.rs:212`). There is no `plan` / `plan_update`.
- Tool calls carry no `locations`, no `diff` content, and no `name` (stabilized
  in v1).
- No `session/request_permission` or elicitation. Approvals happen outside
  ACP.

## 5. Optional and unstable v2 features not present

- Stable but optional: `session/delete`, `additionalDirectories`,
  `systemPrompt`, `auth/login` and `auth/logout`, `tool_call/cancel`
  (per-call `capabilities.cancel`), `terminal/cancel`, display-only
  `terminal_update` and `terminal_output_chunk`, structured diffs (`changes`
  + `git_patch`), `command` permission subject, `notice`.
- Unstable (`meta.unstable.json`): `background` list on `state_update` (a
  natural fit for backgrounded shells and swarm agents), `session/fork`,
  `session/set_title`, `session/status`, `session/archive` and
  `session/unarchive`, subagents, `providers/*`, `auth/status`.

## 6. The official Rust SDK

### It includes the v2 draft

- `agent-client-protocol-schema` 1.15.1 has a parallel `v2` module behind
  `unstable_protocol_v2` (deliberately not part of the `unstable` umbrella).
  It appears to track `alpha.13`: same release date, and alpha.12's
  `background` types are present.
- `agent-client-protocol` 5.0.0 forwards that feature and provides
  `Agent.v1()` and `Agent.v2()` builders. `Agent.protocol_router().with_v1(a).with_v2(b)`
  selects per connection from `initialize`, which is exactly the side-by-side
  setup the migration guide recommends.
- Worked v2 agent: `src/agent-client-protocol/examples/simple_agent_v2.rs`
  (held prompts, `-32800` on cancel, `replayFrom`, `state_update`).
- Custom `_jcode/*` messages map to derived `JsonRpcNotification` types, with
  `on_receive_dispatch` as an untyped catch-all.

### Why jcode hand-rolled it (inferred; nothing recorded)

The adapter landed as `f0266100a` on 2026-05-23, with no commit body.

- Up to 0.10 (2026-03-31) the SDK's `Agent` trait was
  `#[async_trait(?Send)]`, requiring a tokio `LocalSet` (verified in the
  0.10.0 source).
- 0.11 (2026-04-21) was a ground-up rewrite. 0.12 came out six days before
  jcode's adapter. The SDK was pre-1.0.
- The first adapter was a 1,190-line bridge.
- Since 1.0 (2026-07-20) there have been four breaking majors: 2.0
  (09-18), 3.0 (10-28), 5.0 (11-02).

### What it replaces and what it doesn't

- **Replaces** most protocol plumbing in `acp.rs`: `JsonRpcMessage`, the
  `write_*` helpers, 43 `json!` literals, parameter parsers
  (`required_session_id`, `cwd_from_params`, `prompt_from_params`,
  `validate_acp_mcp_servers`), `initialize_result`, and the JSON shapes in
  `EventMapper` and `history_replay_updates`. That is roughly the first
  quarter to third of the file's 3,444 lines.
- **Does not replace** `daemon_router.rs` (1,617 lines plus 1,644 of tests),
  turn mirroring and adoption, stranded-steer recovery, slash commands and
  skills, session cataloguing, or the `_jcode/turn` contract Paseo depends on.

### Risks

1. **`serde_json/preserve_order` becomes global.** The schema crate enables
   it in normal `[dependencies]`. Cargo feature unification switches every
   `serde_json::Map` in jcode from sorted (BTreeMap) to insertion order
   (IndexMap). Check tests that compare serialized JSON, code relying on
   sorted iteration, and provider request bytes (tool schemas → prompt-cache
   keys). This is the only risk that reaches outside the ACP code.
2. **Strict typed parsing** removes leniencies: `cwd` defaulting to the
   process directory, the `mime_type` alias, any-array `mcpServers`. Confirm
   Paseo does not rely on them.
3. **Sequential dispatch loop.** Callbacks hold the loop, so long work must
   go to `connection.spawn`. jcode already spawns per prompt
   (`acp.rs:929`), so this maps cleanly. Never await a client response
   inside a callback (deadlock).
4. **Churn.** v2 is an unstable feature over an alpha schema. Pin exactly
   (`=5.0.0`, schema `=1.15.1`) and budget a change per alpha. Hand-rolled
   code must chase the alphas too.
5. **No `delivery` in the schema yet.** Carry it in `_meta.jcode.delivery`, or
   route `session/prompt` untyped, until #1261 lands.
6. Not verified: whether the SDK enforces `-32600` before `initialize`,
   `-32002` for unknown sessions, and `-32601` for unadvertised methods by
   itself. Its version guards suggest some of this is built in.

## 7. Recommendations

In order. Each step stands alone.

1. **Small v1 fixes now** (no SDK, no v2):
   - rename `session/set_model` and `session/set_reasoning_effort` to
     `_jcode/...`, or drop them in favour of config options;
   - stop accepting `mcpServers` that are then ignored, or document it, and
     advertise MCP honestly;
   - map `ReasoningDelta` → `agent_thought_chunk` and `Todos` → `plan`;
   - echo the client's own steer as `user_message_chunk` at its insertion
     point;
   - advertise steering under `_meta.jcode` so clients can tell steer from
     reject;
   - support `$/cancel_request` for pending steers;
   - return `-32002` for unknown sessions.
2. **Decide cancel-vs-steer semantics.** jcode withdraws pending steers on
   cancel. #1261 keeps them and runs them next. Pick one explicitly, record
   it in `docs/ACP_SHARED_SESSIONS.md`, and test it. #1261's behaviour is the
   likely standard.
3. **Adopt the SDK for v1 with no wire change.** Port onto `Agent.v1()`.
   Treat `tests/e2e/acp_shared_session.rs` and the JSON-shape tests in
   `src/cli/acp/tests.rs` as the wire contract. Resolve the `preserve_order`
   question first, with a jcode-wide CI run, before anything else.
4. **Add v2 through `Agent.protocol_router()`**, behind a config or feature
   flag, sharing the router:
   - `{messageId}` responses;
   - `user_message` echo;
   - `state_update` (the standard form of `_jcode/turn`, which stays for v1
     and Paseo);
   - upserts;
   - `resume` + `replayFrom`;
   - v2 error codes.

   Most of the effort is lifecycle semantics, not the crate.
5. **Add `delivery` (`steer` + `queue`) and its capability on top of v2**,
   carried in `_meta` until #1261 merges, then in the field. Because #1261
   is defined in terms of the v2 lifecycle, doing steering properly before
   step 4 means either mimicking that lifecycle in an extension or doing the
   work twice.

## Open questions

- Does the daemon drop a queued soft interrupt when the ACP connection that
  sent it closes (#1261 SHOULD)?
- How should v2 `session/close` ("cancels ongoing work") apply to a
  shared session where the closing view adopted another client's turn?
  Worth raising upstream.
- Does Paseo depend on any of the parsing leniencies listed in §6 risk 2?
- Is the schema crate's `v2` module exactly `alpha.13`? This was inferred
  from the release date and type presence, not a diff.
