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
| P0-ci | 0 | `fork/ci-baseline`: fork-owned workflow (quality + ubuntu build/e2e) on `custom` and `fork/**` | DeepSeek `9a617f29` (atlas, `acp-p0-ci`) | done (green) | run 42458512159 @ `a7a3c29d0` | revert `ee4f9f6a7` (reformatted 6 upstream-only files); port `fmt-check.sh` into `scripts/fork/`; keep `a47408ec1` |
| P0-budget | 0 | same branch: fork budget baseline files + `JCODE_*_BUDGET` env overrides | DeepSeek `9a617f29` | done | ceilings = upstream's, current size only where failing; ratchet teeth verified |
| P0-record | 0 | first green run on `custom` recorded here | orchestrator | done: `custom` fast-forwarded `a047a0c07` -> `bf9f0b22f` (2026-11-08) on green run 42465256663 (exact SHA); confirmation run on `custom`: 42467221310 green | user authorized merges |
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
| P3-corpus | 3 | record scenarios 05 close owner-aware, 07 config/commands, 08 negotiation/legacy; branch `fork/acp-corpus-v1` off stage `76045f863` | Sol `e66fe5f0` (triton, `acp-corpus-v1`) | accepted (05/07/08 recorded; `25604eeaa`, `5dcce24f8`, `adfe0f4c7`); gap fixed `9bf7aaf25` (accepted) | conformance 20 pass + 1 ignored gap; shared 4; ACP unit 71; 5x replays each |
| P0-split | 0/4 | pure-move split of `src/cli/acp.rs` (4182 > ceiling 3444) + `tests.rs` (2275 > 1380) + `daemon_router_tests.rs` (1815 > 1644); branch `fork/acp-module-split` off stage `38fa8f3ee` | Sol `b4d57c2d` (triton, `acp-module-split`) | done; in stage | `57bc78ce9`: budgets green, no ceiling raised; `ServerEvent::scoped` constructor; acp.rs 4182 -> 940; new modules transport/session/presentation/requests/prompt all < 1200; test names 71/45 unchanged; 123 ACP unit + 17 conformance + 4 shared pass |
| R2-fix | 3 | queued review #2 fixes (F1 F2 F3 F6 F7); branch `fork/acp-review2-fixes` off stage `76045f863` | Sol `aa00969c` (triton, `acp-review2-fixes`) | accepted; merged into `fork/acp-stage-next` `e87becb75` | F1 `9f5c97299`, F2 `8ae2d0834`, F3 `af56fa992`, F6 `82965e1f0`, F7 `415e31f2c`; 126 ACP unit + 17 conformance + 4 shared pass |
| CI-guard | 0 | clear clippy/warning/panic/swallowed-error ratchet failures; branch `fork/acp-ci-guardrails` off `fork/acp-stage-next` `e87becb75` | Sol `ae3f6bd4` (triton, `acp-ci-guardrails`) | done; is the new `custom`/stage head `bf9f0b22f` | run 42465256663 green; panic totals 8 -> 8, swallowed 17 -> 17 across split families; 3 swallowed + 1 panic genuinely removed |
| REL-name | rel | fork release naming `vX.Y.Z+fork.pNNN` (user-approved): build-meta `release_semver` accepts `+build`, updater tests, `fork-release.yml` tag check/title/notes; branch `fork/release-fork-naming` off `custom` | Sol `68a66c4c` (triton, `release-fork-naming`) | accepted at `905e8834c` (worker crashed after push: Codex app-server exit, triton load 28) | CI 42513153287 green @ `b42a9cc8a`; provenance preflight fix `905e8834c` reviewed |
| REL-reload | rel | `jcode update` reloads every reachable shared daemon (socket-env mismatch), plus a `server reload` hint; branch `fork/update-reload-all-daemons` off `custom` | Sol `bcab9716` (triton, `update-reload-all-daemons`) | accepted at `c3ae51461` (worker crashed before reporting; reviewed by orchestrator) | CI 42513311668 green |
| REL-agents | rel | agents repo `harness-update.py`: ignore `+build` in `version_key`, percent-encode the tag in the download URL; PR camerontaylor/agents#56 | orchestrator | merged `f70952f38` (CI green ubuntu+macos) | 11/11 tests; hosts pick it up on next agents deploy (needed before cutting v0.99.1004+fork.p199) |

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
  host: `ssh <host> paseo permit allow <agentId> <reqId> --input '{"answers":{"<header>":"..."}}'`
  (answer values are strings). Key by the question's `header` (e.g. `"Question 1"`); for Codex
  questions keyed `"0"` was rejected with "Answer Question 1 before submitting" (2026-11-09).
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
- 2026-11-08: Scanner root cause (per `1f8a9aff`): legacy denylist class
  `token-ssh-plumbing` matched the bare filename `id_rsa`/`id_ed25519` as key
  material. Fix: cq-toolkit PR #1226 (`bac3b296e`) removes only that
  filename alternative (no repo/path/comment allowlist), with regressions
  for both diff signs and real key shapes. `1f8a9aff` will redeploy ceres
  and re-scan the exact diff, then ask for resubmission
  (`--supersedes=jcode-acp-r1-stage-38fa8f3ee`). No jcode edit needed.
- 2026-11-08: Module split accepted at `a2900422e` (per-commit identical
  line multisets). Remaining fork-budget failures, traced: TUI `remote.rs`
  +2 = R1's `end: None, end_message: None` in a `TurnEvent` literal ->
  asked for a `ServerEvent::scoped(identity, event)` constructor replacing
  such literals workspace-wide (also shrinks core carry);
  `client_lifecycle_tests.rs` 1942 > 1939, `turn_event_channel_tests.rs`
  1617, `tests/e2e/acp_shared_session.rs` 1431 -> pure-move test splits.
  No ceilings raised.
