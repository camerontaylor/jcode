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
| P0-ci | 0 | `fork/ci-baseline`: fork-owned workflow (quality + ubuntu build/e2e) on `custom` and `fork/**` | DeepSeek `9a617f29` (atlas, `acp-p0-ci`) | running | |
| P0-budget | 0 | same branch: fork budget baseline files + script flag | DeepSeek `9a617f29` | running | |
| P0-record | 0 | first green run on `custom` recorded here | orchestrator | blocked on P0-ci | |
| P1-meta | 1 | `fork/acp-capability-metadata`: `agentCapabilities._meta.jcode` (interfaceVersion, profile, extensions, sessionMcpServers) in every profile; `result._meta.jcode` {clientInstanceId, sharedControl} on new/load/resume; detach=false | DeepSeek `5365c86a` (atlas, `acp-p1-meta`) | accepted, awaiting integration | `d335f2e7d`: 5 unit + 1 e2e wire test pass; sharedControl derived from daemon Pong caps (`SharedControlSupport::from_router`) |
| P1-race | 1 | D1/D2 atomic turn-scoped cancel + epoch at turn claim, deterministic seam, terminal publication audit, mixed-version delivery (D3 guard); on `pr/turn-scoped-cancel` / `pr/stale-cancel-flag` merge-forward; topic `fork/p1-cancel-race` | Sol `9008bf12` (atlas, `acp-p1-race`) | running | |
| T-conf | 1→3 | `fork/acp-conformance`: wire-level protocol conformance suite + JSONL recorder/replayer for `tests/fixtures/acp/` | Sol `652f5f66` (triton, `acp-conformance`) | accepted, awaiting integration (after P0-ci green) | `4759e51b3`: 5 pass + 2 ignored contract gaps; 5/5 stable runs on triton |
| P2-R1 | 2 | remove competing lifecycle; split `daemon_router.rs` | Sol | not started (after P1-race) | |
| P2-R2 | 2 | catalog facade in `jcode-harness-api-server`; branch `fork/r2-catalog-facade` off `fork/acp-conformance` | DeepSeek `1334bb53` (triton, `acp-r2-catalog`) | running | |
| P3-corr | 3 | turn correlation `_meta.jcode.turnId` + `role` on prompt responses | Sol | not started | |
| P3-detach | 3 | `_jcode/detach`; also v1 observer close must end an open peer bracket with `canceled` (T-conf gap `acp_conformance_observer_close_bracket_contract`) | Sol | not started | |
| P3-skill | 3 | `skill: null` clear + `skill_selection` ext; branch `fork/acp-skill-clear-mcp-notice` off `fork/acp-capability-metadata` | DeepSeek `8e023355` (triton, `acp-p3-skill-mcp`) | running | |
| P3-mcp | 3 | ignored `mcpServers` notice (same branch/agent as P3-skill) | DeepSeek `8e023355` | running | |
| P3-map | 3 | reasoning/plans/tool metadata mappings | DeepSeek | not started | |
| P3-corpus | 3 | record scenarios 1–5, 7, 8 (v1) into corpus; CI replay | Sol | not started (needs T-conf) | |
| P0-split | 0/4 | split `src/cli/acp.rs` into transport/session/presentation | Sol | not started (after P3 adapter churn settles) | |

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
