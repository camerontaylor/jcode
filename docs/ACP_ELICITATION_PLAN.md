# ACP structured elicitation: implementation plan

Status: implemented and locally verified, 2026-11-09. Branch `fork/acp-elicitation`. Contract:
[Structured elicitation](ACP_FORK_INTERFACE.md#structured-elicitation).

## Decisions

| Question | Decision |
| --- | --- |
| Phase 6 deferral | Pull **form mode** forward. URL mode, `elicitation/complete`, MCP-server elicitation passthrough and permission requests stay in phase 6. |
| ACP version | v1. Elicitation was stabilized in ACP v1 on 2026-07-24 (`elicitation/create`, `$/cancel_request`). It does not wait for the v2 track. |
| First user | The `applet` tool, opted in per connection. |
| Opt-in location | The connection's tool selection: `applet` named explicitly in `[tools] enabled`, `--tools`, `JCODE_TOOLS`, or the `[acp] tool_profile` override. `*`/`all` and the `full` profile do not enable it. `disabled` still wins. |
| TUI renderer | Separate work. The TUI does not become an elicitation client here. |

Upstream reference: `agentclientprotocol/agent-client-protocol` `7441d10`,
`docs/protocol/v1/elicitation.mdx`, `schema/v1/meta.json`.

## Constraints found in the code

- The adapter cannot receive responses yet. `AcpRuntime::handle_message`
  (`src/cli/acp.rs`) answers any method-less message with `-32600`, so a
  client's reply to an agent request would be treated as a malformed request.
- The daemon-side tool allowlist is read from the **daemon's** config when the
  agent is created (`crates/jcode-app-core/src/agent.rs`, `tools.selection()`)
  and applied when tool definitions are built (`agent/turn_execution.rs`). A
  connection's `--tools` therefore only reaches the daemon the adapter spawns
  itself (`run_acp_command` sets `JCODE_TOOL_PROFILE`). The opt-in must travel
  on `Subscribe`.
- `applet` and `user_response` are registered only through
  `Registry::set_desktop_ui_enabled`, driven by `supports_user_response`. The
  adapter subscribes with `false`.
- Stdin requests (`StdinInputRequest`) route to the last attached connection.
  That model is unsuitable for shared sessions; elicitation needs a
  session-scoped broker.
- `deliver_to_waiter` in `tool/applet.rs` already resolves a blocking
  `applet` call from whichever source answers first.

## Footprint

Following the fork boundary strategy, the logic lives in fork-owned files.
Upstream files get thin hooks.

| File | Kind | Change |
| --- | --- | --- |
| `crates/jcode-protocol/src/elicitation.rs` | new | Request, schema and response types; validator |
| `crates/jcode-protocol/src/wire.rs`, `wire/events.rs` | hook | Two events, one request, two `Subscribe` fields, one server capability |
| `crates/jcode-app-core/src/elicitation.rs` | new | Session-scoped broker |
| `crates/jcode-app-core/src/server/*` | hook | Fan-out, response dispatch, catch-up on attach, capability aggregation |
| `crates/jcode-app-core/src/tool/applet_elicit.rs` | new | View-to-form compiler, elicitation-only tool description |
| `crates/jcode-app-core/src/tool/applet.rs`, `tool/mod.rs` | hook | Route `wait=true` through the broker; register the variant |
| `src/cli/acp/client_requests.rs` | new | Agent-to-client requests, response routing, `$/cancel_request` |
| `src/cli/acp/*` | ACP-owned | Capability parse, `elicitation/create` mapping, opt-in on `Subscribe` |

## 1. Types and validator (`jcode-protocol`)

```rust
pub struct FormElicitation {
    pub elicitation_id: String,      // daemon-unique, opaque
    pub session_id: String,
    pub tool_call_id: Option<String>,
    pub message: String,
    pub requested_schema: FormSchema, // flat object, ACP restricted subset
}
pub enum ElicitationResponse { Accept { content: Option<Map<String, Value>> }, Decline, Cancel }
```

- `FormSchema` mirrors ACP `ElicitationSchema`: `type: "object"`, optional
  `title`, `properties` of string (with `enum`/`oneOf`, `format`, length,
  `pattern`, `default`), number, integer, boolean, and multi-select array;
  `required`.
- `FormSchema::validate()` rejects nested objects, unknown property types and
  oversize schemas (field count, string lengths, option counts), and checks
  that `required` names exist.
- `FormSchema::check_content(&Map)` validates an accepted reply: required
  fields present, types and enum membership correct, bounds respected. Unknown
  keys are rejected. ACP says the agent SHOULD re-validate.
- Wire:
  - `ServerEvent::ElicitationRequest(FormElicitation)`
  - `ServerEvent::ElicitationResolved { session_id, elicitation_id, outcome: accepted|declined|cancelled|expired }`
  - `Request::ElicitationResponse { id, session_id, elicitation_id, response }`
  - `Subscribe.supports_elicitation_form: bool` and
    `Subscribe.applet_elicitation: bool`, both `skip_serializing_if` false
  - `"elicitation_form"` in `SERVER_CAPABILITIES`

## 2. Session broker (`jcode-app-core/src/elicitation.rs`)

```rust
pub async fn elicit(session_id, tool_call_id, message, schema, cancel, timeout) -> ElicitOutcome
pub enum ElicitOutcome { Accepted(Map), Declined, Cancelled, Expired, Unsupported }
```

- State: `HashMap<elicitation_id, Pending { request, tx }>` plus, per session,
  the set of attached connections that declared `supports_elicitation_form`.
- `Unsupported` is returned immediately when no attached connection supports
  forms. A tool never blocks on a request nobody can see.
- Fan-out: the request goes to every capable connection on the session. The
  first reply that passes `check_content` wins. An invalid reply gets
  `ServerEvent::Error` for that request id and the elicitation stays pending.
  A reply for an unknown or resolved id is answered `Done` and ignored.
- After a winner, `ElicitationResolved` goes to every connection on the
  session so the losers withdraw their requests.
- Catch-up: a capable connection that subscribes while requests are pending
  receives each one after its history snapshot.
- Cancellation: the tool's `graceful_shutdown_signal` (turn cancel) or the
  timeout removes the entry and broadcasts `Resolved{cancelled|expired}`.
- When the last capable connection detaches, pending requests stay until the
  timeout, so a reconnecting client can still answer them.

## 3. ACP adapter

1. **Agent-to-client requests** (`client_requests.rs`):
   - Keep `result`/`error` in `JsonRpcMessage`.
   - Issue string ids `"jcode-<n>"`, which cannot collide with the client's
     own ids in our responses because they live in the other direction.
   - Route method-less messages with an `id` to the pending map. Unknown ids
     are dropped (logged) and never answered.
   - The read loop only completes oneshots. Waiting happens in spawned tasks,
     so it cannot deadlock.
   - `cancel(id)` sends `$/cancel_request {requestId}` and drops the entry;
     a late reply is then ignored.
2. **Capability**:
   - At `initialize`, record `clientCapabilities.elicitation.form`, next to
     `client_advertises_session_notices`. It counts only as a non-null
     object; `{}` at the top level advertises no modes.
   - Subscribe with `supports_elicitation_form` accordingly.
3. **Opt-in**: `applet_elicitation = supports_elicitation_form &&
   tool selection explicitly names "applet" && !disabled("applet")`, computed
   from the adapter's own config, args and env. Report the effective value as
   `result._meta.jcode.appletElicitation` on new/load/resume only for clients
   advertising form support; missing means false.
4. **Mapping**:
   - `ElicitationRequest` →
     `elicitation/create {sessionId, toolCallId, mode:"form", message, requestedSchema}`.
   - The reply → `Request::ElicitationResponse`.
   - A JSON-RPC error reply, including `-32601` and `-32602`, → `Cancel`.
   - `ElicitationResolved` for a request still open at the client →
     `$/cancel_request`.
   - Connection close drops the outstanding client requests; the broker
     treats them as unanswered, not cancelled.

## 4. `applet` as the first user

- **Registration**: the daemon registers an elicitation-only `applet` for a
  session while at least one attached connection has `applet_elicitation`.
  - Same name and input schema.
  - Narrower description: form controls only, `wait=true` required, no
    Desktop placement, HTML or images.
  - A Desktop `supports_user_response` attachment keeps the full tool.
  - The explicit opt-in is exempt from a preset allowlist (`acp`, `minimal`)
    but not from an explicit `disabled` entry. It is the more specific setting.
- **View-to-form compiler** (`applet_elicit.rs`), walking bound controls in
  tree order:

  | Node | Form property |
  | --- | --- |
  | `input` | `string`; title from `placeholder`, else `bind`; `maxLength` cap |
  | `toggle` | `boolean`; title from `label` |
  | `select` | `string` with `oneOf: [{const: value, title: label}]` |
  | `tabs` | `string` with `oneOf` built from the tab ids and labels |
  | `button` with a custom action, exactly one | Accepting submits that action |
  | `button` with custom actions, several | Synthesized required `action` property, `oneOf` of the buttons |
  | `text`, `heading`, `markdown`, `key_value`, `code` | Plain text appended to `message`, URLs not linkified |
  | `stack`, `grid`, `scroll`, `card`, `spacer`, `divider` | Traversed or dropped |
  | `html`, `image` with `on_press`, `chip`, `list_item` with `on_press`, `host.*` actions | Compile error naming each node |

  - Defaults come from `state[bind]`.
  - Names collide only through duplicate `bind` keys, which is a compile error.
- **Flow**:
  - Only a `mount`/`update` with `wait=true` elicits.
  - Register the waiter, publish the snapshot (Desktop still renders the card
    if attached), then call `elicit` concurrently with the waiter.
  - If no Desktop is attached and `wait` is false, return a hint to retry with
    `wait=true`.
- **Answer**:
  - `Accepted(content)`: strip the synthesized `action`, merge the rest into
    state with `applets::set_state`, publish, and resolve through
    `deliver_to_waiter` with `format_action_message`. The agent sees the same
    `[applet action]` text whether Desktop or the ACP client answered.
  - `Declined`/`Cancelled`: resolve with `host.declined`/`host.cancelled`.
  - `Expired`/`Unsupported`: the existing "no user action" output.
  - If Desktop answers first, the waiter resolves and the tool calls the
    broker's `withdraw`, which broadcasts `Resolved{cancelled}`.

## Commits

Each commit is pushed to `fork/acp-elicitation` as it lands.

1. Contract section, plan, and traceability rows (this commit).
2. Protocol types, validator, wire additions; unit tests for schema and
   content validation, and serde round-trips.
3. ACP agent-to-client requests and response routing; unit tests with an
   in-memory transport, including late and unknown replies.
4. Broker and server hooks; tests for fan-out, first-valid-wins, invalid
   reply, catch-up, cancel and the immediate `Unsupported` result.
5. ACP mapping and capability; e2e scenario and fixture
   `tests/fixtures/acp/09-elicitation-form-v1.jsonl`.
6. Applet compiler, variant registration and opt-in; compiler table tests,
   registry tests mirroring `user_response_capability_updates_locked_tool_surface`.

## Acceptance

1. Without form capability or without the explicit opt-in, the tool surface
   and the recorded corpus are unchanged.
2. With both, a blocking `applet` call produces one `elicitation/create` whose
   schema matches the compiler table, and an accept returns the standard
   `[applet action]` text with merged state.
3. Decline and cancel return distinct actions. A schema-invalid accept is
   rejected and the request stays answerable.
4. `session/cancel` during a pending form sends `$/cancel_request` and ends
   the tool call promptly.
5. Two capable connections: exactly one answer applies, the other receives
   `$/cancel_request`. A Desktop answer withdraws the ACP request.
6. The adapter never answers a client response with an error, and never sends
   `elicitation/create` to a connection that did not advertise `form`.

## Deferred

- URL mode and `elicitation/complete`.
- MCP-server elicitation passthrough (`toolCallId` scope).
- Permission requests (`session/request_permission`).
- v2 `requires_action` state.
- A TUI form renderer.

## Implementation evidence

- Commits 2–6 are implemented. Protocol validation uses `jsonschema` and the
  transitive definitions copied verbatim from the pinned schema. ACP optional
  null constraints are normalized before validating accepted content.
- Acceptance 1: `acp_conformance_elicitation_requires_capability_and_opt_in`
  checks metadata and the provider tool surface. The existing named corpus
  replay tests pass, with no existing fixture changed.
- Acceptance 2–3: `acp_conformance_elicitation_form_v1` records and replays
  fixture 09, checks merged state and standard action text, retries an invalid
  accept, and distinguishes decline from cancel. Compiler table and selector
  collision tests cover the supported controls and button mapping.
- Acceptance 4–5: `acp_conformance_elicitation_peer_winner_and_turn_cancel`
  covers pending-form attachment, one peer winner, Desktop withdrawal, late
  replies, and prompt cancellation. Broker tests independently cover invalid
  replies, timeout, dropped tickets and immediate unsupported results.
- Acceptance 6: the no-capability e2e test and adapter unit tests cover response
  routing, lenient replies with metadata and future actions, unknown/late IDs,
  and JSON-RPC errors. Form emission waits for its tool's flushed progress
  notification without blocking the read loop, with a two-second fallback if
  that update never arrives (`form_proceeds_after_fallback_without_tool_progress`,
  using paused Tokio time).
- The named clause-index test and ten legacy corpus/index tests pass. Compiler,
  registry, broker and existing Desktop capability regression tests pass.
  Library clippy, fork formatting, and all four ratchets pass without budget
  increases. All-target protocol clippy has pre-existing `useless_vec` errors
  at `protocol_tests/core_events.rs:985` and `:999`.
- A selfdev build passed a scripted stdio runtime check on its own socket:
  capability metadata, accepted content with `_meta`, withdrawal, ignored late
  replies and JSON-RPC error cancellation. The shared daemon was not changed.
  Fork CI on the final pushed head remains the authority.

## Deviations

- Publish `elicitation_form` with the broker (commit 4), so intermediate commits
  do not advertise an unimplemented server capability.

- Record the applet-backed e2e fixture with commit 6, when its first tool user
  exists, rather than fabricating a daemon event for commit 5.
- An invalid ACP accept has completed its JSON-RPC request, so the adapter
  reissues the still-pending form under a new request ID. The broker also
  independently rejects invalid accepts without resolving its pending slot.
- Per user clarification, `appletElicitation` metadata is sent only to clients
  advertising form support; missing means false. Legacy corpus files stay intact.

- Move the Desktop registration method and applet-answer handler into
  fork-owned modules to keep the existing code-size ceilings unchanged. No
  budgets were raised; moved response sends now log delivery failures.
- The existing view model represents headings as `text` with heading style;
  no separate `heading` node is added. Noninteractive images contribute alt
  text; interactive images remain unsupported. Forms without a submit button
  use the custom action `submit`.
- Multiple buttons can share an action name with different arguments. The
  synthesized selector uses stable button indices and avoids bound field names
  by prefixing underscores, so no bound state is overwritten or stripped.
- Form requests wait up to two seconds for the originating tool's flushed
  in-progress update, then proceed even if that update is missing. This makes
  ordering best-effort while preserving liveness and observer catch-up.