- 2026-11-08: P0 DONE: first green Fork CI, run 42458512159 on
  `fork/ci-baseline` @ `a7a3c29d0` (Quality 3m35s warm; Build&Test 36m24s).
  Env overrides `JCODE_{CODE_SIZE,TEST_SIZE,PANIC,SWALLOWED_ERROR}_BUDGET`.
- 2026-11-08: Split budget follow-up done (`57bc78ce9`): `ServerEvent::scoped`
  constructor (2 production literals), test-file pure-move splits (19/14/4
  test names preserved); all gates green, fixtures unmodified.
- 2026-11-08: Stage = previous + ci-baseline merge (`9190fb8ea`) + split
  (merge) + `76045f863` ratcheting fork-owned ACP budget entries down
  (acp.rs, acp/tests.rs, daemon_router_tests.rs, test_support/mod.rs leave
  the baseline; daemon_router.rs 1617 -> 1307). Rejected a blanket
  `--update` (would have tightened upstream-owned ceilings, making base
  bumps fail on upstream growth). Local budget checks exit 0. Pushed; Fork
  CI run 42461964356 on `fork/acp-stage` in progress = the candidate.
- 2026-11-08: Asked the user (AskUserQuestion) for authorization to
  fast-forward `custom` (repo rule: authorization before merging); no answer
  yet, `custom` untouched.
- 2026-11-08: Review #2 resubmitted after scanner fix (cq-toolkit #1226 =
  `18896829a`, deployed on ceres): `jcode-acp-r1-stage-38fa8f3ee-r2`,
  `--repeat --supersedes`, item `lr-b26eef738617-g2`, generation 2, head
  `38fa8f3ee`. First try `indeterminate` (queue lock busy), same-id retry
  admitted. Polling.
