# ACP fork implementation record

Living task list and record for executing [the strategy](ACP_FORK_STRATEGY.md)
against [the interface contract](ACP_FORK_INTERFACE.md). It is the
orchestrator's durable state: if context is lost, resume from here. Update it
in the same push as any lane state change. Status claims cite a commit, a CI
run or a named test; "done" without evidence is not done.

Orchestrator branch: `orchestrate-acp-fork-plan` (worktree
`~/.paseo/worktrees/0wvdg91s/naughty-chicken` on atlas), based on `custom`
at `a047a0c07`.

## Delegation policy

- **Sol** = Paseo `codex/gpt-6.1-sol`, thinking `medium`: critical work, i.e.
  daemon concurrency/cancellation, lifecycle (R1), protocol test
  infrastructure and anything the consumer gates on.
- **DeepSeek** = Paseo `pi/opencode-go/deepseek-v4.1-flash`, thinking `high`
  (the `dsh` route failed with "Insufficient Balance" on 2026-11-08): additive,
  mechanical or well-specified work such as the CI workflow, metadata fields,
  docs and fixture plumbing that already has a design.
- Every lane gets its own Paseo worktree on a topic branch off `origin/custom`
  and pushes that branch. The orchestrator reviews, then integrates into
  `custom` (merge, no rebase of merged branches); D1/D2 fixes go on their
  `pr/*` branch as new commits (merge-forward).
- Labels: `cq.role` = `fixer`/`researcher`/`critic`, `cq.task` = lane id.
- Builds: `nice`, `-j 2`, named tests only, `systemd-run --user --scope -p
  MemoryMax=…` where available; check `free -m` first. Runtime checks use
  `./target/selfdev/jcode … --socket /run/user/$UID/jcode-<lane>.sock`.

## Test quality bar (core protocol)

Every lane that changes wire behavior adds tests at the lowest layer that can
observe it, plus a wire-level test:

1. Adapter JSON-shape tests (`src/cli/acp/tests.rs`) for every field the
   contract names, including absence/`null` cases and error codes
   (`-32601` unknown method, `-32602` bad params).
2. Router/lifecycle tests (`src/cli/acp/daemon_router_tests.rs`) for ordering:
   start precedes output precedes exactly one terminal.
3. Wire-level conformance (`tests/e2e/acp_*.rs`) driving `jcode acp` over
   stdio against the mock provider on a private socket, asserting framed
   JSON-RPC, not internal structs.
4. Races use a deterministic seam (test-only yield/barrier), never sleeps.
5. Fixture corpus (`tests/fixtures/acp/*.jsonl`) replays in CI and fails on
   drift (opaque IDs matched structurally).

## Task list

