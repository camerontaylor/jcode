# Plan: port ACP shared sessions onto upstream preview.199

Prepared 2026-11-07. Follows `docs/UPSTREAM_PREVIEW_199_REBASE_REPORT.md`.
Proposed, not yet executed. Every upstream claim below cites code at
`v1.0.0-preview.199` (`7fd42339c`); fork claims cite `custom` (`39c7ecfda`).

## 1. Decision

Use **upstream's turn identity as the single turn model**. Add a small,
additive **origin extension** to it in the daemon. Keep the ACP-facing contract
(`_jcode/turn`, `session/list`, messageIds) unchanged, so Paseo is unaffected.

Do **not** replay the 53 commits as they are. Build a new branch from the
target tag and port the fork in thematic stacks (section 6). Drop the fork's
parallel lifecycle infrastructure once each of its guarantees is covered.

### Why

There are two contracts, and only one of them collides with upstream:

| Contract | Producer → consumer | Collides upstream? | Plan |
|---|---|---|---|
| Daemon wire (`turn_started`, `History.active_turn`, …) | jcode daemon → jcode ACP adapter, same binary | **Yes** (`turn_started` name, payload) | Follow upstream; extend additively |
| ACP `_jcode/turn` notification `{sessionId, turnId: string, state, origin{kind, clientInstanceId}}` | ACP adapter → Paseo (`paseo/specs/jcode-provider-plugin.md:69-75`) | No | Keep byte-compatible |

Upstream's model (`TurnIdentity{runtime_generation, turn_id}`, `turn_event`
envelope, `turn_state` before History, publication pin) does fence stale
events. It was designed for an observer that already knows which turn is its
own, so it cannot attribute a turn to the client that started it:

| Gap in upstream | Evidence (at tag) | Fork needs it for |
|---|---|---|
| `turn_started` carries no origin, no client instance id, no request id | `wire/events.rs:99-100`; `ack{id}` has no identity (`client_lifecycle.rs:1005`) | Telling its own prompt from a peer's; cancel-on-close of its own turn only |
| Prompt text is outside the turn scope | `peer_user_message` is unscoped, sent only for `message` turns and racing `turn_started` (`client_lifecycle.rs:1155-1166`) | Announcing a peer's prompt; suppressing a re-announce on attach |
| No terminal reaches peers when cancel aborts the task | `client_processing.rs:525-533`: `handle.abort()` runs after 500 ms, and its `done` never enters the channel | Paseo's run never ends ("running" forever) |
| No explicit end status | Only `done`/`error` plus an unscoped `turn_stopped` | `_jcode/turn` `completed / failed / canceled` |
| No daemon capability | Pong capabilities lack any turn entry (`session_connections.rs:38-43`) | Old-daemon fallback (Paseo spec, check 6) |
| `RunSubagent` takes no lease and sends flat events to the requester only | `client_actions.rs:303-380` | `/subagent` serialization and pairing |

Upstream does cover several fork guarantees, so those parts can be dropped:

- **Restart-unique ids.** `runtime_generation` is a fresh UUID for each Agent
  (`turn_ownership.rs:56-65`).
- **Monotonic per-runtime counter.** `turn_ownership.rs:134-150`.
- **Ordering.** The publication pin stops turn N+1 from starting before turn
  N's output has drained (`state.rs:516,574-579`).
- **Panic → terminal.** `catch_unwind` at `client_processing.rs:296-321`.
- **One turn across failover retries.** Retries reuse the lease (`turn_failover.rs`).

## 2. Target daemon wire contract

All changes are additive, with serde defaults. Upstream TUI and harness
readers ignore the new optional fields. No existing event changes shape.

```rust
// jcode-protocol wire/events.rs: extend upstream variants
TurnStarted {
    identity: TurnIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<TurnOrigin>,        // fork: kind, client_instance_id, request_id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_content: Option<String>,
}
TurnState {
    session_id, runtime_generation, turn_id, latest_turn_id,   // upstream
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_origin: Option<TurnOrigin>,          // replaces History.active_turn
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_user_content: Option<String>,
}
// New variant. The name is free upstream, but it is NOT the old fork shape.
#[serde(rename = "turn_ended")]
TurnEnded { identity: TurnIdentity, status: TurnEndStatus, message: Option<String> },
```