- 2026-11-08: USER AUTHORIZED the merges for this plan. Rule applied:
  fast-forward `custom` only to a SHA with a green Fork CI run (candidate
  `76045f863`, run 42461964356). Later lanes (corpus, review #2 fixes)
  integrate the same way: topic -> stage -> green CI on that SHA -> ff
  custom.
- 2026-11-08: P3-corpus dispatched (Sol, triton): fixtures 05, 07, 08.

### 2026-11-08 — queued review #2 result

- Request `jcode-acp-r1-stage-38fa8f3ee-r2` completed (receipt `91e7f79d…`, head `38fa8f3ee`, 3 major / 4 minor / 2 trivial). The report is on triton under `~/.cache/jcode-review-38fa8f3ee/.cq/queue-evidence/results/91e7f79d…/report.md`.
- Every finding was checked against stage `76045f863`:
  - **Accepted:**
    - F1: the `detach_view` doc comment belongs to `close_view`.
    - F2: the stale fixture-drift paragraph in `p3-mappings.md`.
    - F3: `route()` emits a Plan or sends `GetTodos` after `view_closed`.
    - F6: an explicit `skill` selection is overridden by `parse_acp_slash_command`, in both `prompt.rs` and the `concurrent` computation.
    - F7: plausible. The dispatcher is sequential, but a prompt task spawned before detach or close can register after `detach_view`, so it never settles and the detach wait can hang.
  - **Skipped:** F4 and F9 are in upstream-owned `translate.rs` bootstrap code. The fork diff there is 2 lines.
  - **Declined:**
    - F5: re-adding `turn_ended` would bring back the R1/D3 competing lifecycle. Old fork clients already fall back to scoped Done/Error, so only the cancelled label degrades.
    - F8: the env lock (`jcode_home_test_lock`) is already shared, and the dedup would edit upstream `translate_tests.rs`.
- Accepted findings were routed to Sol `aa00969c` as `R2-fix`. Reported to `1f8a9aff`.

### 2026-11-08 — P3-corpus accepted; config peer-update gap

- Sol `e66fe5f0` recorded the three scenarios on `fork/acp-corpus-v1`:
  - 05: owner-aware close
  - 07: config, commands and legacy aliases
  - 08: negotiation and legacy degradation
- Fixtures 01–04 and production code are unchanged. I spot-checked the 05 assertions: owner cancel with the turnId correlated, exactly one observer `canceled` terminal, and peer completion.
- **Gap:** a model change by one client sends no `config_option_update` to the other attached peers. The contract says "follow updates from any client". The ignored test `acp_conformance_config_peer_update_contract` reproduced it 5/5 times.
- **Decision:** fix it rather than amend the contract. The fix went to `e66fe5f0` on the same branch:
  - map the daemon's model/effort broadcast to a peer `config_option_update`
  - adapter-only fix preferred
  - un-ignore the test
- **Integration order:**
  1. Fast-forward `custom` to `76045f863` once run 42461964356 is green.
  2. Merge corpus and review2-fixes into the stage.
  3. Wait for one CI run on that stage head, then fast-forward `custom` again.

### 2026-11-08 — config peer-update gap fixed (`9bf7aaf25`)

- A thin, additive core hook was needed. The daemon's model, effort and route setters only acknowledged the requester.
- **The hook** (`crates/jcode-app-core/src/server/provider_config_fanout.rs`, plus 4 call sites in upstream-owned `client_lifecycle.rs`):
  - wraps the reply channel
  - forwards each event to the requester unchanged
  - fans successful `ModelChanged`/`ReasoningEffortChanged` out to the other connections on the session, with broadcast id 0
- **Review:** the relay task is justified because `handle_set_model`/`handle_set_reasoning_effort` use `spawn_deferred_agent_mutation`, so the acknowledgement can arrive late. The requester connection is excluded and its replies are unchanged. Upstream candidate: peers following config is a general TUI benefit.
- **Validation:**
  - conformance 21 pass, 0 ignored
  - shared-session 4 pass
  - ACP unit 71 pass
  - no fixture changed
  - the router regression test fails without the fix and passes with it
  - CI run 42463671628 is pending

### 2026-11-08 — review #2 fixes accepted; stage-next integration

- **F7:** prompt admission (`register_open_prompt`) and the daemon send now run under the router lock, the same lock `close_view`/`detach_view` take.
  - A late prompt either registers before closure and is withdrawn, or settles `cancelled` without sending.
  - The lock order is always router then writer; `session.send` takes only the writer lock, and sink actions are applied after the router lock is dropped.
  - Regression `late_prompt_registration_after_detach_or_close_is_rejected`.
- **Regressions:** F3 `closed_views_drop_todo_snapshots_and_refreshes`; F6 `explicit_skill_keeps_slash_commands_as_prompt_text`. Each failed with its guard removed.
- **Integration branch:**
  - Fork CI has `cancel-in-progress` per ref, and pushing `fork/acp-stage` would cancel the candidate run 42461964356. Integration therefore goes on `fork/acp-stage-next`: `76045f863` + `fork/acp-corpus-v1` (`9bf7aaf25`) + `fork/acp-review2-fixes` (`7c542190d`) = `e87becb75`.
  - Both merges were clean. fmt-check, module and size budgets pass locally.
  - CI run 42463885362.
- **Plan:**
  1. Fast-forward `custom` to `76045f863` on green 42461964356.
  2. Then fast-forward `fork/acp-stage` and `custom` to `e87becb75` on green 42463885362.

### 2026-11-08 — candidate CI red (run 42461964356 @ `76045f863`); `custom` NOT moved

- **Test jobs:** there was no test-result failure. Build & Test failed only at the warning budget, and Quality Guardrails failed at clippy, the warning budget, the panic ratchet and the swallowed-error ratchet.
- **Causes:** debt from lanes integrated after the last green run (P1-race, R1, the module split).
  - Dead `cancel_processing_message` and its import.
  - Test-only `TurnOwnership::claim`.
  - `too_many_arguments` on `cancel_processing_message_for_turn`.
  - Per-file panic and swallowed-error ratchets flag code moved by the split into new files, plus a genuine growth in `client_processing.rs` (21 -> 22).
- **Fix:** sent to Sol `ae3f6bd4` as `CI-guard`:
  - Moved allowances are relocated, with per-file totals shown not to increase.
  - Genuinely new usages are removed.
  - No blanket `--update` and no raise for any upstream-owned file.
- **Lesson:** my local pre-push gates (fmt, module, size budgets) did not include clippy or the panic/swallowed ratchets. From now on, run those on stage merges too, or rely on topic-branch CI before staging.
- **New plan:** `custom` fast-forwards straight to the first green stage head, `fork/acp-stage-next` + CI-guard. `76045f863` is superseded.

### 2026-11-08 — cleanup of finished lanes

- **Archived 12 lane workspaces** (agents archived, worktree directories removed). Each was first verified clean, with no commits missing from origin.
  - triton: acp-conformance, acp-r1-lifecycle, acp-p3-corr-detach, acp-corpus-v1, acp-module-split, acp-p3-skill-mcp, acp-r2-catalog, acp-review2-fixes
  - atlas: acp-p0-ci, acp-p1-meta, acp-p1-race, acp-p3-map
- **Remote branches kept.** The stage and `pr/*` refer to them.
- **One uncommitted rustfmt-only wrap** in `acp-p3-skill-mcp`'s pre-split `tests.rs` was saved to triton `~/.cache/acp-p3-skill-mcp-uncommitted-fmt.patch`. It is superseded: the stage passes fmt-check.
- **Review scratch worktrees removed** on triton (`~/.cache/jcode-review-104604e23`, `~/.cache/jcode-review-38fa8f3ee`). Their `.cq` queue evidence, including the review #2 `report.md`, was copied (verified with `diff -rq`) to triton `~/.local/state/acp-review-evidence/<name>.cq/`.
- **Kept:**
  - this orchestrator worktree
  - atlas `~/.cache/acp-stage` (on `fork/acp-stage-next`)
  - triton `acp-ci-guardrails` (Sol `ae3f6bd4`, running)


### 2026-11-08 — cq client update (from `1f8a9aff`)

- cq-toolkit #1227 (`a4686ec6`) is merged. It fixes review-admission contention.
- The triton task PATH `/home/ctaylor/.local/state/cq-jcode-review-20261108/bin` now has a stable `cq` launcher, which runs `~/.cache/cq-client-a4686ec6/dist/cli.js`. Use `cq review.submit|status|result` instead of `node ~/.cache/cq-client-e61fc8c9/...`.
- Keep `CQ_QUEUE_SSH_ALIAS=cq-queue-ceres`, and unset `PASEO_AGENT_ID` and `CQ_REVIEW_NOTIFY_HOST`.
- The client retries a typed busy response for up to 35 s.
- I told `1f8a9aff` that the old review cwds were removed and their evidence preserved, and offered to restore them if a request id needs its original cwd.

### 2026-11-08 — `custom` advanced (first fork-CI-gated integration)

- **Review of CI-guard (`bf9f0b22f`):**
  - Budget allowances were transferred with totals unchanged: panic 2+6 -> 1+5+2; swallowed 17 -> 17.
  - No upstream-owned allowance was raised.
  - `cancel_processing_message` wrapper removed; tests call the turn-aware entry with `None`.
  - `#[allow(clippy::too_many_arguments)]` matches an existing sibling in upstream `client_processing.rs`.
  - `TurnOwnership::claim` is `#[cfg(test)]`.
  - `RoutedEvent::Event` is boxed (clippy `large_enum_variant`).
  - Failed sends are logged, not dropped.
  - `parse_acp_slash_command` returns `None` for an absent name (`?`, not `unwrap_or_default`).
- **Fast-forwards to `bf9f0b22f`**, which has a green Fork CI run on the exact SHA (42465256663: Quality Guardrails + Build & Test):
  - `fork/acp-stage`: `76045f863` -> `bf9f0b22f`
  - `custom`: `a047a0c07` -> `bf9f0b22f`
  - No history rewrite.
- **`custom` now contains:** P0 CI, P1 meta/race, T-conf, R1, R2, P3 corr/detach/skill/mcp/map/corpus (+ config peer fanout), the module split, the review #2 fixes and CI-guard.
- **Confirmation run** on `custom` (push trigger): 42467221310.
- **Desvio:** `manifest.txt` is `custom` only (no appended carries), so the direct fast-forward is consistent. `mine` is stale until the next `jcode-desvio build`.
- **Remaining open items** (not blocking):
  - Upstream candidates: `a7a3c29d0` (stdin wchan), `a47408ec1`, D1/D2 on `pr/turn-scoped-cancel` / `pr/stale-cancel-flag`, and the config peer fanout.
  - Phases 4–5 (v2) stay gated, per the strategy.


### 2026-11-08 — `custom` confirmation run green

- Run 42467221310 (push to `custom` @ `bf9f0b22f`) passed both jobs: Quality Guardrails and Build & Test (ubuntu-latest). This is the recorded fork CI baseline.
- Phase 0–3 scope is complete. Phases 4–5 (v2) remain gated.

### 2026-11-08 — build and deploy `custom` (user request)

- **Build:**
  - Desvio `build --bisect-gate` on atlas assembled `mine` `19d0b7afc` = base + `custom` `bf9f0b22f`. The tree equals `custom`.
  - `desvio_verify` passed: modules, fmt-check, `cargo check --all-targets`.
- **Problems before a clean run:**
  - The local `custom` in `~/repos/jcode` was stale (`a047a0c07`); fast-forwarded to `origin/custom`.
  - The interrupted build left `build-tree` detached; it was clean at the base and reattached to `mine`.
  - The partial clone (`blob:none`) was missing 2 blobs for rerere training; fetched lazily.
- **Smoke test:** the selfdev binary ran against an isolated daemon on `/run/user/502/jcode-smoke-acp.sock`.
  - `initialize` returned `_meta.jcode` (4 v1 extensions, interfaceVersion 1).
  - `session/new` returned `_meta.jcode` with `sharedControl.detach=true`, plus configOptions and the `available_commands_update`.
  - The smoke daemon is stopped and the socket trashed.
  - The dev version stamp in the selfdev binary is stale (`0.99.816-dev`, cached build-meta). Release builds stamp it from the tag, and CI checks it.
- **Deploy path:** fork release tag `v0.99.1003` on `bf9f0b22f` -> `fork-release.yml` (4 targets, published as Latest) -> per-host `scripts/harness-update.py --only jcode` (agents repo).
  - The tag create/push was **denied by the auto-mode permission classifier**. No tag exists locally or on origin, and nothing was deployed.
  - Waiting on the user.

### 2026-11-08 — release v0.99.1003 deployed (user: "push and test the deployed instances")

- **Release:**
  - Tag `v0.99.1003` (annotated) on `bf9f0b22f`.
  - `fork-release.yml` run 42472288600: 4 builds + publish succeeded, published as Latest.
  - Assets: linux x86_64/aarch64, macOS aarch64/x86_64, SHA256SUMS.
- **Deploy order:** `jcode update` force-reloads the host daemon (`server reload --force`), so hosts without a running daemon went first.
  - **pluto, ceres, saturn, neptune:** `harness-update.py --only jcode` -> `jcode v0.99.1003 (bf9f0b22f)`. Channels current/stable/shared-server -> `versions/0.99.1003`.
  - **uranus:** api.github.com returned 502/503/504 transiently from uranus (asset downloads fine); updated on retry.
  - **atlas:** ran a self-dev build (`0.99.38-dev`, 4e9a54972), which `jcode update` treats as current. I installed the release by hand in the same layout:
    - SHA256-verified `jcode-linux-aarch64.tar.gz` -> `versions/0.99.1003/{jcode,jcode-linux-aarch64.bin}`
    - current/stable/shared-server repointed
    - previous links and version files saved in `~/.jcode/builds/state-backup-20261108/`
  - **triton: DEFERRED.** Its daemon serves live 0.99.1002 ACP adapters for other workstreams' jcode Paseo agents (5 running cq-toolkit reviews). Waiting for an idle window (watcher) before `harness-update` and the reload.
- **Testing:** `~/.cache/acp-deploy-smoke.py <bin> --prompt` runs an isolated daemon on a private socket with ACP initialize / session/new / one-line prompt / session/close, and checks the v1 fields: `_meta.jcode` extensions, interfaceVersion, clientInstanceId, `sharedControl.detach`, configOptions, `end_turn`, turnId correlation and the reply text.
  - **Full pass (13/13), protocol and prompt:** atlas, neptune, uranus.
  - **Protocol checks 10/10, prompt failed:**
    - pluto and saturn: auto provider -> OpenRouter, `OPENROUTER_API_KEY` 401 "User not found"
    - ceres: "No tokens/providers left" (Copilot unauthenticated, usage exhausted)
  - **Pre-existing, not a regression:** the same prompt test with pluto's previous 0.99.1002 binary fails identically. These are host credential issues and were not changed (no credential changes).

### 2026-11-08 — triton deployed; fleet complete on v0.99.1003

- **Rollout:** waited for an idle window (no jcode-provider Paseo agent running a turn), then ran `harness-update.py --only jcode` -> `v0.99.1003 (bf9f0b22f)`.
- **Gotcha:** the `server reload --force` that `jcode update` issues went to the wrong socket.
  - My SSH shell has `XDG_RUNTIME_DIR=/run/user/502`, so it targeted `/run/user/502/jcode.sock` ("No running jcode server found").
  - The Paseo-spawned daemon has no `XDG_RUNTIME_DIR` and listens on `/tmp/jcode-502/jcode.sock`.
  - Both versions share the same `runtime_dir()`, so this is an environment mismatch, not a regression.
  - Fix: `env -u XDG_RUNTIME_DIR TMPDIR=/tmp jcode --no-update server reload` reloaded PID 371976 in place onto 0.99.1003.
- **After the reload:**
  - All 18 idle 0.99.1002 `jcode acp` adapters stayed connected.
  - An in-flight review turn (`session_dove`) kept streaming.
  - No WARN/ERROR beyond pre-existing skill-shadow notices.
- **Tests:**
  - Private-daemon smoke test: PASS 13/13.
  - **Live shared daemon** (new script mode `--existing-socket=/tmp/jcode-502/jcode.sock`, which never stops the daemon): PASS 13/13, including a real prompt with `end_turn` and turnId correlation.
- **Fleet:** every host on `v0.99.1003`.
  - Full prompt-level PASS: atlas, neptune, uranus, triton.
  - Protocol-level PASS only: pluto, saturn, ceres. Their prompt failures are pre-existing provider credentials (OpenRouter 401, no provider), identical with 0.99.1002.
- **Follow-ups:**
  - Distinct fork release naming, proposed to the user: `v0.99.1004+fork.p199`. It needs `harness-update.py` `version_key` to drop `+` metadata (currently raises ValueError).
  - `jcode update` and harness-update should reload the daemon on the socket it actually uses. Run from a login/SSH shell, they miss a daemon started under Paseo's environment.


### 2026-11-09 — release naming + reload follow-ups (user approved both)

- **Naming:** user approved `v0.99.NNNN+fork.pNNN`. The next release will be `v0.99.1004+fork.p199`.
  - **Constraints verified:**
    - jcode `version_is_newer` uses `cmp_precedence`, so `+meta` is ignored and safe.
    - `-fork` would sort as a prerelease below the release, so it was rejected.
    - build-meta `release_semver` rejects `+` (fixing: REL-name).
    - harness-update `version_key` raised ValueError (fixing: REL-agents, PR #56).
    - Asset names must not change.
  - Existing release retitled to "jcode fork 0.99.1003 (upstream v1.0.0-preview.199)"; the tag is unchanged and it is still Latest.
- **Reload:** REL-reload makes `jcode update` reload every live daemon on the candidate sockets (current, XDG, `/tmp` and `$TMPDIR` fallbacks, JCODE_* overrides), with per-socket reporting and no false "requested". Tests use temp-dir sockets only.
- **Order:** merge REL-name and REL-reload into `custom` once Fork CI is green, and merge agents#56, before cutting `v0.99.1004+fork.p199`.

### 2026-11-09 — release lanes accepted; release stage

- **Naming review:**
  - `release_semver` keeps `+build` and rejects malformed or injected metadata.
  - `fork-release.yml` gained a `preflight` job that runs before any build. It does the tag-format check and resolves `v1.0.0-preview.N` from upstream with `git ls-remote` (no upstream tags pushed to the fork). It checks that the upstream SHA is an ancestor of the tagged commit; verified true for `custom`, and preview.199 is a lightweight tag that the fallback handles. It also computes the custom merge-base.
  - The release title is "jcode fork X (upstream v1.0.0-preview.N)", with provenance notes.
  - Installers percent-encode `+`.
- **Blocking issue found in review and fixed:** the first cut read the upstream tag from the fork remote, which lacks it.
- **Reload review:**
  - `socket_discovery.rs` builds a deduped candidate set and checks liveness (an owned socket that answers the ping); it never starts a daemon.
  - `update_reload.rs` reloads each live daemon with explicit `--socket`, keeping `--force` as before, and reports reloaded/skipped/failed per socket, or says no daemon was found.
  - `server reload` names other live daemons.
  - The upstream-owned change is a thin `fallback_runtime_dir_in` helper.
- **Workers:** both crashed (Codex app-server exited; triton load average about 28). Their worktrees were clean and fully pushed.
- **Stage:** `fork/release-stage` = `custom` + naming (fast-forward) + reload = `0f9a3e5de`. fmt, module and all 4 budgets pass locally; Fork CI is running. On green, fast-forward `custom`, then ask the user before tagging `v0.99.1004+fork.p199`. The hosts' agents checkouts need agents `f70952f38` first.


- 2026-11-09: Fork CI run 42515725129 on `fork/release-stage` `0f9a3e5de` green (Build & Test, Quality Guardrails). `custom` fast-forwarded `bf9f0b22f..0f9a3e5de`; local `~/repos/jcode` custom synced. Next: deploy agents `f70952f38` to hosts, then (with user approval) tag `v0.99.1004+fork.p199`.
- 2026-11-09: agents `f70952f38` status: ceres, uranus already had it; pluto fast-forwarded (only untracked files). atlas, triton, saturn, neptune checkouts have other workstreams' uncommitted edits overlapping the incoming diff, so they were NOT pulled. Rollout plan for those: `git show origin/main:scripts/harness-update.py > ~/.cache/harness-update-f70952f.py` and run it with `--manifest ~/.local/agents/manifests/harnesses.conf --only jcode`. Awaiting user approval to push tag `v0.99.1004+fork.p199`.
- 2026-11-09: Released `v0.99.1004+fork.p199` (run 42519253015, all 6 jobs green; preflight validated upstream base `7fd42339c`). Title "jcode fork 0.99.1004 (upstream v1.0.0-preview.199)", Latest, notes carry tagged/upstream/custom SHAs. All 7 hosts on `jcode v0.99.1004+fork.p199 (0f9a3e5de)`.
  - ceres/uranus/pluto via checkout harness-update; saturn/neptune/triton via `~/.cache/harness-update-f70952f.py`; atlas manual (SHA256 OK; tarball launcher `jcode-linux-aarch64` renamed to `jcode`).
  - triton: update's reload still missed `/tmp/jcode-502/jcode.sock` because harness-update runs the *previously installed* (1003) binary's `jcode update`; the new reload-all code takes effect from the next release. New `server reload` hint confirmed live ("Another jcode daemon is running at /tmp/jcode-502/jcode.sock"); reloaded with `--socket`, PID 1892446 re-exec'd onto 1004; Paseo jcode agents 6 idle, none errored.
  - Smoke: full PASS atlas, neptune, uranus, triton (private + live). pluto/saturn protocol PASS, prompt OpenRouter 401; ceres protocol PASS, prompt no provider (pre-existing, unchanged).
  - Follow-up candidate: release caching (tag-scoped caches unusable by next tag; linux compat container never cached; 9/10 GiB used).
- 2026-11-09: User approved release caching + native Linux x86_64 (compat container manual-only; keep Intel macOS for now) and the post-install re-exec.
  - REL-cache (mine, atlas `~/.cache/acp-release-cache`, branch `fork/release-cache` `204872da2`): fork-release also runs on push to `custom` (paths-ignore docs/md) as a no-publish cache-warm matrix; rust-cache `save-if` custom only in fork-release and fork-ci; Linux x86_64 native ubuntu-24.04 (glibc 2.39 <= pluto's 2.40); `build-linux-compat` job only on dispatch `linux_compat=true`, artifact `compat-jcode-linux-x86_64`, release downloads `pattern: jcode-*`; release job gated on tag + preflight/build success; non-release runs cancel-in-progress. actionlint 1.7.12 clean. Test dispatch run 42523722074 (no tag, linux_compat) confirmed preflight skipped, builds running.
  - Deleted 7 unusable caches (5 tag-scoped, 2 `fork/acp-stage`); remaining: 2 `custom` caches, 2.5 GB.
  - REL-reexec (Sol `3b2a7d9d`, medium, triton workspace `wks_c78c979d8fd06caa`, branch `fork/update-reexec-post-install`): post-install reload performed by the newly installed binary via hidden subcommand, fallback in-process only when unsupported/unspawnable, no double reload.
- 2026-11-09: Paseo deploy check: all 10 Paseo hosts' daemons already run fork `0.15.2-fork.261108-1211.g6b45103d7` (mine built after paseo#10 "safe shared-session ACP steering"; paseo `custom` 324dff30c has 0 unreleased commits). No Paseo redeploy needed. Paseo consumes the jcode contract at `a047a0c` (peer turns, steering, Stop policy, MCP policy); turn correlation, detach, skill selection not yet consumed.
  - E2E-paseo (Sol `7c2debb9`, medium, full-access, atlas workspace `wks_ef911201bf56caea`, dir `~/.cache/paseo-jcode-e2e`): isolated Paseo daemon + private jcode runtime; TUI in tmux; scenarios 1-8 (regression, import, shared session both ways, steering both ways, Stop, no-cancel on send/refresh, independence, misattribution). Report -> `~/.cache/paseo-jcode-e2e/REPORT.md`.
- 2026-11-09: E2E-paseo result (`~/.cache/paseo-jcode-e2e/REPORT.md`): PASS 1 regression, 2* import (CLI needs encoded `plugin:{...}` handle; raw native id fails), 3* shared session both ways, 6* send/reload does not cancel TUI turn, 7 independence. FAIL 4 reverse steer (TUI steer into Paseo turn missing from Paseo timeline), 5 Stop (Paseo INTERRUPTED+idle but TUI turn continued despite `stopObservedTurn=true`), 8 duplicate first user msg after import, transient closed state after Paseo agent reload. Deviation: isolated daemon inherited PASEO_PASSWORD env (nothing read/copied; temp home removed).
  - My check: report's Stop hypothesis is wrong (`RouterState::stop` only withdraws waiting prompts; `peer_turn` survives to `active_turn_identity`).
  - E2E-fix (Sol `498cce41`, medium, full-access, atlas `wks_a35147bda358546d`, branch `fork/acp-e2e-defects`): localize Stop / reverse steer / duplicate at the ACP wire (jcode vs Paseo), fix jcode-side with router regression tests; Paseo-side causes documented for a Paseo lane.
- 2026-11-09: REL-reexec delivered `1b7209d87` (CI 42523842597 green): hidden `update-post-install [--probe]`; probe exits 73 = supported, clap exit 2 / spawn failure = fall back to in-process `reload_all`; once the reload child starts, no fallback (no double reload); `JCODE_POST_INSTALL_REEXEC` recursion marker; bounded waits (10 s probe, 120 s reload). Sent back: (1) revert formatting churn in upstream-owned `args.rs`/`dispatch.rs`; (2) skip the `unshare -Urnm` integration test when namespaces are unavailable; (3) Fork CI never runs root-crate unit tests or `tests/*` (only `--lib --bins --no-run`), so add a targeted step running `update_post_install`/`update_reload` bin tests and `--test update_post_install`.
  - REL-cache test run 42523722074: native ubuntu-24.04 Linux x86_64 and aarch64 builds green; macOS + manual compat still running.
- 2026-11-09: REL-reexec worker asked: pure-additions `args.rs` exceeds code-size budget 1298 -> 1306; answered: raise that one file entry only, no blanket update.
- 2026-11-09: User asked for solid protocol-boundary testing on the jcode side (complex contract, fuzzily defined space).
  - Correction: Fork CI does run `cargo test --test e2e` (in-process daemon + `MockProvider`) and `provider_matrix`; it does not run standalone `tests/*.rs` targets such as `update_post_install`.
  - ACP-SUITE (Sol `4881fd70`, medium, full-access, triton `wks_58be9b1a8d76c629`, branch `fork/acp-contract-suite`): clause IDs in `ACP_FORK_INTERFACE.md` + `tests/e2e/acp_conformance/TRACEABILITY.md` (clause -> tests, ambiguity list with proposed wording for owner approval); multi-client scenarios using gated MockProvider; seeded interleaving/invariant test; new corpus transcripts + live-match test; the three E2E defects as expected-fail tests; all running in Fork CI. Test-only; no production fixes.
  - QUEUED: PASEO-E2E (Sol, medium) — `packages/server/src/server/daemon-e2e/jcode-shared-session.e2e.test.ts` in the Paseo fork, fake OpenAI-compatible server via `JCODE_OPENAI_COMPAT_API_BASE`, pinned jcode fork release binary, second daemon client as TUI stand-in, scenarios 1-8, replay jcode corpus; added to `test:integration`. Start after E2E-fix (`498cce41`) reports localization.

### 2026-11-09 — orchestration takeover from quota-exhausted Opus

- Lead is now Sol `620fea4c-327c-464b-8f9f-28d02cddedb0` on atlas, same orchestration workspace. Completion reports route here.
- Recovered completed work: re-exec `b4e71cdce`, Fork CI 42525588676 green; cache `204872da2`, Fork CI 42523721421 and five-target dispatch 42523722074 green (including both macOS and manual compat).
- Defects `39fb76205`, Fork CI 42526665518 green: Stop is a core 500 ms cancellation reset racing bash's 750 ms shutdown handoff, fixed by `d46434b52`. Reverse steer is Paseo's foreground `reduceLiveUserChunk` filter; jcode emits it correctly. Original duplicate already existed in persisted history; clean replay emits each stored row once. Source: `docs/ACP_E2E_DEFECT_LOCALIZATION.md` on the defect branch.
- Contract suite `f92acd7cb` has exact-head green CI 42528125499, but is incomplete. Resumed `4881fd70` on triton to complete clause coverage, broader interleavings, faithful Stop/replay tests and canonical transcripts; correct false expected-fail assumptions using localization. Ambiguous wording remains proposed for owner approval.
- Started PASEO-E2E Sol `ffb4014a-89ef-4705-9a87-3b0a9e9ef638`, atlas workspace `wks_ed2c27a887549728`, branch `fork/jcode-shared-session-e2e` off `origin/custom`. Includes reverse-steer fix with own-echo regression, daemon-level fake-model tests and corpus replay. Fixed Stop requires a pinned fixed source build or a future release; installed p199 lacks it.
- Resumed `498cce41` to review and stage Stop + re-exec + cache together off latest custom, preserving lane commits and obtaining combined exact-head Fork CI. Report before advancing custom; no release or deployment in this step.

### 2026-11-09 — completed lanes integrated; expanded tests finding defects

- `custom` fast-forwarded `0f9a3e5de..87ee16d96ad86d9dd8b815f1e010ea560be38308` by ordinary push, remote SHA confirmed. Includes Stop, post-install re-exec and release caching; exact-head Fork CI 42531331138 passed. Manual review recorded in `docs/ACP_INTEGRATION_REVIEW.md`; CodeRabbit unavailable. No tag, release or deployment.
- Contract suite candidate `101858d8b` plus local additions `8842425e6e1`: 34 ACP E2E pass, three baseline failures ignored. 152 indexed clause IDs are not 152 independent behavioral proofs. Full compact/subagent execution remains partial; six wording proposals await owner review. Seeded three-client operation tests, ten wire transcripts and explicit adapter-unit CI execution added.
- Faithful Stop regression passes on stage (1.05 s). Newly discovered repeated-load bracket defect fixed separately on `fork/acp-load-bracket`, head `e298b1cf8`: reuse live attachment/router, reconnect finished reader. Faithful wire regression passes with fix (0.69 s); exact-head CI 42532296659 pending at this update.
- Newly confirmed lost-steer defect: queued, uninjected input survives killed steerer and triggers an extra provider call, violating transport-loss withdrawal. `498cce41` implementing transport-owned pending-input cleanup on separate `fork/acp-lost-steer`, preserving other clients and inserted work. Final suite integration must enable all three regressions after fixes validate.
- Peer config requests intentionally serialize behind active calls; dedicated tests verify deferred mutation, not compulsory rejection. Own busy requests reject atomically. Whether peer rejection should be required remains wording ambiguity, no production change.
- Paseo PR #11 (`fork/jcode-shared-session-e2e`, initial head `73d05737a`) adds reverse-steer fix, unchanged-model attach skip, late-subscription active-turn catch-up, corpus tests and Linux CI building pinned jcode `39fb76205`. Full fake-model daemon E2E passes locally with actual bash cancellation, held-peer reload, continued steering, stable replay IDs, attribution and client independence.
- Lead manual review found two concrete follow-ups: same-text peer chunk arriving before own echo must use supplied message identity, and unchanged effort/mode setters must also be skipped. Worker fixed both locally; 88 ACP tests and configured-high-effort daemon E2E pass. Follow-up push and exact-head CI pending. No Paseo merge/deploy yet.

### 2026-11-09 — contract suite and discovered jcode fixes integrated

- Lead verified and fast-forwarded remote `custom` from `87ee16d96` to `d66d8d2181069c3524345723cf8ac0772b23944f` (ordinary push; remote exact SHA confirmed). Exact-head Fork CI 42534258327 passed: 128 ACP adapter units, 97 E2E passed / 7 existing ignored. Stop, repeated-load and lost-steer behavioral regressions are enabled and explicitly passed in CI.
- Preserves suite `8842425e6`, load fix `1825e3bc3` and lost-steer fix `96cc79e70`; all three individual heads also have green Fork CI. Load fix retains attachment identity only for reused live connections, with reconnect/different-session/process tests. Lost-steer fix uses daemon-generated transport ownership under injection lock, not caller IDs, and preserves inserted/other-client work. No wire fields added. Review/provenance: `docs/ACP_INTEGRATION_REVIEW.md` on integrated head. No tag/release/deployment.
- Suite final CI 42533172364 proved named tests executed. 152 clause references indexed, bounded three-client generator (12 exhaustive + 24 default seeded schedules), ten transcripts, seven wording proposals and explicit coverage limitations; full subagent/compaction scheduling remains partial.
- Paseo review corrected the earlier blanket unknown-message-ID rule: generic native IDs do not correlate with submission IDs. Final approach restores generic text echo matching and negotiates no-own-echo only for verified `_meta.jcode.interfaceVersion: 1`; generic equal-text peer attribution remains ambiguous without correlation. Source/router/corpus audit confirmed jcode v1 suppresses own prompt and steering echoes. Unknown/future metadata stays generic.
- Paseo head `f691ace7ff5fc10ba5aac4ed30409a4511a1de44`: all review threads addressed, CI consolidated into existing Ubuntu server job and local-E2E script per repo instructions, credential-free scoped checkout. Local 92 ACP / 24 capability / 25 workflow tests plus daemon E2E pass; exact-head CI 42535381671 and Fork CI 42535381658 pending. PR #11 remains unmerged.

### 2026-11-09 — release authorization, deployment findings and follow-up lanes

- User authorized merge, then release/deploy once known issues are resolved. Paseo PR #11 merged by lead as `5d23f8b6267505c287cb7a14a3a5638e2ea3c25a` after attested Sol PASS, green primary/Fork CI and same-head successful desktop packaging retry. Initial AppImage exit 127 remains unexplained; diagnostics retained, no launcher workaround added.
- Jcode `v0.99.1005+fork.p199` on `d66d8d218` published Latest; release CI 42536770694 green, all four native archives/checksums/provenance verified. Installed binary hashes/channels/probe verified on all seven hosts. Honest rollout/incident record: `docs/ACP_RELEASE_1005.md` on `fork/release-1005-evidence` (latest `7d0745bb2`). Some earlier smoke runs labeled live used an old private-only script; those labels are retracted. Atlas private/new-shared, triton existing-shared and Neptune corrected existing-socket smoke passed; remaining hosts have private checks with pre-existing credential-only prompt failures.
- Triton updater was launched before the active-agent query result was inspected, disrupting an old adapter's next prompt with Broken pipe. Prior task checkpointed; owning parent replaced it with `f49aaec5`, whose actual PR1279 review completed. No duplicate prompt sent. Existing ACP reader exits on daemon EOF without reconnect; unchanged from 1004. Neptune's idle adapter was safely reattached with supported `paseo agent reload`, same native session and byte-identical conversation, without user prompts.
- Neptune then exposed a pre-existing cleanup defect: synchronous telemetry project scan blocked filesystem `open` while holding global client registry write lock, stalling Ping. Sample establishes cause; normal private 1004/1005 probes do not reproduce the exceptional filesystem stall. It later recovered naturally: same PID 87836, socket inode 69898860, bonehound attachment, idle/error-free Paseo state and exact 188-message conversation hash verified. No diagnostic restart/reload. Immediate health hold cleared; preventative fix not yet deployed.
- Telemetry fixer `498cce41` branch `fork/acp-disconnect-telemetry`, head `2ac1da281`: owned capture clears terminal slot once; ID lookup, finalization, profile/transcript/hook reporting off registry guard on blocking worker. Lead review fixed remaining capture-time I/O. Real ID and under-lock negative controls fail as expected; restored clean source, 20 cleanup tests + 3 telemetry tests/clippy/ratchets pass. Exact CI 42542843335 quality green, build pending.
- User explicitly requested Sol for reconnection: `cfd15b23-15e7-4a08-9182-78e2bbaf620f`, atlas `fork/acp-daemon-reconnect`, head `69de84652a423a940fbe8607d7c762a3b4a4d731`. Same endpoint/session recovery, no uncertain request retry, bounded I/O handshake retry, unknown offline outcomes explicit; non-Unix endpoint unavailable disables auto recovery. Local 136 ACP tests and actual private same-PID re-exec/fake-provider prompt proof pass (two calls, no replay). CI 42542654867 quality green/build pending; explicitly runs real re-exec against built candidate. No shared daemon test/reload.
- Once both exact heads are green, `498cce41` will stage their preserved merges in `fork/acp-followup-integration`, run combined tests and exact-head CI, then report before custom advancement/tag. Expected next jcode release 1006 only after reviewed combined stage.
- Paseo release owner moved to Neptune Sol `4e104f61-9725-42de-b823-b4ea23ec3a41`; live resolver is triton, not stale Neptune docs. Base v0.15.2 + existing basket retained. Generated candidate diverged from prior mine only by old assembly merges; standard merge retained ancestry with identical source tree. Candidate `896bb6c9feacfb571338026b51ccecbedb34d819`, stamp `0.15.2-fork.261109-0007.g896bb6c9feac`, remains unpublished. Ceres isolated dependency/heap repairs preserved source/lock/shared modules; build, typecheck, lint and 456 named tests pass.
- Assembled candidate's shared-session scenario failed: timestamp trace proved provider released before steer admission, yielding new own turn rather than intended peer injection; harness then caught idle gap. Separate real SDK pending-steer rollover idle gap accepted as release blocker. `ffb4014a` owns fixture correction (queue-confirming second Ack before release, same-turn injection/end assertions; local pass) plus delegated production pending-work lifecycle fix on separate commits/new custom PR. No fake native brackets or protocol changes. Current candidate must be reassembled/revalidated after fixes; publication and Paseo rollout remain held. Neptune deploy must be through surviving Saturn worker, never self-deploy.