| Id | Phase | Lane / branch | Worker | Status | Evidence |
| --- | --- | --- | --- | --- | --- |
| P0-docs | 0 | planning docs on `custom` | n/a | done | `docs/ACP_FORK_*`, `docs/acp-pins/` present at `a047a0c07` |
| P0-ci | 0 | `fork/ci-baseline`: fork-owned workflow (quality + ubuntu build/e2e) on `custom` and `fork/**` | DeepSeek `9a617f29` (atlas, `acp-p0-ci`) | Quality green; Build&Test red on stdin-forwarding (fork-caused, upstream passes) — diagnosing | revert `ee4f9f6a7` (reformatted 6 upstream-only files); port `fmt-check.sh` into `scripts/fork/`; keep `a47408ec1` |
| P0-budget | 0 | same branch: fork budget baseline files + script flag | DeepSeek `9a617f29` | running | |
| P0-record | 0 | first green run on `custom` recorded here | orchestrator | blocked on P0-ci | |
| P1-meta | 1 | `fork/acp-capability-metadata`: `agentCapabilities._meta.jcode` (interfaceVersion, profile, extensions, sessionMcpServers) in every profile; `result._meta.jcode` {clientInstanceId, sharedControl} on new/load/resume; detach=false | DeepSeek `5365c86a` (atlas, `acp-p1-meta`) | accepted, awaiting integration | `d335f2e7d`: 5 unit + 1 e2e wire test pass; sharedControl derived from daemon Pong caps (`SharedControlSupport::from_router`) |
| P1-race | 1 | D1/D2 atomic turn-scoped cancel + epoch at turn claim, deterministic seam, terminal publication audit, mixed-version delivery (D3 guard); on `pr/turn-scoped-cancel` / `pr/stale-cancel-flag` merge-forward; topic `fork/p1-cancel-race` | Sol `9008bf12` (atlas, `acp-p1-race`) | accepted, awaiting integration | `26c296a09` (review fixes: Interrupted queued inside locked revalidation, failing-first test `db810004f`); D1 `26ff594f2` on pr/turn-scoped-cancel, D2 `86db359cf` on pr/stale-cancel-flag; evidence `docs/verification/p1-cancel-race.md` |
| T-conf | 1→3 | `fork/acp-conformance`: wire-level protocol conformance suite + JSONL recorder/replayer for `tests/fixtures/acp/` | Sol `652f5f66` (triton, `acp-conformance`) | accepted, awaiting integration (after P0-ci green) | `4759e51b3`: 5 pass + 2 ignored contract gaps; 5/5 stable runs on triton |
| P2-R1 | 2 | remove competing lifecycle; split `daemon_router.rs`; branch `fork/r1-single-lifecycle` off stage `47a130e98`; gate = 4 fixtures replay unmodified | Sol `7be2f4d6` (triton, `acp-r1-lifecycle`) | accepted; stage fast-forwarded to `38fa8f3ee` | `13eccbd12`: `TurnEnded` removed, scoped terminal `end`/`end_message`, adapter-owned local ends; fixtures unmodified; core diff 4476/296 -> 4457/304 (simpler, not much smaller) |
| P2-R2 | 2 | catalog facade in `jcode-harness-api-server`; branch `fork/r2-catalog-facade` off `fork/acp-conformance` | DeepSeek `1334bb53` (triton, `acp-r2-catalog`) | accepted, awaiting integration | `8a9e9e499`: catalog-path diff vs base 1,242 -> 113 lines (`translate.rs` 5/1, `session_files.rs` 0); fixture 01 replays unmodified; 199 harness + 92 acp lib + 5 conformance pass |
| P3-corr | 3 | turn correlation `_meta.jcode.turnId` + `role`; branch `fork/acp-turn-correlation-detach` off `fork/acp-stage` | Sol `05e7eb9f` (triton, `acp-p3-corr-detach`) | accepted; in stage | `104604e23`: 100 ACP unit/router + 2 shared-session + 12 conformance pass; new fixtures 03, 04; 5x stable |
| P3-detach | 3 | `_jcode/detach`; also v1 observer close must end an open peer bracket with `canceled` (T-conf gap `acp_conformance_observer_close_bracket_contract`); same lane as P3-corr | Sol `05e7eb9f` | accepted; in stage | core hook: `PrepareDisconnect.preserve_work` + capability `detach_preserve_work` (additive) |
| P3-skill | 3 | `skill: null` clear + `skill_selection` ext; branch `fork/acp-skill-clear-mcp-notice` off `fork/acp-capability-metadata` | DeepSeek `8e023355` (triton, `acp-p3-skill-mcp`) | accepted, awaiting integration | `5ca728de0`; 57 `cli::acp::tests` + 3 e2e wire tests pass |
| P3-mcp | 3 | ignored `mcpServers` notice (same branch/agent as P3-skill) | DeepSeek `8e023355` | accepted, awaiting integration | `5ca728de0`: notice only when client advertised `session.notices`; contract bullet amended on the branch |
| P3-map | 3 | reasoning/plans/tool metadata mappings; branch `fork/acp-v1-mappings` off `fork/acp-stage`; audit in `docs/verification/p3-mappings.md` | DeepSeek `436bb2af` (atlas, `acp-p3-map`) | accepted; merged into stage `b1d801910`, compile verification assigned to `436bb2af` | `d94ec61af`: reasoning -> `agent_thought_chunk`; todo -> session `plan` via `GetTodos` snapshot; tool `name`, `locations` (read/edit/write), `diff` on edit/write; apply_patch paths/diffs and plan-on-load skipped (no structured data) |
| P3-corpus | 3 | record scenarios 1–5, 7, 8 (v1) into corpus; CI replay | Sol | partial: 01 create/list/load, 02 shared observe/steer, 03 stop-then-prompt, 04 detach recorded; remaining: 05 close owner-aware, 07 config/commands, 08 negotiation/legacy | |
| P0-split | 0/4 | pure-move split of `src/cli/acp.rs` (4182 > ceiling 3444) + `tests.rs` (2275 > 1380) + `daemon_router_tests.rs` (1815 > 1644); branch `fork/acp-module-split` off stage `38fa8f3ee` | Sol `b4d57c2d` (triton, `acp-module-split`) | running; blocks integration into custom | |

