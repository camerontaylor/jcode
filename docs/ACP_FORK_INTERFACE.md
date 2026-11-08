# Jcode fork ACP interface contract

<!-- [C-OTHER-P01] -->
Status: implementation target, 2026-11-08, revision 3. Revision 2 amended
`mcpServers` handling, v2 close on shared sessions, the v2 rollout switch,
pinned reference copies and the named consumer. Revision 3 makes **ACP v1
with jcode extensions the primary target**, adds turn correlation on v1
prompt responses, skill clear and the shared fixture corpus, and defines how
consumers reference this document. It is a contract for the jcode fork and
its consuming tool, Paseo, to implement independently, not a claim that the
current binary implements the additions below. Baseline inspected:
`f5e45624c`.

<!-- [C-OTHER-P02] -->
Preserve existing v1 clients and ACP/TUI shared control. Complete the v1
profile of this contract first. Negotiated v2 with explicit delivery is a
later track that opens under the gate in the strategy; its sections below
remain the target for that track and are not required for full v1 support.
Keep execution authority in the daemon and protocol presentation in the
adapter. SDK adoption does not change this contract.

<!-- [C-OTHER-P03] -->
See the [full development strategy](ACP_FORK_STRATEGY.md) for architectural
direction, sequential implementation, dependencies and completion criteria.

<!-- [C-OTHER-P04] -->
Roadmap order: (0) verification baseline; (1) repair existing shared control
and advertise capability metadata; (2) reduce the core carry; (3) complete
the v1 target: mappings, detach, turn correlation, skill clear, fixture
corpus; then, gated, (4) separate version presentation and evaluate the SDK
and (5) add v2 and explicit delivery; (6) optional new features, which do not
wait for the v2 track.

## Referencing this contract

<!-- [C-REFERENCE-P01] -->
The canonical copy is `docs/ACP_FORK_INTERFACE.md` on branch `custom` of
`camerontaylor/jcode`:
<https://github.com/camerontaylor/jcode/blob/custom/docs/ACP_FORK_INTERFACE.md>.
Consumers link to that path and record the commit hash they built against;
they do not keep a derived copy, because two hand-synchronized contracts
drift. Where a consumer needs its own decisions written down, it keeps a
short "choices" document that cites this one by commit. The pinned ACP
references in `docs/acp-pins/` and the fixture corpus in `tests/fixtures/acp/`
are versioned with this file; cite the same commit for all three.

### What "v1 with jcode extensions" means

<!-- [C-SCOPE-P01] -->
The primary target is every row of this contract's v1 column plus the jcode
extensions: capability metadata, `_jcode/turn` observation, implicit steering
with turn correlation, `_jcode/detach`, config options with the legacy
aliases, skill selection and clear, list metadata, and the fixture corpus.
A consumer that implements only this is a complete consumer. It must not
assume v2 exists, and nothing here may be specified so that v1 behavior only
makes sense after v2 ships.

## Sources and precedence

<!-- [C-SOURCES-P01] -->
Standard fields and operations use the official documents; this document
defines jcode choices and extensions rather than copying the specifications.

