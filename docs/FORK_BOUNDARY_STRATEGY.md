# Fork boundary strategy: handover

Prepared 2026-11-08. This records how the fork should sit against upstream, what
the current carries cost, and the reshaping queue that shrinks that cost.
Nothing in the queue has been executed yet.

The fork is assembled by Desvio from topic branches on `v1.0.0-preview.199`
(`7fd42339c`). See `~/.jcode-fork/README.md` and `manifest.txt` on atlas.

Related docs:

- `docs/UPSTREAM_PREVIEW_199_REBASE_REPORT.md` (on `fork/docs`): the aborted
  rebase and the turn-lifecycle collision.
- `docs/ACP_UPSTREAM_PORT_PLAN.md` (on `fork/docs`): the port decision to use
  upstream's turn identity as the single turn model.
- `docs/ACP_SHARED_SESSIONS.md` (on `fork/docs`, Deviations 48-49 on
  `fork/docs-acp-steering`): the ACP contract.
- Raw audit reports: `~/.jcode-fork/state/footprint-audit-2026-11-08/`
  (`pr-stack.md`, `fork-features.md`, and `brief.md`, the classification brief
  the auditors were given).

## 1. Strategy

**Own ACP outright; touch the core only through thin hooks or refinements
upstream would accept.**

Upstream treats ACP as an afterthought, so a fork-owned ACP is fine. Upstream
also moves fast: about 1,470 commits in the 30 days to 2026-11-08, nearly all
from one author driving agents. It splits files often to stay under a
1,200-line size budget (`scripts/code_size_budget.json`). Every core hunk the
fork carries is a recurring conflict. At the time of the audit, upstream had
changed 94 of the 110 files the fork touched in the previous 30 days.

### Boundary

| Side | Paths |
|---|---|
| **Outside** (fork-owned) | `src/cli/acp.rs`, `src/cli/acp/**`, other ACP glue under `src/cli`, new fork-only files and crates, tests of fork-only code, docs, CI |
| **Inside** (core) | `crates/jcode-protocol`, `crates/jcode-app-core` (`server/`, `agent/`, `tool/`), `crates/jcode-base`, `crates/jcode-provider-*`, `crates/jcode-harness-api-server`, `crates/jcode-tui`, and non-ACP startup code in `src/cli` |

### Rules for new work

1. Put logic outside the boundary first. Core changes should expose data or
   accept a callback, roughly 20 lines or fewer, and be additive.
2. **Extend upstream's turn design; never parallel it.** Upstream's model is
   `TurnIdentity{runtime_generation, turn_id}`, `TurnState`, `TurnStarted`,
   `TurnEvent` envelopes (`crates/jcode-protocol/src/wire/events.rs`), turn
   leases (`agent/turn_ownership.rs`) and `TurnEventChannel`
   (`server/state.rs`). Build ACP semantics (stop reasons, `_jcode/turn`,
   adoption) from it inside `src/cli/acp/`.
3. Add new enum variants and capabilities at the end of the list. `ServerEvent`
   is `#[serde(tag = "type")]` with no unknown-variant fallback, so a peer that
   lacks a variant probably fails to parse it. Gate new events on a capability
   *per event*, not on identity support in general.
4. Touch the TUI only where an interface forces it. Prefer one unambiguous
   interface refinement over fork behaviour in TUI state. The `App` struct has
   415 fields (`crates/jcode-tui/src/tui/app.rs:928-1862`).
5. Avoid the central-list hotspots: `ActiveProvider` and its many match arms in
   `jcode-base/src/provider/mod.rs`, slash-command dispatch
   (`commands_dispatch.rs`), and mid-list insertions into `Config`,
   `ServerEvent` or `Request`. Prefer the open extension points: providers
   defined in config, MCP, hooks, skills, and tools as self-contained files.
6. A `pr/*` branch must stand alone: its own tests and test helpers, with no
   dependency on an earlier `pr/*` branch unless that one is upstream already.