- **Capability.** Advertise `"turn_origin"` in `server_features` (and Pong).
  Do **not** reuse `"turn_lifecycle"`, because its payload changed. Older fork
  adapters on a new daemon then fall back to legacy behaviour instead of
  misparsing events.
- **Opt-in.** The ACP adapter subscribes with `supports_turn_identity: true`.
  Upstream ACP does not (`acp.rs:843,922`). Without the flag, the daemon
  unwraps envelopes for legacy readers (`client_lifecycle.rs:84-98`).
- **Canonical string id.** `turnId = "{runtime_generation}:{turn_id}"`. It is
  used for `_jcode/turn` and for peer messageIds (`{session}:peer:{turnId}:{n}`,
  `{session}:{turnId}:user`). The UUID generation keeps it unique across
  restarts. This replaces `68e7a5931`.
- **`stored_index` on `HistoryMessage`.** Keep it (`f0d73790b`). Upstream
  renders it (`session/render.rs:409`) but does not put it on the wire. It is
  independent of turn identity.

## 3. Daemon changes (replacing `turn_lifecycle_registry.rs` and `server/turn_lifecycle.rs`)

1. **Origin at acceptance.** Put `origin` and `user_content` into
   `TurnEventChannel` when it is constructed (`state.rs:501-580`). The channel
   emits them in `turn_started`. The call sites that already create a channel
   pass them as follows:

   | Call site | Origin |
   |---|---|
   | `client_processing.rs:286` (Message) | `Client{instance, request_id}` |
   | `client_lifecycle.rs:1231-1261` (idle soft interrupt) | Client |
   | `live_turn.rs:141` | Background / Swarm |
   | `comm_session.rs:637`, `comm_control.rs:432` | Swarm |
   | `jade_relay.rs:1254` | Relay |
   | `client_actions.rs:1269` | Debug |
   | `server.rs:1017` | Background |

   This puts origin at the point upstream already serializes; no extra
   registry is needed.
2. **Active-turn snapshot.** Store origin and content next to the lease in
   `TurnOwnership`, and have `turn_state` read them
   (`client_state.rs:866-871,1185-1189,1256-1264`). Keep the fork rule
   "registered before announced" (`84d61da75`): store before `turn_started` is
   queued, clear before `turn_ended`. Port test `turn_lifecycle_tests.rs:882`.
3. **Single exit with a guaranteed terminal.** Give `TurnEventChannel` a
   finish/Drop guard. It emits `turn_ended{identity, status}` and then the
   scoped terminal `done`/`error` through the same drain task, so the
   publication pin keeps the order. Status values:
   - normal end → Completed
   - `error` → Failed
   - cancel epoch observed (fork technique, `turn_lifecycle.rs:177-189`) → Cancelled
   - dropped without finish (abort) → Cancelled
   - panicking → Failed

   This closes the abort gap at `client_processing.rs:525-533` for every
   attachment. It is the one behavioural fix that upstream also lacks.
4. **Scope cancel's `turn_stopped`/`interrupted`** inside the active
   identity's envelope (`client_processing.rs:497-509,607-619`), so fenced
   readers do not drop them.
5. **`/subagent` launch.** Claim a regular lease, so upstream ownership
   serializes it with model turns. This replaces the lock juggling in
   `4251e5881`. Publish its `turn_started`/`turn_ended` on a requester-only
   channel, as fork Deviation 14 does. Validation stays outside the lease
   (keep test `invalid_subagent_reports_without_waiting_for_the_agent_lock`).
   *Verify first* that holding a lease across the swarm spawn cannot deadlock
   with `comm_session.rs:637`.
6. **Cancel semantics unchanged.** `session/cancel` stays session-wide; Paseo
   relies on this ("Stop in Paseo during a TUI turn cancels it in both"). As an
   optional hardening step, add `Cancel{expected_turn: Option<TurnIdentity>}`.
   It closes the race where ACP's close-time cancel lands on a peer turn that
   began just after its own turn ended.

Deleted after the port, once the mapped tests pass:
`turn_lifecycle_registry.rs`, `server/turn_lifecycle.rs`, the fork's
`TurnStarted{turn_id: String,…}`/`TurnEnded` shapes, and `History.active_turn`.

## 4. ACP adapter changes (`src/cli/acp.rs`, `src/cli/acp/daemon_router.rs`)