<!-- [C-SOURCES-P02] -->
- [ACP v1](https://agentclientprotocol.com/protocol/v1/overview), including <!-- [C-SOURCES-P02-I1] -->
  [config options](https://agentclientprotocol.com/protocol/v1/session-config-options)
  and [extensibility](https://agentclientprotocol.com/protocol/v1/extensibility).
- [Pinned v2 migration guide](https://github.com/agentclientprotocol/agent-client-protocol/blob/7441d10/docs/protocol/v2/migration.mdx) <!-- [C-SOURCES-P02-I2] -->
  and [v2 schema](https://github.com/agentclientprotocol/agent-client-protocol/blob/7441d10/schema/v2/schema.json),
  draft `2.0.0-alpha.13`.
- [Pinned v2 prompt lifecycle](https://github.com/agentclientprotocol/agent-client-protocol/blob/7441d10/docs/protocol/v2/draft/prompt-lifecycle.mdx) <!-- [C-SOURCES-P02-I3] -->
  and [session setup](https://github.com/agentclientprotocol/agent-client-protocol/blob/7441d10/docs/protocol/v2/draft/session-setup.mdx).
- [Steering RFD #1261](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261), <!-- [C-SOURCES-P02-I4] -->
  [pinned delivery revision](https://github.com/kennethsinder/agent-client-protocol/blob/ee193af58f211a1cbfde30029d5283f9c94c554c/docs/rfds/v2/prompt-delivery.mdx).
  This remains a proposal, not stable ACP.
- [Current shared-session behavior](ACP_SHARED_SESSIONS.md), especially <!-- [C-SOURCES-P02-I5] -->
  Deviations 34–49; [gap research](ACP_V2_GAP_HANDOVER.md);
  [fork boundary strategy](FORK_BOUNDARY_STRATEGY.md).

<!-- [C-SOURCES-P03] -->
Verbatim copies of the pinned v2 documents, v2 schema, delivery RFD and
their license are kept in [`docs/acp-pins/`](acp-pins/README.md) with
SHA-256 hashes. The RFD is hosted on a personal fork of the spec repository
and may disappear; the copies, not the URLs, are the durable target. The
draft label `2.0.0-alpha.13` does not appear inside the pinned files; it was
inferred from release dates, so the commit hashes and file hashes are the
pin, not that label.

<!-- [C-SOURCES-P04] -->
For the target implementation, this document supersedes recommendations in
the reports where they differ. The reports remain useful evidence. Official
definitions govern standard operations; the named RFD governs explicit v2
delivery. A future spec revision does not silently change the pinned target:
update this contract, the pinned copies and both implementations together.

## Delivery stages

<!-- [C-STAGES-P01] -->
| Surface | Current baseline | Target before phase 6 |
| --- | --- | --- |
| ACP v1, shared attach, peer turns, implicit steering | Implemented, with known races | Preserved and repaired  <!-- [C-STAGES-P01-R1] --> |
| Model/effort, commands, skills, usage, raw diagnostic events | Implemented | Preserved  <!-- [C-STAGES-P01-R2] --> |
| Reasoning, plans, tool metadata | Incomplete mapping | Surface data the daemon already provides  <!-- [C-STAGES-P01-R3] --> |
| Capability metadata described below | Mostly absent | Added in every profile  <!-- [C-STAGES-P01-R4] --> |
| Detach without cancelling owned work | Not a distinct operation | `_jcode/detach`  <!-- [C-STAGES-P01-R5] --> |
| v1 prompt-response turn correlation | Not present | `_meta.jcode.turnId` and `role` on every successful v1 prompt response  <!-- [C-STAGES-P01-R6] --> |
| Skill clear | No operation | `skill: null` in prompt metadata  <!-- [C-STAGES-P01-R7] --> |
| Shared fixture corpus | None | `tests/fixtures/acp/`, replayed by both sides  <!-- [C-STAGES-P01-R8] --> |
| Client-supplied `mcpServers` | Accepted and silently ignored | Accepted, declared unsupported in metadata, reported by notice  <!-- [C-STAGES-P01-R9] --> |
| ACP v2 and explicit steer/queue | Not implemented | v2 track, gated; negotiated behind the `[acp] v2` switch when it opens  <!-- [C-STAGES-P01-R10] --> |
| v2 turn attribution metadata | Not implemented | v2 track; as specified below  <!-- [C-STAGES-P01-R11] --> |

<!-- [C-STAGES-P02] -->
The new client implements the v1 target first and completely. V2 support is
added only when the v2 track opens, and then alongside v1, not instead of it.
Full target support does not require explicit queueing: a client may hold
unsent text locally and label it as local. The client must not assume a
feature just because the executable is named jcode; it gates on the
advertised metadata below.

## Transport and feature discovery

<!-- [C-TRANSPORT-1] -->
Use the existing `jcode acp` stdio transport and ACP framing. Stdout contains
protocol traffic; diagnostics go elsewhere. One connection negotiates one
ACP version. Session IDs identify daemon conversations shared with TUI and
other ACP connections; attaching does not take ownership away from peers.

<!-- [C-PROFILE-1] -->
Profiles `standard`, `extended` and `full` remain presentation choices.
Essential shared control and feature discovery work in every profile.
`extended` and `full` additionally expose raw diagnostic events.

<!-- [C-DISCOVERY-P01] -->
V2 negotiation is controlled by a rollout switch that follows the profile
convention: configuration key `v2` in the `[acp]` table (boolean, default
`false`) with environment override `JCODE_ACP_V2` (`1`/`true` enables). Off
means the adapter negotiates v1 only, as today. On means v2 is offered and
v1 remains available on the same executable. There is no CLI flag. A client
that spawns `jcode acp` as a child process controls the switch per
connection by setting the environment variable on the child, without
touching the user's configuration. Remove
the switch, defaulting v2 on, only when the pinned draft is superseded by a
stable v2 and this contract is re-pinned. Until the v2 track opens, the
switch does nothing because no v2 presentation exists.

### Jcode capability metadata

<!-- [C-DISCOVERY-P02] -->
New target metadata lives at `agentCapabilities._meta.jcode` in the v1
initialize result and `capabilities._meta.jcode` in v2. Example for v1:

```json
{
  "interfaceVersion": 1,
  "profile": "standard",
  "extensions": ["shared_control", "peer_turns", "turn_correlation", "skill_selection"],
  "sessionMcpServers": false
}
```

<!-- [C-DISCOVERY-P03] -->
Extension names and what each promises:

<!-- [C-DISCOVERY-P04] -->
| Name | Promise |
| --- | --- |
| `shared_control` | `result._meta.jcode.sharedControl` is present on new/load/resume and `_jcode/detach` exists when its field is true  <!-- [C-DISCOVERY-P04-R1] --> |
| `peer_turns` | `_jcode/turn` notifications (v1 only)  <!-- [C-DISCOVERY-P04-R2] --> |
| `turn_correlation` | `_meta.jcode.turnId` and `role` on every successful v1 prompt response  <!-- [C-DISCOVERY-P04-R3] --> |
| `skill_selection` | `_meta.jcode.skill` on prompts, including `null` to clear  <!-- [C-DISCOVERY-P04-R4] --> |
| `raw_server_event` | `_jcode/server_event` diagnostics (extended/full profiles)  <!-- [C-DISCOVERY-P04-R5] --> |
| `turn_metadata` | v2 track only; replaces `peer_turns` on v2 connections  <!-- [C-DISCOVERY-P04-R6] --> |

<!-- [C-DISCOVERY-P05] -->
`interfaceVersion` versions the jcode extensions defined here, not ACP or
the daemon protocol. `sessionMcpServers` states whether client-supplied
`mcpServers` are connected for the session; see the MCP section below. It
exists because ACP v1 has no standard way to declare stdio MCP unsupported. Unknown extension names and metadata keys are ignored.
Additive keys do not require a version bump; incompatible semantics do.
The extensions array retains the existing `raw_server_event` name where
that profile enables it. In v2, replace `peer_turns` with `turn_metadata`.

<!-- [C-DISCOVERY-P06] -->
These markers describe adapter support. Effective daemon support is reported
in each successful new/load/resume response at `result._meta.jcode`:

```json
{
  "clientInstanceId": "opaque-attachment-id",
  "sharedControl": {
    "observe": true,
    "stopObservedTurn": true,
    "detach": true,
    "implicitSteering": true
  }
}
```

<!-- [C-CAPS-1] -->
All four fields are booleans. `implicitSteering` is v1-only and is false in
v2. True means the whole behavior is usable on this attachment, including
required daemon support. Missing/false means unavailable, not an invitation
to probe by submitting work. `clientInstanceId` identifies this attachment
for comparison with origin metadata; it is not a user identity or credential.
Repeated `session/load` or `session/resume` of the same live attachment retains
its id and active-turn bracket. Loading a different session opens a distinct
attachment. Reconnecting after EOF opens a fresh attachment with a fresh id.
A daemon restart invalidates the attachment;
reconnect and rediscover rather than retaining its capability snapshot.

<!-- [C-DISCOVERY-P07] -->
For v2, advertise standard delivery capabilities only when the serving
backend can honor them for sessions on the connection. Do not advertise
optimistically based on adapter version. Use v1 or fail session setup clearly
if backend drift makes the negotiated v2 contract impossible.

<!-- [C-DISCOVERY-P08] -->
Old binaries have no `interfaceVersion`. Existing consumers can retain their
legacy behavior; new consumers must not send new extensions to those binaries.

## Standard operations and jcode choices

<!-- [C-OPERATIONS-P01] -->
| Operation | v1 target | v2 target |
| --- | --- | --- |
| Initialize | Standard v1 fields, plus metadata above | Standard negotiation and v2 fields, plus metadata above  <!-- [C-OPERATIONS-P01-R1] --> |
| New session | Standard; return config options and attachment metadata | Standard equivalent  <!-- [C-OPERATIONS-P01-R2] --> |
| List | Standard list/pagination, plus optional liveness metadata | Standard equivalent  <!-- [C-OPERATIONS-P01-R3] --> |
| Attach with replay | `session/load` | `session/resume` with `replayFrom: {"type":"start"}`  <!-- [C-OPERATIONS-P01-R4] --> |
| Attach without replay | Advertised `session/resume` | `session/resume` without `replayFrom`  <!-- [C-OPERATIONS-P01-R5] --> |
| Prompt | Response ends the prompt's work, with v1 stop reason | Standard insertion response; completion through state updates  <!-- [C-OPERATIONS-P01-R6] --> |
| Cancel | Existing jcode Stop semantics below | Standard baseline cancellation; RFD exception for delivery prompts  <!-- [C-OPERATIONS-P01-R7] --> |
| Close | Existing owner-aware close below | Standard v2 close when this is the sole live attachment; attachment-scoped close when peers are present (see below)  <!-- [C-OPERATIONS-P01-R8] --> |
| Config | `session/set_config_option` | Standard v2 config route and typed selectors  <!-- [C-OPERATIONS-P01-R9] --> |
| Detach view | `_jcode/detach` | `_jcode/detach`  <!-- [C-OPERATIONS-P01-R10] --> |

<!-- [C-OPERATIONS-P02] -->
List entries carry `sessionId`, `cwd`, and when stored, `title` and
`updatedAt` (RFC 3339), newest first. A consumer's import or picker should
render previews from these fields and attach only to the session the user
selects; probing each entry with `session/load` to build a preview is
unnecessary and attaches to every probed session. For list entries,
`_meta.jcode.live: boolean` remains advisory: omission means
unknown, false means not live at the snapshot. It is not a running/idle flag.
Working-directory filters use canonical paths. Treat pagination cursors as
opaque and do not assume a fixed page size or a transactional listing.

<!-- [C-REPLAY-1] -->
V1 replay preserves stored-message grouping but live and replay IDs are not
guaranteed identical. On reload, rebuild the replayed transcript instead of
merging it with a cached live transcript by guessed ID equivalence. In v2,
use the standard replay/upsert rules; preserve an inserted message's ID into
stored replay when that message is retained. Live-only/synthetic items need
not appear in replay. Neither version promises durable exactly-once submission
across a broken connection. Do not automatically resend an uncertain prompt.

## Shared control semantics

<!-- [C-STOP-1] -->
There is one execution authority and one authoritative insertion order per
daemon session, including submissions from TUI and multiple ACP adapters.
Observing or steering a peer turn does not transfer ownership of it.
Cancellation checks and effects must be atomic relative to turn replacement.
No stale stop may cancel a successor turn. A stop received while no turn is
active must not arm cancellation for future work.

<!-- [C-SHARED-P01] -->
| Action | v1 | v2 |
| --- | --- | --- |
| Stop | Withdraw this attachment's waiting steers, then stop its observed turn | Stop the active turn; explicit delivery prompts follow the RFD, ordinary held prompts follow baseline v2  <!-- [C-SHARED-P01-R1] --> |
| Leave without stopping | `_jcode/detach` | `_jcode/detach`  <!-- [C-SHARED-P01-R2] --> |
| Close | Withdraw this attachment's waiting steers; cancel only its owned turn; adopted/peer turn continues | Sole live attachment: standard close, stopping all session work. Peers present: withdraw this attachment's pending input, cancel only work it originated, finish its final updates, detach; adopted/peer-origin turns continue  <!-- [C-SHARED-P01-R3] --> |
| Transport loss | Withdraw this connection's pending input; preserve already-inserted shared work as the target policy | Same  <!-- [C-SHARED-P01-R4] --> |

<!-- [C-CLOSE-1] -->
A client must not send `session/cancel` as a generic step before close or
teardown. V1 close is already owner-aware: it cancels this attachment's own
turn and leaves adopted and peer turns running. A cancel issued first turns
that safe release into a stop of whatever turn the client was observing,
which on a shared session is often another user's work. Cancel is for the
user's explicit Stop, nothing else.

### V2 close on a shared session

<!-- [C-V2CLOSE-P01] -->
The pinned v2 draft says closing "tells the Agent to cancel any ongoing work
for a session", and separately that "whether a session can be active on more
than one connection at once is up to the Agent". Jcode sessions are shared
with the TUI and other ACP connections, so a literal reading would let any
standard v2 client stop a TUI user's turn by closing a window. Revision 1 of
this contract accepted that and required clients to call `_jcode/detach`
instead; a third-party client cannot know to. The rule is now:

<!-- [C-V2CLOSE-P02] -->
- **Sole live attachment.** Standard v2 close applies in full: cancel
  foreground work as `session/cancel` would, send its final updates and the
  idle `state_update` with `stopReason: "cancelled"`, answer never-inserted
  prompts with `-32800`, stop background work, respond `{}`, then send
  nothing further for the session on this connection.
- **Other live attachments exist.** Close is attachment-scoped. Cancel only
  turns this attachment originated and still owns, withdraw its pending
  input (`-32800` for never-inserted prompts), send the final updates and
  idle state for anything it cancelled, release its subscriptions and
  respond `{}`. Adopted and peer-origin turns continue. The session stays
  active for the peers; the standard "ends it on the connection but doesn't
  discard it" wording still holds for this connection.
- In both cases close is idempotent and a missing or inactive session
  returns the same empty result.

<!-- [C-V2CLOSE-P03] -->
This is a documented deviation from the literal draft, consistent with the
owner-aware v1 close and with the shared-session authority rules above.
`_jcode/detach` remains the operation for leaving without stopping anything,
including work this attachment started. Paseo still distinguishes **leave
view** from **stop and close session**; the deviation protects clients that
do not. Standard close never grants permission to delete stored history.
If a client needs "stop everything regardless of peers", that is
`session/cancel` on the shared turn followed by close, and it is a product
decision in the client, not a default.

### Detach extension

<!-- [C-DETACH-P01] -->
Request, in either version:

```json
{"jsonrpc":"2.0","id":30,"method":"_jcode/detach","params":{"sessionId":"s1"}}
```

<!-- [C-DETACH-1] -->
Success result is `{}`. Detach removes this connection's attachment only,
withdraws its never-inserted input, and releases its subscriptions. It does
not cancel already-inserted work, even work originally started here, and it
does not remove stored history or other attachments.

<!-- [C-DETACH-2] -->
Before answering, settle this attachment's pending prompt requests. In v1,
finish outstanding prompt waits with `stopReason: "cancelled"`; this means
the local wait ended, not that shared execution stopped. Close any open
local `_jcode/turn` bracket with `canceled`. In v2, never-inserted prompts
receive `-32800`; already-inserted ones retain a successful insertion result.
Do not invent a session-wide idle transition for a detach. No session updates
are sent on this attachment after the detach response. Resume to attach again.

<!-- [C-DETACH-3] -->
Repeated detach of an already-detached session returns `{}`. Invalid parameter
shapes return `-32602`; an unadvertised extension returns `-32601`. The client
must require effective `sharedControl.detach` before calling it.

## V1 steering and peer lifecycle

<!-- [C-STEER-1] -->
Preserve implicit steering when `sharedControl.implicitSteering` is true.
A normal prompt sent during active work is inserted at a safe point in that
turn. A prompt sent while idle starts work. Several outstanding requests may
share a turn, but each receives its own final response. Usage/output must
not be counted again for every request sharing that turn.

<!-- [C-STEER-2] -->
When steering joins a peer turn, the existing adoption behavior remains:
the connection follows its remaining output and resolves the prompt at turn
end. An already-open peer bracket still receives exactly one terminal;
adoption does not produce another start. Own prompt/steer text is not echoed
by default in v1. The client retains its submitted text; other attachments
receive peer-user updates. This deliberately defers the report's suggested
own-echo change to v2 rather than creating another v1 behavior mode.

### Turn correlation on prompt responses

<!-- [C-CORRELATION-1] -->
With `turn_correlation` advertised, successful v1 `session/prompt` results
carry, beside `stopReason`:

```json
{"stopReason":"end_turn","_meta":{"jcode":{"turnId":"opaque-turn-id","role":"origin"}}}
```

<!-- [C-CORRELATION-P01] -->
- `turnId` is the same opaque identity used in `_jcode/turn`. <!-- [C-CORRELATION-P01-I1] -->
- `role` is `origin` when this request started the turn, `steer` when it was <!-- [C-CORRELATION-P01-I2] -->
  inserted into a turn already running, or `none` when no turn ran (for
  example a selection-only command, bare skill invocation, or skill clear).
  When `role` is `none`, `turnId` is null.
- Several responses with the same `turnId` describe one execution. The <!-- [C-CORRELATION-P01-I3] -->
  consumer settles completion, usage and terminal display once per `turnId`;
  the `origin` response is the natural place, and a `steer` response that
  arrives first must not be treated as a second completion.
- Responses finished by detach or local close still carry the `turnId` they <!-- [C-CORRELATION-P01-I4] -->
  were following, with `stopReason: "cancelled"`; the shared turn may be
  continuing.

<!-- [C-CORRELATION-2] -->
When work runs without a daemon turn identity (including legacy daemons without
`turn_origin`), omit `_meta.jcode` turn correlation: absence means unknown,
whereas `role: "none"` means no turn ran.

<!-- [C-BUSY-1] -->
Busy rejections keep the legacy JSON-RPC error: there is no result, so no
`_meta` turn correlation.

<!-- [C-CORRELATION-P02] -->
This is informational. A client that ignores it gets today's behavior.

<!-- [C-BUSY-2] -->
Without effective implicit steering, do not offer it. Busy requests may fail
as in the legacy implementation. Do not introduce `delivery` or
`_meta.jcode.delivery` on v1. Queueing UI may hold unsent text locally, but
must label it as local, not as accepted shared-session work.

### Peer turn notification

<!-- [C-PEER-P01] -->
Existing agent-to-client notification, unchanged:

```json
{"jsonrpc":"2.0","method":"_jcode/turn","params":{"sessionId":"s1","turnId":"opaque-turn-id","state":"started","origin":{"kind":"client","clientInstanceId":"opaque-peer-id"}}}
```

<!-- [C-PEER-1] -->
- `state`: `started`, `completed`, `failed`, or `canceled` (one `l`).
<!-- [C-PEER-2] -->
- `origin` accompanies start. `kind` is currently `client`, `swarm`,
  `background`, `relay`, or `debug`; display unknown future values generically.
  `clientInstanceId` is optional. Do not infer TUI versus ACP from its spelling.
- A failure can include `error: {"message":"..."}`.
<!-- [C-PEER-3] -->
- IDs are opaque. A start precedes its peer output and one terminal follows
  that output. Mid-turn attachment opens the observed active bracket.
<!-- [C-PEER-4] -->
- This is a peer-view bracket, not a lifecycle event for every own prompt.
  A local close/detach can end the bracket while the shared turn continues.
<!-- [C-PEER-5] -->
- Peer content arrives through ordinary `session/update`; the bracket carries
  attribution/lifecycle only. Continue reading notifications while no request
  is outstanding. Do not count adoption as a second turn.

## V2 lifecycle and explicit delivery

<!-- [C-V2DELIVERY-P01] -->
Implement the pinned v2 baseline and the pinned delivery RFD by reference.
Use their standard `delivery` field and capability location directly under
an explicit experimental rollout; do not invent an interim `_meta` spelling.
Both sides pin the same RFD revision. Missing SDK types are a tooling problem,
not a reason to introduce a second public API.

<!-- [C-V2DELIVERY-P02] -->
Support both advertised `steer` and `queue`. Apply the RFD's insertion,
withdrawal, turn-end, ordering and cancellation rules. Started tools are not
cancelled by a steer. The consumer should track outstanding requests separately
from session activity; a prompt response is not a turn-completion signal.

<!-- [C-V2DELIVERY-P03] -->
Jcode chooses to hold ordinary busy v2 prompts until foreground work ends,
rather than implicitly steering. These remain ordinary held requests, with
baseline cancellation semantics, not explicit-delivery queue entries. At a
turn boundary, process waiting delivery work in RFD order, then ordinary held
prompts in receipt order, each starting a turn. New submissions join their
applicable pending class. No silent conversion of an ordinary request into
a cancellation-surviving queue item is allowed. The intended client sends
explicit delivery whenever the user specifies steer or queue.

<!-- [C-V2DELIVERY-P04] -->
The daemon establishes the shared receipt order; independent adapter queues
must not race to claim the next turn. A stop from a legacy v1/TUI client can
stop the common turn but must not erase another attachment's pending steers
or explicit-delivery queue entries. Ordinary held v2 prompts retain the
baseline v2 cancellation rules.
Transport loss and detach withdraw only that attachment's pending requests.

### Turn attribution metadata

<!-- [C-V2META-P01] -->
V2 uses standard `state_update` as its only busy/idle authority. Do not emit
v1 `_jcode/turn` notifications on v2 connections. With `turn_metadata`
advertised, augment live `state_update` objects at `update._meta.jcode`:

```json
{
  "turn": {
    "turnId": "opaque-turn-id",
    "origin": {"kind":"client","clientInstanceId":"opaque-peer-id"}
  }
}
```

<!-- [C-V2META-P02] -->
Use the same origin shape as v1. Running/requires-action snapshots carry the
active turn; terminal idle updates carry the turn just ended. An idle attach
snapshot with no applicable turn has `turn: null`. Every state snapshot sets
this field explicitly so clients do not retain attribution from an older turn.
It is informational: a standard client can ignore it and still operate.

<!-- [C-V2META-P03] -->
Live user-message and agent-message updates also carry
`update._meta.jcode.turnId` where known. User messages additionally carry
`update._meta.jcode.origin` for their submitter, which can differ from the
turn's origin after steering. The standard `messageId` correlates an own
insertion response and echo; no additional request-correlation extension is
needed. Historical items may omit origin and turn association when not stored.

## Configuration, commands and skills

<!-- [C-CONFIG-1] -->
Use config options as the primary structured model/effort interface:

<!-- [C-CONFIG-P01] -->
- Option identifiers remain `model` and `reasoning_effort`. Use the negotiated <!-- [C-CONFIG-P01-I1] -->
  ACP version's option schema; v1 option `id` is not a v2 `configId`.
- Render advertised choices, including future provider models. Do not embed <!-- [C-CONFIG-P01-I2] -->
  a release-specific model or effort allowlist in the consumer.
- Options describe session state shared with TUI. Show the acknowledged <!-- [C-CONFIG-P01-I3] -->
  effective value and follow updates from any client. A model change can
  replace the effort choices or remove the effort option entirely.
- Preserve current nearest-supported effort normalization and `auto`/`default` <!-- [C-CONFIG-P01-I4] -->
  reset behavior. Do not assume the requested value was the applied value.
- Submit config changes while idle; handle a busy response without claiming <!-- [C-CONFIG-P01-I5] -->
  success. Configuration changes must not partially apply on error.

<!-- [C-CONFIG-P02] -->
Keep existing v1 `session/set_model` and `session/set_reasoning_effort` aliases
for compatibility, but the new consumer uses config options. Do not add these
unprefixed custom methods to v2, or create redundant namespaced aliases.

<!-- [C-CONFIG-P03] -->
Legacy alias parameters are `{sessionId, modelId}` (also accepts `model`)
and `{sessionId, effort}` (also accepts `reasoningEffort`), respectively.
Values are strings; aliases delegate to config options and return the same
result, including the effective config option set. They are compatibility
entry points, not independently advertised capabilities.

<!-- [C-CONFIG-P04] -->
Discover commands from `available_commands_update`. Preserve `/model`,
`/models`, `/effort`, `/compact`, and `/subagent`; display advertised syntax.
`/compact` schedules compaction for the next prompt. `/subagent` reports the
launch through tool activity; completion of that launch is not completion
of the worker. No structured child-session API is promised in this phase.

<!-- [C-CONFIG-P05] -->
Commands are submitted as text prompts. The consumer waits for idle before
submitting mutation/subagent commands. V1 may reject a busy command; v2 holds
an otherwise valid ordinary command prompt until it can execute. Delivery
on a control command is rejected with `-32602` before side effects; delivery
applies to conversational input, not command scheduling.

<!-- [C-CONFIG-P06] -->
In v2, successful command/skill submissions still use the standard insertion
response and user-message echo. Report resulting notices through session
updates; do not return a v1 stop-reason result. A selection-only command need
not manufacture a daemon model turn.

<!-- [C-CONFIG-P07] -->
Canonical explicit skill selection is prompt metadata:

```json
{"_meta":{"jcode":{"skill":"skill-name"}}}
```

<!-- [C-SKILL-1] -->
`skill_selection` advertises this field. It takes precedence over textual
`/skill-name` or `$skill-name` recognition. Existing metadata aliases remain
v1 compatibility only. Unknown/path-like slash text remains conversational
text; a leading space escapes command/skill interpretation. The adapter
remembers the selected skill on its session attachment for subsequent prompts;
this is not a shared, synchronized config option. Its use affects shared work.
Sending `{"_meta":{"jcode":{"skill":null}}}` clears the attachment's sticky
selection before the prompt text is interpreted; with empty prompt text it is
a selection-only action and starts no model turn, with text it clears and
then submits that text unskilled. A missing `skill` key leaves the selection
unchanged. A bare skill invocation
selects it without requiring a model turn. A busy steer must not claim to
change a sticky skill: if explicit selection cannot be honored, reject before
insertion instead of silently
dropping it. Queue or wait for idle to change the skill.

## Content and diagnostics

<!-- [C-CONTENT-P01] -->
Retain text, image and embedded-context prompt support; no audio support is
promised. Use standard content shapes, not parser aliases. Render assistant
text, exposed reasoning, plans/todos, tool progress/results, available usage,
session information, commands and config updates. Tool names, locations and
diffs are populated when authoritative data exists; absence is legitimate.
No hidden reasoning or fabricated tool metadata is implied.

<!-- [C-CONTENT-P02] -->
V2 uses its standard replacement/upsert semantics. V1 cannot faithfully
represent every replacement; do not append a replacement as if it were new
text. Where v1 lacks a suitable standard shape, retain a best-effort display
and treat reload as authoritative; do not invent another message-edit protocol.

<!-- [C-CONTENT-P03] -->
The extended/full diagnostic notification remains:

```json
{"jsonrpc":"2.0","method":"_jcode/server_event","params":{"sessionId":"s1","event":{"type":"daemon-event-type"}}}
```

<!-- [C-CONTENT-P04] -->
`event` is the daemon's serialized event, with no ACP stability guarantee.
It can duplicate information in standard updates. Render it in diagnostics
only; do not base shared control, billing/usage totals or core UI state on it.
Unknown event types are ignored. Supporting every stable ACP capability does
not require exposing every daemon-internal event as a product feature.

## Unsupported features and error behavior

<!-- [C-ERRORS-P01] -->
Before phase 6, do not advertise new auth flows, client filesystem/terminal
execution, elicitation, per-tool cancellation, session deletion/fork/archive,
or provider-management APIs. Existing `/subagent` is retained without adding
the experimental structured subagent protocol. Reasoning, plans and metadata
mapping do not imply a new permission system.

<!-- [C-ERRORS-P02] -->
Provider authentication and tool approvals remain daemon-side. The client
must not present a nonfunctional Approve button or infer consent from a prompt.
If work requires an unavailable approval interaction, report the condition
clearly; implementing a new ACP approval flow is separate scope.

<!-- [C-ERRORS-P03] -->
Client-supplied session MCP servers remain unconnected before phase 6, but
the field stays **accepted**. Revision 1 required rejecting nonempty input
with `-32602`; that reverses upstream commit `0517f01f2` (issue 887, where
Zed could not create a session once it had any context server configured)
and would also break Paseo, which sends the session's configured servers on
every `session/new` and `session/load`. ACP v1 makes stdio MCP support
mandatory and `mcpCapabilities` only covers HTTP and SSE, so there is no
standard field for declining the servers. The target behavior is:

<!-- [C-ERRORS-P04] -->
- Accept a missing, null or array `mcpServers`; reject a non-array shape <!-- [C-ERRORS-P04-I1] -->
  with `-32602`, as today. Continue to advertise `mcpCapabilities` with
  `http` and `sse` false.
- Declare the limitation with `sessionMcpServers: false` in the capability <!-- [C-ERRORS-P04-I2] -->
  metadata, so Paseo and other jcode-aware clients can show the user that
  session-scoped servers are not in effect, and send them or not as they
  prefer.
- When a nonempty array is ignored, send one `notice` session update <!-- [C-ERRORS-P04-I3] -->
  (`severity: "info"`) on that connection after the new/load/resume response,
  naming the ignored server names and pointing at the daemon-side MCP
  configuration: send it **when the client advertised
  `clientCapabilities.session.notices`** at `initialize`, and otherwise send
  none. V1 only permits `notice` updates to a client that advertised that
  capability, and a message update is not a substitute. It is a notice, not an
  error, and it must not change the response.
- Daemon-configured MCP tools remain usable. Connecting client-supplied <!-- [C-ERRORS-P04-I4] -->
  stdio servers for the session is a phase 6 item, to be taken up when a
  consumer needs it; flipping `sessionMcpServers` to true is how it is
  announced.

<!-- [C-ERRORS-P05] -->
V2 follows standard error codes and insertion-boundary semantics. In v1,
preserve existing error behavior except explicitly documented corrections;
the new client must tolerate legacy unknown-session `-32602` rather than
depending on v2's `-32002` in v1. Never silently accept an unsupported behavior
field. Malformed extension parameters receive `-32602`; unknown methods
receive `-32601`. No parsing of human-readable error strings is required.

## Independent acceptance scenarios

<!-- [C-ACCEPTANCE-P01] -->
Both implementations should exercise the same observable scenarios. Scenarios
1 through 5, 7 and 8 are the v1 target; scenario 6 and the v2 halves of 5 and
8 belong to the v2 track.

### Fixture corpus

<!-- [C-CORPUS-1] -->
The executable form of these scenarios is the corpus in `tests/fixtures/acp/`
of the jcode repository, versioned with this file:

<!-- [C-CORPUS-P01] -->
- One JSON Lines file per scenario and version, named <!-- [C-CORPUS-P01-I1] -->
  `<nn>-<scenario>-v1.jsonl`. Each line is an object with `seq` (monotonic
  integer), `dir` (`c2a` or `a2c`), and `msg` (the framed JSON-RPC message
  exactly as sent). Opaque identities are recorded as emitted; a replayer
  treats them as opaque and matches structure, not values.
- jcode records the files from its e2e shared-session scenarios against the <!-- [C-CORPUS-P01-I2] -->
  mock provider and checks them in CI: a replay through the adapter must
  reproduce the `a2c` lines up to opaque values, so any wire change updates
  the corpus in the same commit.
- Paseo replays the `a2c` lines into its client and asserts the `c2a` lines <!-- [C-CORPUS-P01-I3] -->
  and its own resulting state. A consumer change that needs a different
  exchange is a contract change and is made here first.
- The corpus is the first thing to update when either side finds the prose <!-- [C-CORPUS-P01-I4] -->
  ambiguous; prose follows the recorded exchange.

<!-- [C-CORPUS-P02] -->
1. Create, list, replay and attach to a TUI session mid-turn; preserve ordering <!-- [C-CORPUS-P02-I1] -->
   between replay and live events and show the current activity correctly.
2. Start from either TUI or ACP; observe the same work from two ACP connections. <!-- [C-CORPUS-P02-I2] -->
   Steering/adoption must not duplicate output, usage or turn completion.
3. Stop followed immediately by another prompt; a delayed stop cannot hit the <!-- [C-CORPUS-P02-I3] -->
   successor. Producer failure/abort always yields a correctly classified end.
4. Detach the originator and an observer independently; inserted work continues, <!-- [C-CORPUS-P02-I4] -->
   only the departing attachment's pending input is withdrawn.
5. Verify v1 close remains owner-aware; v2 close follows standard semantics <!-- [C-CORPUS-P02-I5] -->
   as the sole attachment and attachment-scoped semantics with a live peer,
   so a TUI turn survives a v2 client's window close.
6. V2 steer, queue and ordinary held prompts: test their distinct cancellation <!-- [C-CORPUS-P02-I6] -->
   behavior, concurrent producers, insertion/withdrawal races and disconnect.
7. Model/effort changes from either interface propagate; skills, commands, <!-- [C-CORPUS-P02-I7] -->
   compaction and subagent launch retain their existing capabilities.
8. Negotiate v1/v2, absent capabilities, legacy daemons and unknown metadata; <!-- [C-CORPUS-P02-I8] -->
   unsupported actions are not silently executed under another meaning.

<!-- [C-CORPUS-P03] -->
Use existing ACP JSON-shape/router tests and
`tests/e2e/acp_shared_session.rs` as the jcode foundation. On the Paseo side
(`camerontaylor/paseo`, branch `custom`), the generic ACP client lives in
`packages/plugin/src/server/acp-internal/connection.ts` with tests in
`packages/plugin/src/server/acp.test.ts` and
`packages/server/src/server/agent/providers/generic-acp-agent*.test.ts`; the
e2e ACP fixtures are `packages/app/e2e/support/fixtures/catalog-acp.cjs` and
`packages/app/e2e/support/fixtures/acp-chunks-plugin/`. The jcode-specific
provider plugin and its `_jcode/turn` transformer test are on the unmerged
branch `ct/jcode-provider-plugin` under `plugins/jcode-provider/`. Add the
consumer-side fixtures for this contract there; do not require both
implementations to land at once. Runtime checks use a dedicated daemon socket and the intended binary.
The fork CI workflow established in strategy Phase 0, run on the exact
`custom` head, is the validation authority; this document adds no new
approval process.

## SDK trial and build impact

<!-- [C-SDK-P01] -->
The bounded SDK trial belongs to the v2 track and is scheduled when that track opens; it may be run earlier as unscheduled research.
Use an isolated experimental checkout because the dependency graph and adapter
will differ from active repair work. The trial produces evidence and a small
representative prototype, not a second production adapter. Rebase/recheck
against the repaired v1 baseline before integrating it. Do not combine SDK
integration, lifecycle refactoring and v2 behavior changes in one change.

<!-- [C-SDK-P02] -->
Compare the current implementation against SDK v1 and SDK v1+v2 variants,
pinning SDK/schema versions and recording compiler, target, profile, feature
set, job count and host. Measure:

<!-- [C-SDK-P03] -->
| Dimension | Required comparison |
| --- | --- |
| Dependency cost | Added crates, feature unification, lockfile changes, supported targets/MSRV  <!-- [C-SDK-P03-R1] --> |
| Cold build | Clean build with a populated dependency download cache; elapsed time and peak memory  <!-- [C-SDK-P03-R2] --> |
| Daily edit loop | Warm no-op, ACP-only edit, and representative non-ACP edit; elapsed rebuild/link time  <!-- [C-SDK-P03-R3] --> |
| Packaging | Production binary and archive sizes under identical strip/LTO settings; build disk usage  <!-- [C-SDK-P03-R4] --> |
| Coupling | `serde_json/preserve_order` effects on iteration, serialized provider requests and cache-sensitive bytes  <!-- [C-SDK-P03-R5] --> |
| Runtime compatibility | Concurrent requests, extension dispatch, parser compatibility, startup and session behavior  <!-- [C-SDK-P03-R6] --> |

<!-- [C-SDK-P04] -->
Use separate target directories, consistent cache conditions and repeated
measurements; report absolute values and deltas, not a single noisy timing.
Run heavy builds on saturn, pluto, ceres or CI, following host memory guidance.
Inspect `cargo tree -e features` and Cargo timings to explain regressions.
An optional feature within the same binary does not isolate unified Cargo
features once enabled.

<!-- [C-SDK-P05] -->
The original adapter commit `f0266100a` records no reason for avoiding the
SDK. Build cost is a hypothesis to test, not established upstream intent.
Prefer adoption if protocol-maintenance savings justify measured build and
compatibility costs. If not, retain the hand-written transport and proceed
with this interface. Neither a new sidecar nor a full rewrite is required.