Phases 4–5 (v2 track) are gated per the strategy and are not scheduled.

## Log

- 2026-11-08: Record created. Preconditions re-checked: HEAD = `custom` =
  `origin/custom` = `a047a0c07`; docs and pins present on `custom`; no fork CI
  workflow exists (`.github/workflows/` has only upstream plus
  `fork-release.yml`). Decision: start P0-ci, P1-meta, P1-race and T-conf
  concurrently on separate topic branches. Phase 1 lanes may develop before
  P0 is green but are not integrated into `custom` until the fork workflow
  runs on their head (strategy: until then claims are named local tests).
- 2026-11-08: Wave 1 dispatched (P0-ci, P1-meta, P1-race, T-conf). Agent ids
  above are Paseo agent ids (prefix). T-conf writes to the contract and marks
  contradictions `#[ignore = "contract gap: …"]` rather than changing code.
  Integration order once reviewed: P0-ci → P1-race → P1-meta → T-conf
  (re-record corpus after P1-meta, since metadata changes the wire).
- 2026-11-08: T-conf finished (`d26d278a4`, `4df380b88`). Review: comparator
  normalises opaque keys with a shared first-seen map (catches splits,
  reorder, content drift); shared scenario gated on debug-socket queue
  admission, no sleeps. Gaps found and pinned as ignored tests:
  (a) capability metadata absent -> closes with P1-meta, un-ignore then;
  (b) v1 observer `session/close` emits no `canceled` terminal for the open
  peer bracket -> assigned to P3-detach (shared settle path). Integration
  note: P1-meta changes the initialize wire, so re-record
  `01-create-list-load-v1.jsonl` in the P1-meta integration commit.
  Side effect: the worker installed `pkg-config libssl-dev` on triton via
  `sudo apt-get` to build.
- 2026-11-08: P1-meta finished and accepted (`8ff748be7`..`d335f2e7d`).
  Atlas OOM-killed one link at -j2 (another cargo job running);
  `systemd-run --user` is unavailable on atlas (no session bus), so prefer
  triton (24 GB) for new lanes. Expected conflict: P1-meta and T-conf both
  edit `tests/e2e/acp_shared_session.rs` (T-conf moved helpers to
  `test_support/acp.rs`). P0-ci has a Fork CI run in progress
  (run 42453543273) and committed a code fix to `cloud_managed_tests`;
  review that it is a genuine fix, not test removal.
  Wave 2 dispatched on triton: P2-R2 and P3-skill+mcp (DeepSeek). Holding
  P3-corr and P3-detach (Sol) until P1-race lands, since both touch turn
  identity and prompt settlement.