- **Classify on the envelope, before unwrapping.** Keep the background reader
  and router. The router consumes `turn_started` / `turn_event` / `turn_ended`
  / `turn_state` with identity, then unwraps for `EventMapper`. Upstream's
  `unwrap_turn_event` (`commands.rs:3342-3349`, used at `acp.rs:295,1530`) must
  run **after** routing, never inside `read_event`.
- **Own vs peer.** Keep the fork rule (`daemon_router.rs:216-224`): origin
  kind is Client, `client_instance_id` equals this connection's
  `acp-{uuid}/{n}`, and `request_id` is a pending Prompt id. Keep:
  - the per-connection instance id (this replaces upstream's hard-coded
    `"acp"`, `acp.rs:850`);
  - upstream's `client_name` fill (`c5e816490`);
  - the random high-half request-id base.
- **Fencing.** Port the TUI's `TurnFence` rules
  (`tui/app/remote/turn_identity.rs:20-83`):
  - retire old generations;
  - reject ids at or below the watermark;
  - accept scoped events only for an open identity.

  Prefer moving the pure fence into `jcode-protocol` and sharing it with the TUI
  and the harness copy (`harness-api-server/src/translate/turn_identity.rs`).
  That is an upstreamable dedup; if it complicates the port, use a local copy.
- **Peer terminal collision.** Becomes simpler: a scoped terminal carries an
  identity, so `peer_terminals` markers keyed by request id
  (`daemon_router.rs:268-297`, `9c03394cc`, `61c606933`) reduce to "terminal
  for an identity that is not ours". Still keep pending-id routing for flat
  `done`/`error` replies to slash commands (Deviation 10).
- **Attach.** Handle `turn_state` (active identity, origin, content), then
  `History`:
  - open the peer turn;
  - suppress the prompt re-announce via `history_has_prompt` (`05e66cb01`);
  - keep the bounded gate that preserves brackets (`42ca58f7f`, `d6e429109`,
    `61c606933`). Upstream queues are unbounded, so the adapter's own cap
    still matters.
- **Unchanged.**
  - Cancel-on-close of its own turn only (`63b3b572b`).
  - Peer `usage_update` (`6bc4aa595`).
  - Close releases the connection (`f12deb2d1`).
  - EOF ends open peer turns as canceled.
  - Upstream's tool-input identity fix and its tests (`ce840273c`,
    `src/cli/acp/tests.rs`); move fork tests into that extracted module.
- **Old daemon.** Without `turn_origin`, use the legacy path (`daemon_router.rs:475-483`).

## 5. Guarantee map (fork → port)

| Fork guarantee / commit | Covered by | Action |
|---|---|---|
| Lifecycle events (`a8125eec5`) | Upstream identity + section 2 extension | **Rewrite** |
| Abort/panic terminal (`a15483479`) | Panic: upstream. Abort: section 3.3 guard | **Adapt** into `TurnEventChannel` |
| Register before announce (`84d61da75`) | Section 3.2 | **Adapt** |
| Restart-unique ids (`68e7a5931`) | UUID generation | **Drop** |
| `/subagent` pair (`b39e02d36`), serialization (`4251e5881`), lint (`e56467c4e`) | Section 3.5 lease | **Adapt** |
| Shared feature list (`c92e51d17`) | Needed for `turn_origin` | **Adapt** |
| Failover lint (`881b1374c`) | Only if the failover tests survive | Re-check |
| Router, mirror, gate, close, usage (`0faa57282`, `e51095273`, `f12deb2d1`, `42ca58f7f`, `6bc4aa595`, `63b3b572b`, `d6e429109`, `9c03394cc`, `61c606933`) | Not upstream | **Adapt** to identity |
| messageIds (`4e2a823fc`, `05e66cb01`), `stored_index` (`f0d73790b`) | Not upstream | **Keep**; the peer scope uses the string `turnId` |
| `session/list` and catalog (`fbec1720a`, `59d4a3ee2`) | Not upstream | **Keep** (reads the store; no daemon turn dependency) |
| `/compact`, `/subagent` ACP commands (`59d948f07`) | Not upstream | **Port** into the extracted files |
| `cargo fmt` (`65cd2126b`) | — | **Drop**; regenerate |
| arm64 CI (`9ad19ac9f`), spec and docs (8 commits) | — | **Keep**; rewrite the spec's Deviations for the new contract and set Status to Implemented |
| e2e and tests (`58a4e34ce`, `70a4f8fce`, `1ce65132f`) | — | **Adapt** assertions to string `turnId` |
| Model/effort/TUI/Azure stack (`09f8cb307`…`d0fae4337`, 19 commits) | Unassessed. Upstream touched catalogs (`709b2a210`, `de9e60eca`, `8e229897e`) and pickers | **Replay** separately; triage each for redundancy |

