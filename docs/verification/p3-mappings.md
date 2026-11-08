# Phase 3 mappings verification

Phase 3 of [the ACP fork strategy](../ACP_FORK_STRATEGY.md#phase-3-complete-the-v1-target)
requires the adapter to deliver information the daemon already has, using
standard ACP v1 `session/update` shapes, and forbids synthesizing structured
metadata from display strings. This document records the audit that decided
each mapping, then the mappings themselves.

Paths are relative to the repository root. `events.rs` is
`crates/jcode-protocol/src/wire/events.rs`, `wire.rs` is
`crates/jcode-protocol/src/wire.rs`, and `acp.rs` is `src/cli/acp.rs`.

## Audit: what the daemon has and what the adapter emits

Audited at `2830da176` (merge of `fork/acp-conformance` and
`fork/acp-capability-metadata` into `fork/acp-stage`).

| Information | Authoritative daemon data | Adapter emission before this change | Decision |
| --- | --- | --- | --- |
| Exposed reasoning | `ServerEvent::ReasoningDelta { text }` (`events.rs:277`, raw model reasoning deltas); boundary marker `ServerEvent::ReasoningDone { duration_secs }` (`events.rs:283`) | None. `EventMapper::map_event` (`acp.rs:2678`) has no arm, so both fall through to `_ =>` (`acp.rs:2794`) | **Map.** `agent_thought_chunk` per delta; `ReasoningDone` closes the chunk group. Not replayed from history: `HistoryMessage.content` is a flat string (`acp.rs:2342` `history_replay_updates`), so a reload cannot expose it |
| Plans / todos | `ServerEvent::Todos { session_id, payload }` (`events.rs:212`). `payload` is a JSON string built in `crates/jcode-app-core/src/server/client_lifecycle.rs:1541-1556` from `todo::load_todos` / `load_plan` / `load_goals`. Entries are `TodoItem` (`crates/jcode-task-types/src/lib.rs:461-465`: `content`, `status`, `priority`, `id`). Fetched with `Request::GetTodos { id }` (`wire.rs:134`); the reply carries **no request id** and the daemon never pushes it | None. Nothing in the adapter requests todos, and the router drops a flat `Todos` event when no turn is open (`src/cli/acp/daemon_router.rs:1300-1317`) | **Map.** The router requests `GetTodos` when a `todo` tool call completes (own or peer turn) and renders the reply as a session-level full-replacement `plan` update. The todo tool's own output is a rendered card, so it is not used. See "Timing and load" below |
| Tool call identity | `ServerEvent::ToolStart { id, name }` (`events.rs:302`); `ToolExec { id, name }` (`events.rs:314`); `ToolDone { id, name, output, error }` (`events.rs:318`) | `tool_call` (`acp.rs:2698`) with `title`, `kind`, `status`; `tool_call_update` with `rawInput` (`acp.rs:2705`), `in_progress` (`acp.rs:2722`), `completed`/`failed` + text content + `rawOutput` (`acp.rs:2741`) | **Keep.** `name` added to the initial `tool_call`: the daemon supplies the programmatic name and v1 defines the field |
| Tool call kind | The same `name` field, through the explicit table at `acp.rs:3444` | `tool_kind` table already existed; `todo` and the planning tools fell to `other` | **Extend** the table: `todo` → `think`. No jcode tool is named for deletion or moving (`apply_patch` performs both under the `edit` kind), so `delete`/`move` stay unreachable and are documented rather than faked |
| Tool call locations | Structured tool input, streamed as `ServerEvent::ToolInput { id, delta }` (`events.rs:306`) and accumulated by the adapter (`acp.rs:2705`). The path-bearing fields are documented by the tool schemas: `read` → `file_path`, `start_line` (`crates/jcode-app-core/src/tool/read.rs:131-142`); `edit` → `file_path`, `edits` (`.../edit.rs:166-190`); `write` → `file_path` (`.../write.rs:39-55`). The daemon resolves a relative path against the session working directory (`ToolContext::resolve_path`, `crates/jcode-tool-core/src/lib.rs:141-149`) | None | **Map** for `read`, `edit`, `write` only, resolved the same way the daemon resolves them. `apply_patch` takes `patch_text` (`.../apply_patch.rs:80-92`), a string: its paths are not used, because that would mean parsing a patch |
| Tool call diffs | The same structured edit input: `edits: [{old_string, new_string}]` plus the legacy single `old_string`/`new_string` form (`edit.rs:166-190`), and `write`'s `content` | None | **Map** as `diff` content entries, from structured input only, never by re-reading the file. `write` carries no `oldText`: the adapter does not know the previous content and will not read it |
| Tool call raw input/output | `ToolInput` deltas; `ToolDone.output` / `.error` | `rawInput` on input/exec updates, `rawOutput` on completion | **Keep** |
| SDK tool calls | `ServerEvent::ToolCall { session_id, call_id, name, input }` (`events.rs:232`), emitted only for SDK-registered custom tools (`crates/jcode-app-core/src/tool/sdk.rs:237`) | None (dropped) | **Skip.** ACP sessions expose no SDK/custom tools, so the event does not occur on this connection |
| Images, usage, config, commands, `_jcode/turn`, server events | — | Existing mappings | **Preserve**, unchanged |

## Implemented mappings

### 1. Exposed reasoning → `agent_thought_chunk`

`ServerEvent::ReasoningDelta` becomes:

```json
{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{
  "sessionUpdate":"agent_thought_chunk",
  "messageId":"s1:own:7:thought:0",
  "content":{"type":"text","text":"Let me check the adapter."}}}}
```

Chunks of one reasoning block share a `messageId`, so a client groups them the
way it groups assistant chunks. `ReasoningDone` (and `MessageEnd`) advance the
thought index, so reasoning after a text message or after a closed block starts
a new one instead of being appended to the previous block. `messageId` is
optional in v1; it is emitted for the same reason the assistant chunks emit it.

Absent data is preserved as absence: a turn with no `ReasoningDelta` produces no
`agent_thought_chunk`, and the adapter never turns `TextDelta` into reasoning.

### 2. Todos → `plan`

When a `todo` tool call completes, the router sends `Request::GetTodos` on the
same connection and renders the daemon's `Todos` reply as a full-replacement
plan update:

```json
{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{
  "sessionUpdate":"plan",
  "entries":[
    {"content":"Audit the adapter","priority":"high","status":"completed"},
    {"content":"Map reasoning","priority":"medium","status":"in_progress"}]}}}
```

Field mapping, all from structured data:

- `content` ← `TodoItem.content`.
- `status` ← `TodoItem.status`. `pending`, `in_progress` and `completed` pass
  through; `cancelled` entries are dropped (ACP has no cancelled state and
  cancelled work is not part of the plan); any other value becomes `pending`.
- `priority` ← `TodoItem.priority` when it is exactly `high`, `medium` or
  `low`; every other value becomes `medium`, because ACP requires one of the
  three and jcode's field is free-form.

Each update carries the complete list, as the v1 plan section requires
("The Agent MUST send a complete list of all plan entries in each update").
An unchanged plan is not re-sent. An empty todo list is sent as
`entries: []`, which is a real plan replacement (the agent cleared its plan),
not an omission.

**Timing and load.** A plan is session state, so it is routed as a session-level
action (`RouteAction::Plan`) rather than as turn output. That is not a
simplification for its own sake: the daemon writes the `Todos` reply on the
connection's event channel, which drains *after* the turn's ordered output, so a
reply requested mid-turn arrives after that turn's `session/prompt` response
(measured in `acp_conformance_maps_reasoning_tool_calls_and_plan`). ACP allows
session updates outside a prompt turn, so the plan is delivered as soon as the
daemon supplies it. The same routing means a plan written by a peer turn is
mirrored to an observing client instead of being dropped with the turn.

### 3. Tool call locations

`read`, `edit` and `write` updates carry `locations`, from the structured input
field the tool schema documents, resolved against the session working directory
exactly as `ToolContext::resolve_path` does:

```json
{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{
  "sessionUpdate":"tool_call_update","toolCallId":"t1","status":"in_progress",
  "locations":[{"path":"/repo/src/cli/acp.rs","line":12}]}}}
```

`line` is only emitted where the schema documents a line: `read.start_line`
(1-based). When the input names no file — a shell command, a search query, an
`apply_patch` blob — the update has **no `locations` key at all**. The adapter
never emits an empty array and never invents a location.

### 4. Tool call diffs

The completion update for `edit` and `write` carries structured diff content
ahead of the text output, and only when the call succeeded:

```json
{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{
  "sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed",
  "content":[
    {"type":"diff","path":"/repo/src/a.rs","oldText":"let a = 1;","newText":"let a = 2;"},
    {"type":"content","content":{"type":"text","text":"Applied 1 edit to src/a.rs"}}],
  "rawOutput":{"output":"Applied 1 edit to src/a.rs","error":null}}}}
```

- `edit`: one diff per structured entry in `edits`, plus the legacy
  single-`old_string` form. `oldText` is the replaced text, `newText` the
  replacement.
- `write`: `newText` is the input `content`; `oldText` is omitted, because the
  adapter does not know the previous content and does not read the file to find
  out. A client renders an omitted `oldText` as new content.
- A failed call emits no diff, and `apply_patch` emits none either: its only
  input is patch text.

## Skipped for lack of authoritative data

- **Load-time plan.** `session/load` and `session/resume` replay `History`,
  which has no todos (`events.rs:702-745`), and the daemon's `Todos` reply is
  id-less and pull-only: nothing in ACP v1 asks an agent for its plan. A client
  attaching to a session that already has a plan therefore shows one from the
  next `todo` write, not from the attach. A load-time request would need an
  id-less reply claimed by a registered waiter during the handshake; that is
  recorded here rather than approximated from tool output text.
- **Reasoning in history.** `HistoryMessage.content` is flat text, so a reload
  cannot replay reasoning. Nothing is fabricated to fill the gap.
- **`apply_patch` locations and diffs.** The tool's input is `patch_text`, a
  string. Deriving paths or hunks from it means parsing a patch, which the
  strategy excludes.
- **Delete/move tool kinds.** No jcode tool is named for deletion or moving.
  `apply_patch` performs both and is reported as `edit`.
- **SDK `ToolCall`.** Emitted only for SDK-registered custom tools
  (`tool/sdk.rs:237`), which an ACP session cannot invoke.

## Verification

JSON-shape tests live in `src/cli/acp/tests.rs` (reasoning chunks and their
grouping, locations with and without a file, edit and write diffs, no diff for a
failed call, plan replacement/dedup/cancelled-entry handling, and the explicit
kind table including unknown names). Router tests in
`src/cli/acp/daemon_router_tests.rs` cover the snapshot request on a `todo`
completion (and its absence for other tools) and the session-level `Plan`
action, in and out of a turn. The wire-level ordering test is
`acp_conformance_maps_reasoning_tool_calls_and_plan` in
`tests/e2e/acp_conformance.rs`: it scripts reasoning, a `todo` call and a `read`
call against the real stdio adapter and asserts that `agent_thought_chunk`
precedes the tool frames, each `tool_call` precedes its `tool_call_update`, the
`read` update carries the cwd-resolved location, and the plan arrives as a
session update after the prompt response (see "Timing and load").

Existing usage, config, available-command, `_jcode/turn` and raw-server-event
behavior is unchanged. Two recorded conformance fixtures already drift on this
branch for a reason unrelated to these mappings: the initialize result now
carries `agentCapabilities._meta.jcode` (the Phase 1 capability-metadata lane)
and session results carry `_meta.jcode`, which
`tests/fixtures/acp/01-create-list-load-v1.jsonl` and
`tests/fixtures/acp/02-shared-observe-steer-v1.jsonl` predate. Re-recording them
here would mix an unrelated wire change into this one, so they are left alone;
a recording run confirms these mappings add no other drift line.