- 2026-11-08: P1-race finished at `69d85b2a4`. D1: identity revalidation +
  signal under the ownership state lock (`TurnObserver::cancel_active`); idle
  snapshot never arms. D2: discard + epoch capture inside `claim_after`
  before identity exposure. Terminal audit fixed duplicate producer terminals
  and duplicate requester `Done` (drain is sole `Done` publisher). D3 left for
  R1 (no per-variant capability). Extra: SDK callback ownership released on
  resume. Review requested: (1) `Interrupted` is sent before revalidation, so
  a turn that completes naturally in the gap is mislabelled as stopped; fix +
  failing-first test; (2) rustfmt violations; (3) Drop-side-effect idiom and
  thread-local seam note.
- 2026-11-08: P1-race accepted at `26c296a09`. P0-ci review: `ee4f9f6a7`
  reformatted six files with zero fork diff (would become permanent
  carries); asked for a revert and a scoped fmt check in-repo
  (`scripts/fork/fmt-check.sh`, base in a workflow env). `a47408ec1`
  (`cloud_managed_tests` Path import, clippy failure at base) kept as an
  upstreamable one-line carry.
- 2026-11-08: Created `fork/acp-stage` = custom + p1-cancel-race +
  acp-capability-metadata + acp-conformance (`2830da176`, textual merge
  clean, uncompiled). It is the candidate integration for `custom` once P0
  is green. P3-corr+detach (Sol, triton) branches from it; its step 0
  reconciles the stage (compile, un-ignore metadata contract test,
  re-record fixture 01).
- 2026-11-08: P2-R2 finished: facade
  `stored_session_catalog(limit) -> Vec<SessionCatalogEntry>`;
  `session_files.rs` and `translate_tests.rs` back to upstream (bar one
  `stored_index` line). Residual: ~30 lines of list assembly duplicated with
  upstream's `list_sessions` handler (drift risk if upstream changes title
  resolution; covered by facade tests + conformance list test). Asked to move
  the facade under `translate/` so private items are visible without the 27
  visibility markers.
- 2026-11-08: P2-R2 accepted at `8a9e9e499` (facade moved to
  `translate/session_catalog.rs`, re-exported from `lib.rs`; all visibility
  markers reverted; verified `translate.rs` diff vs base is only the
  `turn_ended` attribution lines + `mod` line). Note for R1: those
  `turn_ended` lines go away when R1 removes the variant.
- 2026-11-08: P3-map dispatched (DeepSeek, atlas). R1 deliberately held behind P3-corr-detach.
- 2026-11-08: P3-skill+mcp finished (`1c91a40e4`). Skill clear accepted.
  The ignored-`mcpServers` report uses `sessionUpdate: "notice"`, which ACP
  v1 defines (`$defs.Notice`) but only allows when the client advertised
  `clientCapabilities.session.notices`; the code sent it unconditionally.
  Asked: gate on the capability, send nothing otherwise (no
  agent_message_chunk fallback), amend the contract bullet to match.
  Contract note: busy explicit skill selection uses the existing busy
  rejection `-32000`, not `-32602`; the contract wording only says "reject
  before insertion", so no amendment is needed.
- 2026-11-08: P3-skill+mcp accepted at `5ca728de0` (verified gate reads `clientCapabilities.session.notices` as an object marker; contract amended in-branch).
- 2026-11-08: Decision (contract ambiguity raised by P3-corr worker): busy
  `session/prompt` rejections keep the legacy JSON-RPC error; turn
  correlation applies to every *successful* prompt result; `role: none` =
  successful result with no turn (selection-only command, bare skill, skill
  clear). Contract amended in the P3-corr branch. Rationale: the contract
  also requires preserving existing v1 error behavior, and the skill lane
  already uses the `-32000` busy error. Note: `respond_to_permission` cannot
  reach agents on other hosts (no serverId); answer remote questions via
  `send_agent_prompt` steer.
  Remote permission/question requests can also be answered on the agent's
  host: `ssh <host> paseo permit allow <agentId> <reqId> --input '{"answers":{"0":"..."}}'`
  (answer values are strings).
- 2026-11-08: P3-corr+detach accepted at `104604e23`. Contract amended
  in-branch: correlation applies to successful results; `role: none` has
  `turnId: null`; busy keeps the JSON-RPC error.