## 6. Execution stacks

The new branch `port/preview199` is cut from `v1.0.0-preview.199`, in its own
worktree (the report's justification: replaying leaves the checkout unusable).
Do builds and tests on saturn, pluto or ceres after `free -m` (at least 2 GB
free), with named tests only, under `nice`.

| Stack | Content | Gate before next |
|---|---|---|
| S0 | arm64 CI, spec and docs (clean cherry-picks) | Builds |
| S1 | Protocol: section 2 fields, `turn_origin`, `stored_index`; serde round-trip and backward-compat tests (adapt `protocol_tests/core_events.rs`, add to `protocol_tests/turn_identity.rs`) | Protocol tests; upstream TUI/harness fence tests still pass |
| S2 | Daemon sections 3.1–3.4; port `turn_lifecycle_tests.rs` scenarios onto `TurnEventChannel`: two attachments ordered, overlap no-nest with origins, abort → Cancelled for all, panic → Failed, cancel relayed, registered before announced | Those tests, plus upstream `session_manager` and ownership tests |
| S3 | `/subagent` lease (3.5), plus `59d948f07` commands | `client_actions_tests` |
| S4 | ACP router on identity (section 4); move tests into `src/cli/acp/tests.rs` alongside upstream's | Router unit tests (all 22 listed in the contract report) |
| S5 | `session/list` and catalog | Catalog tests |
| S6 | e2e `tests/e2e/acp_shared_session.rs`; isolated-socket runtime probe (`./target/selfdev/jcode … --socket /run/user/1000/jcode-port.sock`), never the shared daemon | e2e passes; manual matrix below |
| S7 | Model/effort/TUI/Azure stack, commit by commit; drop any that upstream made redundant | Their named tests |
| S8 | `cargo fmt`, clippy; CI on the exact head | CI green |

### Behavioural evidence required before replacing `custom`

Concurrent ACP and TUI clients on one session must show:

- a TUI prompt mirrored in ACP;
- an ACP prompt seen as a peer turn in the TUI;
- a queued ACP prompt behind a TUI turn that receives only its own brackets;
- a mid-turn attach with no duplicate prompt;
- stable replay messageIds;
- own cancel versus peer cancel;
- close during a queued prompt not cancelling the peer;
- a forced abort (a tool ignoring cancel for more than 500 ms) ending the turn as `canceled` in **every** client;
- a daemon restart producing a new generation, with the old peer turn ended on EOF;
- failover keeping one `turnId`;
- `session/list` discovery;
- model and effort preserved.

## 7. Risks and open items

- **Fork surface on hot upstream files.** The origin plumbing touches
  `state.rs` and `client_processing.rs`, which upstream changes often. The
  mitigation is to upstream sections 2 and 3.3–3.4 as small PRs (origin on
  `turn_started`, terminal on abort, a scoped cancel `turn_stopped`, a
  capability string). The abort gap is a genuine upstream bug.
- **`peer_user_message` stays upstream-shaped** for the TUI. ACP ignores it
  in favour of `turn_started.user_content`, which has no ordering race.
- **Generation churn.** `runtime_generation` also changes on clear and cold
  resume (`client_session.rs:353-360`). Paseo must treat a new `turnId` prefix
  as a new turn; no state carries over.
- **Gaps in turn ids.** `run_once_capture` debug paths claim ids without
  publishing (`turn_execution.rs:37,79`). The fence must not expect contiguous
  ids.
- **Unbounded daemon queues.** Unchanged upstream behaviour. The adapter's
  continuous reader avoids building a backlog.
- **The model/effort stack (S7) is the largest unknown.** `f245e74d8` touches
  28 files, 21 of them also changed upstream.
- **Replacing `custom`.** This needs a force-push of the fork's integration
  branch, or a new branch name. Owner decision.