7. **Footprint beats architecture inside the boundary, ratchets
   notwithstanding.** Do not move, split or refactor core code to make room for
   a hook or to tidy a design. A moved upstream function conflicts with every
   later upstream edit to it; a raised fork budget conflicts with nothing. When a
   hook exceeds a size budget, raise that file's entry in `scripts/fork/*.json`
   (fork-owned; upstream's JSON is never edited) by the hook's size, and record
   it in the commit message. Recorded after `fork/acp-elicitation` moved
   `handle_applet_action` and the Desktop tool registration out of upstream files
   to stay under the ceilings (2026-11-09).

### Why the core is hard to carry

This is the background for the rules above, from an architecture review on
2026-11-08.

- The crate graph is a clean DAG, but boundaries follow compile cost, not
  responsibility (`crates/jcode-base/src/lib.rs:1-10`). `pub use jcode_base::*`
  and `pub use jcode_app_core::*` flatten everything into one `crate::`
  namespace. Moved code keeps resolving, but nothing enforces any boundary.
- `Server` (`app-core/src/server.rs:551`) and the TUI `App` are god structs
  with wide shared `Arc<RwLock<…>>` state.
- In favour of carrying: strict CI (fmt, clippy `-D warnings`, budget scripts),
  an offline mock-provider e2e harness (`tests/e2e/mock_provider.rs`), and
  ACP's dependency on `crate::protocol` alone.

## 2. Current footprint

Line counts are added plus deleted, counted per branch against the base and
earlier manifest branches.

| Branch | Outside | Core (non-test) | Core tests |
|---|---:|---:|---:|
| `fork/release`, `fork/docs` | ~1,330 | 0 | 0 |
| `pr/*` stack (seven branches) | ~14 | ~750 | ~1,620 |
| `fork/acp-shared-sessions` | ~5,020 | ~1,540, of which 1,217 is relocation | ~320 |
| `fork/model-effort` | ~380 | ~520 | ~650 |
| `fork/acp-steering` | ~2,150 | 13 | 1 |

About three-quarters of the non-test code is already outside. The queue in
section 4 takes core non-test changes from about 2,800 lines to about 800-900.
Nearly all of what remains is upstreamable.

ACP's `OwnTurn`, `PeerTurn` and `TurnFence` (`bd415a417`, `d2f3a1501`) are
client-side tracking. They consume upstream identities and events and do not
duplicate lease authority. ACP's extension uses `_jcode/turn`, so there is no
wire-name collision. The one competing piece is `TurnEnded` (item R1 below).

## 3. Classification by branch

The classes are:

- **HOOK**: thin; keep.
- **COULD**: could be a hook, so reshape.
- **INSIDE**: genuinely belongs in the core, so upstream it.
- **TEST**: test-only.

Hashes are topic-branch commits.

### `pr/turn-abort-terminal` (`0e525f6dc`)

- INSIDE: `server/state.rs` plus seven channel-construction sites, +82/−28.
  A terminal is guaranteed after a producer abort. This extends upstream's
  publication lease and is a strong candidate.
- Nit: it tracks whether any terminal appeared, but does not enforce the
  "exactly one" its docs claim.
- TEST: +381.

### `pr/cancel-turn-stopped-scope` (`820a4b8f7`)

- INSIDE: `client_processing.rs`, +8/−2. Wraps cancellation's `TurnStopped` in
  the turn envelope. A strong candidate.
- TEST: +93. It borrows test helpers from the previous branch; move them in to
  make it standalone.

### `pr/turn-scoped-cancel` (`5888016f5`)

- INSIDE: `Cancel{turn: Option<TurnIdentity>}` across the protocol and server,
  +74/−29, plus a TUI `backend.rs` adaptation of +2/−1.
- **Defect D1 (verified):** `cancel_targets_active_turn` is checked, then
  `cancel_processing_message(...).await` signals the whole session. A successor
  turn that starts in between is cancelled.
- TEST: +128. These tests exercise the predicate, not the dispatch race.

### `pr/turn-origin` (`cab3e316f`)

- INSIDE (about 160 lines): origin and prompt metadata attached to upstream
  identity, atomic active-identity snapshots (`turn_ownership.rs`,
  `client_state.rs`, `session_connections.rs`). Plausible upstream, but the
  actor categories need the owner's agreement.
- HOOK: origin and prompt plumbing through entry points, +48/−2.
- **COULD (about 236 lines):** `TurnEnded`/`TurnEndStatus`, the `ended`
  bookkeeping, cancellation probes and early retirement (`agent.rs`,
  `server/state.rs`, wire definitions). These compete with upstream's scoped
  terminals and lease release. See R1.
- **Defect D3 (verified):** `event_for_transport`
  (`server/client_lifecycle.rs:85`) passes every event through to
  identity-aware clients, so an upstream identity-aware client receives an
  unknown `turn_ended`. This only matters with mixed versions, and it goes away
  with R1.
- TEST: +789. About 400-500 of these lines test the duplicate lifecycle.

### `pr/history-stored-index` (`f79d8a369`)

- INSIDE plus HOOK: an optional `HistoryMessage.stored_index`, +12 core lines.
  A strong standalone candidate.
- TEST: +95.

### `pr/stale-cancel-flag` (`91d07ad02`)

- INSIDE: `discard_cancel_armed_before_turn` (`agent/interrupts.rs`) and its
  call site in `start_processing_message`, +22.
- **Defect D2 (verified):** it reads the epoch *after* the claim, so an
  unscoped cancel that fires between the claim and the read is erased. Capture
  the epoch at the claim, or have the claim return it.
- TEST: +137.

### `pr/idle-steer-announce` (`49a837d93`, not in the manifest)

- INSIDE: `client_lifecycle.rs`, +13. Announces prompts to peers that use the
  older protocol without turn identity. Plausible upstream, but it needs a
  regression test.
- It is patch-identical to `cdf0826f5` on `fork/acp-steering`. Carry it once.

### `fork/acp-shared-sessions` (`3f61fe38b..81bd88757`)

- INSIDE: a stable stored-message identity (+11). RunSubagent repair and
  serialisation on the existing leases (`client_actions.rs`,
  `client_lifecycle.rs`, +145/−111). This extends upstream's turns.
- **COULD:** the session catalog extraction in `harness-api-server`
  (`86014e1a7`, +642/−575). See R2.
- **COULD:** spawn translation and `--continue` rejection, plus peer-audience
  selection in `client_actions.rs` (`60efd7433`, about 52 lines). See R5.
- HOOK: `stored_index: None` in TUI `server_events.rs`.
- Outside: 7 files, +4,749/−273.

### `fork/model-effort` (`5b080b743..2196d12ea`)

- INSIDE: effort capabilities and session-correct history fallbacks (+70/−8),
  Azure routing (+22), and stopping `gpt-oss` names from implying effort
  support (+1/−1).
- **INSIDE but contentious:** the model-resolution change (`5b080b743`,
  `dbae01234`, +30/−3). Unknown bare names now fail instead of reaching the
  active provider. Expect pushback, or put it behind a setting.
- HOOK: four capability-exposure sites (+33), and provider re-exports plus an
  accessor (+12/−6).
- **COULD:**
  - Startup selection, acknowledgement and timeout state in the TUI (+95/−4)
    and `src/cli` (+74/−21). See R3.
  - Preview and direct-command rules in the TUI picker (+65/−2). See R4.
  - Nearest-effort rounding in `provider-core/reasoning.rs` (+19). See R6.
- Outside: 5 files, +329/−48.

### `fork/acp-steering`

- INSIDE: `cdf0826f5`, the same change as `pr/idle-steer-announce`.
- Outside: 5 files, +1,983/−167.

## 4. Work queue

The queue is in priority order. Each item lands on its own topic branch. Rerun
`jcode-desvio build --bisect-gate` after each item, and publish per
`~/.jcode-fork/README.md`.

| ID | Change | Core lines saved | Acceptance |
|---|---|---:|---|
| **R1** | Drop `TurnEnded`/`TurnEndStatus` and the `ended` bookkeeping from `pr/turn-origin`. Derive the end status in `src/cli/acp/` from upstream's scoped terminals, which `pr/turn-abort-terminal` guarantees after a producer abort. | ~300, plus ~400-500 test lines | `_jcode/turn` and the ACP `stopReason` output are byte-identical (ACP and e2e tests pass unchanged). No `turn_ended` on the wire. D3 is gone. |
| **R2** | Replace the catalog relocation with a small `stored_session_catalog(limit) -> Vec<Entry>` facade in `harness-api-server`. Alternatively, upstream the extraction as a size-budget refactor. | ~1,150, mostly moved code | `session/list` behaviour unchanged. Upstream's `translate.rs` is left untouched, or the extraction is merged upstream. |
| **D1-D3** | Fix the verified defects on their `pr/*` branches before proposing them upstream. | 0 | D1: identity check and signal are atomic, with a race regression test. D2: epoch captured at the claim. D3: by R1. |
| **R3** | Move the model startup selection controller out of the TUI and `src/cli` into a fork-owned module. It consumes `History`, `ModelChanged` and `Error`, and gets send, hold and restore callbacks. | ~250-350 | CLI startup model selection behaves the same. TUI change is limited to the callbacks. |
| **R4** | Reduce the picker preview and direct-command rules to a single predicate hook. If the TUI picker behaviour is not needed, drop it instead. | ~150-190 | The TUI diff is the hook only. |
| **R5** | Subagent spawn and audience policy: core accepts a tool spec; a fork module builds `{name, input}` and chooses the audience. | ~65-80 | `/subagent` ACP tests pass. Leases and execution stay in core. |
| **R6** | Move nearest-effort rounding to `acp/session_configuration.rs`. | ~40 | ACP effort tests pass. Nothing is added to `provider-core` besides the capability ladder. |
| **U** | Propose upstream in this order: `turn-abort-terminal`, `cancel-turn-stopped-scope`, `history-stored-index`, `stale-cancel-flag` (after D2), `turn-scoped-cancel` (after D1), `idle-steer-announce` (with a test), then the slimmed `turn-origin`. | All, on landing | Delete each `pr/*` manifest line once it lands. Drafts live in `~/.jcode-fork/state/upstream-drafts/`. |

Fallback: if upstream's turn design drifts somewhere the fork cannot follow,
freeze the base and cherry-pick from upstream selectively rather than chase it.
The rules above keep that option cheap.

## 5. Provenance and confidence

- Footprint and classification come from two Codex `gpt-6.1-sol` (medium)
  auditors, run read-only on the topic branches. Their method was
  `git log --no-merges B --not v1.0.0-preview.199 <earlier branches>`, then
  per-commit diffs. Their raw reports are under `state/footprint-audit-2026-11-08/`.
- D1, D2 and D3 were verified directly against the diffs (`5888016f5`,
  `91d07ad02`, and `client_lifecycle.rs:85` on `custom`). The other line counts
  and classes are the auditors'. Re-check a row before acting on it.
- The architecture findings in section 1 come from read-only reviews on
  2026-11-08. The `App` field count, the absence of a serde unknown-variant
  fallback, the size budget and the glob re-exports were checked directly.
- Not done: no cargo build or tests were run for this audit, and the queue
  items' line savings are estimates.