- 2026-11-08: Stage rebuilt: `fork/acp-stage` = previous stage + corr-detach
  (ff) + skill-mcp (merge `557b5d9d1`, conflicts resolved by orchestrator:
  extension list in contract order; obsolete "not advertised yet" loop
  replaced by a positive assertion; rerere recorded) + R2 (`c2342437a`,
  clean). Uncompiled. Known mismatch: skill selection-only results lack
  `_meta.jcode` role none. Reconciliation assigned to `05e7eb9f`
  (pushes ff-only to `fork/acp-stage`). R1 starts after that, from the
  reconciled stage.
- 2026-11-08: Side request (relayed "owner request" from Paseo agent
  `1f8a9aff` on triton): use the cq-toolkit queued review (`cq
  review.submit`, ceres server e61fc8c9, review-v1) on one committed lane.
  Chosen: `fork/acp-turn-correlation-detach` @ `104604e23`, target `custom`,
  `reachable=true`, requestId `jcode-acp-p3-corr-detach-104604e23`. NOT
  submitted. Blockers: ceres queue does not grant camerontaylor/jcode or
  `custom`; no review-v1 client on atlas (checkout at 11d2204f, no `cq` on
  PATH); no `CQ_QUEUE_SSH_ALIAS`. No credentials or server policy touched.
  Waiting on `1f8a9aff` to grant/configure; independent acceptance + fork
  CI remain the gate regardless.
- 2026-11-08: Queued review SUBMITTED (owner instruction confirmed by
  `1f8a9aff`: "Have the jcode agent on atlas use the tool"). Request
  `jcode-acp-p3-corr-detach-104604e23`, item `lr-c6d9121449db-g1`,
  reviewedHead `104604e23972bfb0aad9c2c301c81066fa182266`, target `custom`,
  reachable=true, no notify. Run from triton: client
  `node ~/.cache/cq-client-e61fc8c9/dist/cli.js` (isolated e61fc8c97 build),
  cwd `~/.cache/jcode-review-104604e23`, env
  `PATH=/home/ctaylor/.local/state/cq-jcode-review-20261108/bin:$PATH
  CQ_QUEUE_SSH_ALIAS=cq-queue-ceres`, unset `PASEO_AGENT_ID`
  `CQ_REVIEW_NOTIFY_HOST`. Created local branch `custom` tracking
  `origin/custom` in triton's `~/repos/jcode` (client resolves target as a
  local ref). Polling status every 60 s from atlas (background task).
  Findings: route actionable ones to a lane fix, then report to `1f8a9aff`.
- 2026-11-08: Stage reconciled by `05e7eb9f` at `47a130e98` (ff-only push).
  Orchestrator verified fixture drift under comparator normalisation: the
  only change in all four files is `initialize` extensions gaining
  `skill_selection`. Results: 58 ACP unit, 41 router, 12 conformance,
  4 shared-session, 199 harness-api-server.
- 2026-11-08: Fork CI: first run 42453543273 failed in Build & Test step
  "Run stdin-forwarding cohort (all platforms)"; Quality green. Second run
  42455789393 (scoped fmt check) in progress; P0 worker owns diagnosis.
- 2026-11-08: R1 dispatched (Sol, triton) from stage `47a130e98`.
- 2026-11-08 (session restart): background review poller was lost and
  restarted; review `jcode-acp-p3-corr-detach-104604e23` state `running`.
- 2026-11-08: Fork CI run 2 (42455789393) failed again in Build & Test,
  `tool::bash::tests::test_stdin_forwarding_{single_line,multiple_lines}`
  ("timed out waiting for stdin request"). NOT pre-existing upstream:
  upstream CI on base 7fd42339c (1jehuang/jcode run 42225693659) passed
  that step; it failed only Quality/Format. Steered P0 worker to diff the
  workflow steps against upstream and reproduce locally; no skip/ignore.
- 2026-11-08: R1 worker stopped early with prep only: inventory
  (`6ca19f953`), pure-move router split (`36d16910e`, `daemon_router.rs`
  -> 1,270 lines), notes (`ed7c5521b`). Core diff unchanged. Sent design
  decision and asked it to continue: optional `end: Option<TurnEndStatus>`
  on the scoped terminal set by `TurnEventDrain`; adapter-owned local end
  type; delete `ServerEvent::TurnEnded` (removes D3) and translate.rs lines;
  rewrite router test helper; full equivalence gate; checkpoint if context
  is tight.
- 2026-11-08: Queued review result: `jcode-acp-p3-corr-detach-104604e23`
  completed, outcome `findings`, receipt `f61b12cc…`, report at triton
  `~/.cache/jcode-review-104604e23/.cq/queue-evidence/results/f61b12ccde951a3e58ce141c016d263a2f64637b343a1af7a0654d2823474fad/report.md`.
  Verified against stage `47a130e98`, all valid:
  F1 major: `cancel_processing_message_for_turn` drops the taken task handle
  on `signal_target -> None` early return (was introduced by P1-race review
  fix). Orchestrator found sibling: `TurnStopped` still published before
  revalidation (same misclassification class as `Interrupted`). -> P1-race
  worker `9008bf12`, branch `fork/p1-cancel-race`.
  F3 major: `_jcode/detach` propagates PrepareDisconnect send/wait errors
  out of handle_message; loop ignores Error reply. F4 minor: legacy
  unattributed turns report role none (proposed: omit correlation; contract
  sentence). F2 minor: normalise `agentInfo.version` in fixture comparator.
  -> corr-detach worker `05e7eb9f`, branch `fork/acp-turn-correlation-detach`
  (not pushing stage; orchestrator merges both).
- 2026-11-08: Review F2/F3/F4 fixed by `05e7eb9f` (`6f181c53f`,
  `8841ffa93`): detach prepare failure -> `-32603`, waiter unregistered,
  attachment preserved (wire regression); unattributed work omits
  correlation (contract sentence added); only `result.agentInfo.version`
  normalised, no re-record. 59 ACP unit, 41 router, 14 conformance,
  4 shared-session, 199 harness lib. Orchestrator spot-checked F3 and
  fast-forwarded `fork/acp-stage` to `8841ffa93`. F1 (core) pending.
- 2026-11-08: P3-map accepted (`d94ec61af`). Verified `name` is in the
  ACP v1 ToolCall schema. Merged into stage (`b1d801910`): union-resolved
  `daemon_router_tests.rs` (45 tests = 37 common + 4 corr/detach + 4
  mappings; git dropped a `}`/`#[test]` at the seam, fixed; rerere
  recorded). acp.rs auto-merged, uncompiled -> `436bb2af` verifies and
  pushes ff-only.
- 2026-11-08: Review F1 + TurnStopped sibling fixed by `9008bf12`
  (`fork/p1-cancel-race` b5384ff29, `pr/turn-scoped-cancel` e25ef4005):
  fanout targets snapshotted before the lock (`session_event_targets`),
  TurnStopped/Interrupted sent synchronously inside `cancel_active` after
  revalidation, task handle restored on rejection; failing-first run
  reproduced all three. Accepted edge: a subscriber joining between snapshot
  and send misses that TurnStopped (live_status still observed). Merged
  into stage cleanly: `b034aad96`. All four review findings are now fixed
  in the stage.
- 2026-11-08: Stage verified by `436bb2af` at `7e4acbee2` (= b034aad96 +
  one-line `tests/e2e/main.rs` mod-order format fix). Core: 58 lifecycle,
  14 turn-event, 44 session, 3 ownership + named race regressions; ACP: 71
  unit, 45 router, 15 conformance (4 fixtures replay, no re-record needed:
  fixtures use text-only mock scripts), 4 shared-session; fmt-check exit 0.
- 2026-11-08: P0 stdin-forwarding root cause (`a7a3c29d0`): fork runner got
  ubuntu-26.04 first; newer kernels report `anon_pipe_read` in
  /proc/PID/wchan, missing from `stdin_detect.rs`'s fallback list, so
  `sh -> reader` chains never trigger forwarding. Reproduced on atlas
  (kernel 7.2.12): times out without the line, passes with it. One-line
  upstream-file carry; strong upstream candidate (real user impact on new
  kernels). Fork CI run 3 (42458512159) in progress: Quality green.
  Upstream candidates so far: `a7a3c29d0` (stdin wchan), `a47408ec1`
  (cloud_managed_tests import), D1 `pr/turn-scoped-cancel`, D2
  `pr/stale-cancel-flag`.
- 2026-11-08: R1 finished at `13eccbd12`: single lifecycle; D3 gone;
  `TurnEvent` gains optional `end: Option<TurnEndStatus>` + `end_message`;
  epoch probes/snapshot retirement/publication pins kept; local bracket ends
  are adapter FIFO deliveries. All 4 fixtures replay unmodified; new wire
  tests: abort after Stop -> cancelled/canceled, panic -> failed. Core diff
  barely shrinks (the new fields + tests offset the deleted lifecycle);
  value is one lifecycle authority, not line count (strategy: correctness
  over the 800–900 line estimate). Asked to merge stage `7e4acbee2` (router
  split vs mappings changes), rerun the full equivalence set, preserve the
  cancellation fix's ordering, and state mixed-version `turn_ended` decode.
  Plan: submit the merged R1 head to the queued review.
- 2026-11-08: R1 merged the stage (`38fa8f3ee`): 17 conformance (4
  fixtures unmodified), 4 shared-session, 71 ACP, 45 router, 58 lifecycle,
  14 channel, 3/44/4 ownership/session/SDK, 199 harness, 10 protocol.
  Cancellation notices stay synchronous under `cancel_active`'s lock; drain
  takes the same lock before terminal publication (explicit ordering
  assertions). Old-daemon `turn_ended` line is skipped without failing the
  connection; following scoped terminal still processed. `fork/acp-stage`
  fast-forwarded to `38fa8f3ee` = every accepted lane.
- 2026-11-08: Queued review #2 submitted: `jcode-acp-r1-stage-38fa8f3ee`,
  item `lr-b26eef738617-g1`, reviewedHead `38fa8f3ee6f4…`, target custom
  (full stage diff vs custom), cwd triton `~/.cache/jcode-review-38fa8f3ee`.
  Polling in background.
- 2026-11-08: Budget check: the stage exceeds fork ceilings (`acp.rs` 4182 vs
  3444, `tests.rs` 2275 vs 1380, `daemon_router_tests.rs` 1815 vs 1644).
  Strategy forbids ratcheting these upward, so P0-split moves onto the
  critical path before integration. Dispatched to Sol: pure moves, per-commit
  move verification, identical test names, full equivalence gate, budgets
  checked against `origin/fork/ci-baseline` JSON. Review #2 findings (if any)
  will be applied on top of the split.
- 2026-11-08: Queued review #2 `jcode-acp-r1-stage-38fa8f3ee` FAILED before
  upload: server secret scan, 2 denylist hits (rule
  `class:token-ssh-plumbing`), 0 gitleaks. Both are the upstream doc comment
  "`../../.ssh/id_rsa`" (path-traversal example) in
  `jcode-harness-api-server` session-id validation, appearing as - (deleted
  relocation `session_catalog.rs`) and + (restored upstream `translate.rs`).
  False positive; not bypassed; comment not edited (would re-carry an
  upstream file R2 restored). Allowlist decision handed to `1f8a9aff`
  (server policy owner); resubmit with `--supersedes` when resolved.
  Integration proceeds on independent acceptance + fork CI.
- 2026-11-08: `1f8a9aff` acknowledged the false positive and is investigating
  narrow scanner exceptions. Instruction: leave the source comment
  unchanged, do NOT resubmit review #2 until it reports a fix.
