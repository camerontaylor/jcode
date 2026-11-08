# Maintainable fork and ACP development strategy

Prepared 2026-11-08 against documentation commit `5e235215d`; amended the
same day after a review of its process assumptions (revision 2).
Status: implementation plan; the phases below are not claims of completed work.

### Preconditions verified on 2026-11-08

This strategy delegates verification to CI, the Desvio manifest and the
consumer. Those were checked rather than assumed, with these results. Re-check
this block whenever the document is revised; stale preconditions were the main
defect of revision 1.

- **CI does not run on the fork's product branches.** Upstream `ci.yml`
  triggers only on pushes and pull requests to `main`/`master` and has never
  run on `custom` or `mine` at `camerontaylor/jcode`. The only fork workflow
  that has run is `fork-release.yml`, which builds release artifacts and runs
  no tests, fmt, clippy or budgets. Phase 0 below fixes this.
- **The size-budget ratchets fail on the base tag itself.** At
  `v1.0.0-preview.199`, `scripts/check_code_size_budget.py` reports ten
  failures and `scripts/check_test_size_budget.py` seven, before any fork
  change. The fork adds more (see Phase 0). Upstream's `quality` job cannot
  be adopted unchanged.
- **The manifest is `custom` plus appended carries.** The `pr/*` stack and the
  `fork/*` feature branches are already merged into `custom`; the layered
  front/middle/back manifest described in the boundary report is archived at
  `~/.jcode-fork/state/manifest.pre-custom-baseline.txt`. The integration
  section below describes the current model.
- **Most planning documents are not in the assembled fork.** `custom` and
  `mine` contain only `ACP_SHARED_SESSIONS.md` and `ACP_UPSTREAM_PORT_PLAN.md`.
  This strategy, the interface contract, the gap handover and the boundary
  report live on `report-acp-gaps-v2-steering` and `fork/docs-boundary`.
- **Upstream base drift is currently small**: four commits past the tag. It
  will not stay small; upstream averaged about fifty commits a day and touched
  the ACP adapter files fourteen times in the thirty days before the tag.
- **Upstream deliberately tolerates client-supplied `mcpServers`** (commit
  `0517f01f2`, fixing issue 887). Revision 1 proposed reversing that; see
  Phase 3.

Build a compatibility-first fork: one daemon-owned execution model, a
fork-owned ACP adapter, and v1/v2 presentations of the same shared session.
Preserve existing ACP/TUI capabilities first, reduce costly core changes next,
then extend ACP. The consumer and jcode can develop independently against the
[interface contract](ACP_FORK_INTERFACE.md).

## Document map

- **This strategy** is the primary document for direction, order and scope.
- [ACP fork interface](ACP_FORK_INTERFACE.md) is authoritative for target wire
  behavior, extension payloads, compatibility choices and consumer expectations.
- [Fork boundary report](FORK_BOUNDARY_STRATEGY.md) supplies the footprint audit,
  defects D1–D3 and reshaping items R1–R6. Its original queue order is superseded
  by this strategy; its estimates are evidence, not promised savings.
- [ACP gap handover](ACP_V2_GAP_HANDOVER.md) supplies the pinned standards/SDK
  research. Its recommendations are superseded where this strategy or the
  interface contract chooses differently.
- [Shared-session history](ACP_SHARED_SESSIONS.md), especially Deviations 34–49,
  describes existing behavior to preserve. Earlier design sections include
  superseded daemon machinery.
- [Upstream port plan](ACP_UPSTREAM_PORT_PLAN.md) explains the move to upstream
  turn identity. Do not reintroduce its now-obsolete duplicate lifecycle.
- [Pinned ACP references](acp-pins/README.md) are verbatim copies of the v2
  draft documents, v2 schema and delivery RFD at the pinned commits, with
  hashes. The delivery RFD is hosted on a personal fork of the spec repository
  and can disappear; the copies make the contract target durable. Do not edit
  them; re-pin deliberately and update the contract in the same change.

All of these documents must be present on `custom` before Phase 1 is
assigned, so a worker starting from the product assembly finds the primary
document. Carry them on one docs topic branch and integrate it into `custom`;
do not leave planning documents only on report branches.

The interface contract links the official v1 specification, pinned v2 draft
and delivery RFD. Workers should use those references for standard details,
not infer wire shapes from this strategic summary. If implementation evidence
requires a contract change, update that document explicitly so the consumer
does not build to a different interpretation.

The consuming tool is Paseo (`camerontaylor/paseo`, branch `custom`, checkout
at `~/repos/paseo`). Today it drives jcode through its generic ACP client in
`packages/plugin/src/server/acp-internal/connection.ts` and calls no
`_jcode/*` extension. The dedicated provider plugin that consumes
`_jcode/turn` is on the unmerged branch `ct/jcode-provider-plugin`
(`plugins/jcode-provider/`). The contract lists the fixture locations; the
consumer fixtures track in this strategy means extending those.

## Architectural direction

The daemon owns turn identity, execution leases, authoritative history,
ordered publication, and atomic cancellation/insertion/withdrawal. These
guarantees must hold across TUI, several ACP processes and background activity.

The adapter owns ACP requests, version negotiation, connection-specific pending
requests, peer mirroring/adoption and wire presentation. Share that adapter
behavior between v1 and v2; do not build two session routers. The TUI remains
another daemon client, with minimal interface adaptations rather than ACP
policy embedded in its state.

Distinguish turn identity, message identity and submitting attachment. A steer
adds a message; adoption changes how a view follows work. Neither transfers
the turn's execution ownership. A connection-local request ID is not a global
message or turn identity.

Prefer additive, upstreamable core changes. The boundary report's approximate
20-line hook guideline is a preference, not a correctness constraint. A small
core primitive that makes cross-client behavior atomic is preferable to
adapter timers, duplicated ownership or several competing queues.

Do not minimize line count by deleting capabilities. In particular, preserve
model/effort selection, skills, commands, subagent launch, replay and peer
control while relocating policy. Do not introduce a generic plugin framework,
a new service or a large crate reorganization merely to enforce this boundary.

## Execution order and dependencies

| Phase | Deliverable | Dependency | Completion evidence |
| --- | --- | --- | --- |
| 0 | Verification baseline: CI on `custom`, budget decision, documents integrated | None | Quality and e2e jobs green on the `custom` head; planning documents present on `custom` |
| 1 | Reliable existing shared control | Phase 0 | Race regressions and current ACP/TUI scenarios pass in CI on `custom` |
| 2 | Reduced lifecycle/catalog core carry | Phase 1 for lifecycle removal | Same external behavior with less upstream-owned code changed |
| 3 | Useful v1 mappings and discovery | Stable affected paths; low-risk mapping work can overlap 1–2 | Existing clients remain compatible; new data is usable |
| 4 | Shared adapter with version-specific presentation; SDK decision | Repaired v1 baseline; SDK experiment can start immediately | v1 compatibility preserved with a measured dependency decision |
| 5 | Negotiated v2, then explicit delivery | Phase 4 presentation boundary, not necessarily SDK adoption | Contract scenarios work across v1, v2 and TUI |
| 6 | Optional expansion | Phases 1–5 and an actual consumer need | Each feature has a defined consumer and maintenance benefit |

Default to sequential integration. Independent investigation, isolated SDK
experiments and consumer fixtures can proceed alongside it. R2's catalog facade
does not depend on R1's lifecycle removal. Minor R3–R6 cleanups need not block
v2 if they are behaviorally independent. Upstream acceptance is never a
dependency for local progress.

## Phase 0 Establish the verification baseline

Every later phase cites "CI on the exact head" as its authority. That
authority does not exist yet, so this phase creates it before any behavior
change is assigned. It is small, mechanical and fork-owned.

1. **Run the quality and e2e jobs on `custom`.** Upstream `ci.yml` has no
   `workflow_call` entry point and `custom` carries no diff to it, so the
   choice is between a one-line trigger change (`push.branches` gains
   `custom`) carried as a fork hunk, or a fork-owned workflow that copies the
   `quality` job and the ubuntu leg of the `build` job, which contains the
   `provider_matrix` and `e2e` steps. Prefer the fork-owned workflow: it keeps
   `ci.yml` identical to upstream and lets the fork drop jobs it does not
   need (Windows, macOS, TypeScript SDK, installer evaluation). The jobs use
   no repository secrets. Expect roughly fifteen to thirty minutes warm.
2. **Decide the size-budget policy.** The base tag already fails both
   ratchets, so the fork cannot simply pass upstream's check. Fork-caused
   code failures today are `src/cli/acp.rs` (2319 to 3444 lines), the new
   `src/cli/acp/daemon_router.rs` (1617), and growth in
   `server/client_actions.rs`, `server/client_state.rs`, TUI `remote.rs`,
   `remote/server_events.rs` and `src/cli/provider_init.rs`; test failures are
   the new ACP and `client_actions` test files plus growth in three TUI and
   provider test files. The policy is:
   - The fork workflow runs the budget scripts against a fork-owned baseline
     file so pre-existing upstream debt does not block the fork. Carry that
     baseline on the CI topic branch, not in upstream's `code_size_budget.json`.
   - Fork-owned oversized files are debt the fork pays itself: split
     `src/cli/acp.rs` into transport, session and presentation modules as the
     first step of Phase 4's version boundary, and split `daemon_router.rs`
     when R1 lands. Do not ratchet those files upward again.
   - Core files that grew only slightly under fork hunks are tolerated in the
     fork baseline until the corresponding reshaping item (R1 to R6) or
     upstream acceptance removes the hunk.
3. **Integrate the planning documents into `custom`** as described in the
   document map, including the pinned references.
4. **Record the baseline.** The first green run on `custom` is the reference
   for every later "scenarios still pass" claim. Until it exists, workers may
   only claim named local test results.

Completion means the fork workflow is green on the `custom` head, with the
budget baseline committed and the documents present. Nothing else in this
strategy starts before that, except the SDK trial and consumer fixtures,
which do not depend on it.

## Phase 1 Preserve and repair shared control

First establish what the existing fork already does, using current tests and
the latest deviations in the shared-session document. Reconcile actual branch
state before editing: reports name historical commits, not necessarily the
worker's current head.

Preserve these user journeys:

- Attach to a live TUI/ACP session, replay stored history and observe peer work.
- Steer into owned or peer work, including adoption and stranded-input recovery.
- Stop the displayed turn from either interface and immediately submit again.
- Close an observing view without stopping the peer's turn.
- Select models/effort, use skills and commands, compact and launch subagents.

Implement D1 and D2 before structural lifecycle cleanup:

1. Make turn-scoped cancellation's identity check and effect atomic relative
   to turn replacement. A successful predicate check followed by a later
   session-wide signal is insufficient.
2. Capture cancellation epoch state at the turn claim, so clearing an earlier
   cancellation cannot erase a cancellation aimed at the new turn.
3. Verify terminal publication for normal completion, failure, panic and
   producer abort. Every observer must leave running state correctly.
4. Verify mixed-version event delivery. D3 disappears with R1; if R1 is delayed,
   prevent unsupported lifecycle variants reaching identity-aware clients that
   did not advertise support for those particular events.

Start in `crates/jcode-app-core/src/server/client_processing.rs`,
`server/client_lifecycle.rs`, `server/state.rs`, `agent/interrupts.rs` and the
existing ownership/publication primitives. Keep ownership in the daemon.

Completion means dispatch-level race tests and shared-session behavior pass,
not merely that a cancellation predicate is unit-tested. Exercise cancellation
during replacement, stop followed by prompt, two steering producers and
disconnect on either side of insertion. Do not alter v1 Stop semantics here.

Those races need a deterministic interleaving, not timing. A small test-only
seam in core is an authorized core change for this phase: for example a
`cfg(test)` or test-feature yield point between the cancellation predicate
and its signal, or a hook the mock-provider harness can block on at the turn
claim. Prefer one seam reused by several tests over per-test sleeps. A test
that passes only because of a sleep does not count as completion evidence.

## Phase 2 Reduce expensive core changes

### R1 Remove the competing lifecycle safely

Use upstream scoped terminal events and publication leases as the single
lifecycle foundation. Derive ACP lifecycle presentation in
`src/cli/acp/daemon_router.rs` and related adapter code.

The prerequisite is stronger than guaranteeing that some terminal arrives.
The inspected abort path sends `TurnEnded(Cancelled)` followed by `Done`.
Deleting the former without preserving its information can turn cancellation
into apparent success. Preserve authoritative completion/failure/cancellation
classification, output-before-terminal ordering and attachment snapshots
during finalization. Keep `_jcode/turn` and v1 stop reasons compatible.

If ordinary scoped events lack necessary information, add a minimal generic
terminal refinement in core. Do not guess status from a timeout or from the
fact that this particular adapter requested a stop. Remove duplicate lifecycle
state only after equivalent behavior is demonstrated; retiring redundant tests
must not retire the behavioral coverage.

### R2 Replace catalog relocation with a facade

Expose the stored-session catalog through a small facade in
`jcode-harness-api-server`, preserving `session/list`. Prefer this to carrying
a large relocation of upstream implementation. An upstream-accepted extraction
is also reasonable, but do not wait for it to deliver the facade.

### R3 through R6 Preserve features while relocating policy

Move model-startup coordination out of TUI/startup hotspots, reduce picker
policy to a narrow interface, move ACP-specific effort rounding into the
adapter, and move subagent translation/audience policy outside core where
practical. Execution leases and shared ordering remain in core.

Prioritize changes by actual conflict burden and implementation simplicity,
not estimated lines saved alone. Conflict burden is measured, not estimated:
the per-file conflict counts recorded at each base bump (see the maintenance
section) decide whether an R3 to R6 item is worth doing now. Until a bump
shows recurring conflicts in a file, its reshaping item is opportunistic
work for whoever is already changing that file, not a scheduled phase
deliverable. R1 and R2 are scheduled regardless, because R1 removes a
competing lifecycle and R2 removes a 1,200-line relocation of upstream code.
Do not drop TUI picker behavior merely because
the report lists removal as an alternative. Do not bundle contentious model
resolution policy changes into an ACP maintenance refactor.

Completion means the relevant behavior remains equivalent and the core diff
is smaller or simpler to carry. The historical estimate of 800–900 remaining
core lines is not a target that overrides correctness.

## Phase 3 Improve the existing v1 experience

Deliver information the daemon already has: exposed reasoning, todos/plans,
tool names and reliable locations/diffs. Do not synthesize structured metadata
by guessing from display strings. Preserve existing usage and config updates.

Add the interface contract's capability discovery in every profile, using
effective daemon support rather than executable version.

Keep tolerating client-supplied `mcpServers`. Revision 1 proposed rejecting
nonempty input; that reverses upstream commit `0517f01f2`, which made session
creation tolerate the field because real clients send their configured
servers to every agent (issue 887, reported from Zed), and it would break
those clients at `session/new`. Paseo is one of them: it sends the session's
configured servers on every new/load. There is no standard field that lets
an agent opt out, because ACP v1 makes stdio MCP support mandatory and the
`mcpCapabilities` booleans only cover HTTP and SSE. So: keep accepting the
field, state the limitation in the jcode capability metadata, and report
ignored entries through a session notice rather than an error. Retain
daemon-configured MCP tools. Actually honoring session-scoped stdio servers
is a Phase 6 candidate with a real consumer need, not a Phase 3 mapping.
The contract specifies the metadata and notice shape.

Implement `_jcode/detach` as the distinct operation for leaving inserted work
running. Keep existing v1 close owner-aware and keep its Stop behavior intact.
Detach must settle local waits and remove only the departing attachment's
pending input; it must not manufacture a global idle transition.

The interface contract deliberately narrows several recommendations in the
gap report:

- Retain legacy v1 model/effort method aliases; new consumers use config options.
- Defer own-message echoes to v2, avoiding duplicate display in current clients.
- Do not build a second explicit-delivery protocol for v1.
- Do not make v2 error codes a dependency for consumers using v1.

Per-request withdrawal is part of v2 in the current contract. If an immediate
v1 consumer need emerges, specify that extension before implementing it rather
than adding an undocumented behavior mode.

Completion means existing v1 clients still work and the new client can discover
and consume the improvements. Each mapping can land independently; broad
structural cleanup is not a prerequisite for a small, reliable mapping.

## Phase 4 Establish the version boundary and decide on the SDK

Separate transport/schema handling from shared session behavior just enough
to support two presentations. Avoid a speculative abstraction framework.
Keep the daemon router, turn tracking, adoption, command translation and
stranded-steer handling shared.

Prefer the official SDK if a bounded compatibility trial supports it. It can
reduce protocol plumbing, but it does not replace shared-session semantics.
Its adoption is not a prerequisite for v2 or a reason to rewrite working logic.

The trial starts independently of phases 1–3 in an isolated experimental
checkout. Its deliverables are a representative prototype, compatibility
findings, build measurements and an adopt/defer recommendation. Do not let it
grow into a parallel production adapter. Integrate only after rechecking
against the repaired v1 baseline, in a change separate from lifecycle and
v2 behavior changes.

Use the detailed matrix in
[SDK trial and build impact](ACP_FORK_INTERFACE.md#sdk-trial-and-build-impact):

- Compare current implementation, SDK v1 and SDK v1+v2 under identical toolchain,
  host, target, profile, feature and cache conditions.
- Measure cold compilation, warm no-op, ACP edits and unrelated code edits,
  including link time, peak memory, disk use and production artifact sizes.
- Inspect dependencies, supported targets and Cargo feature unification.
- Check `serde_json/preserve_order` beyond ACP: map iteration, serialized
  provider requests and cache-sensitive bytes. A separate crate in the same
  binary does not isolate unified features.
- Exercise concurrent requests, extension dispatch, parser leniencies and
  current consumer traffic. A compilation-only prototype is insufficient.

No build-cost reason was recorded in the original adapter commit; treat that
explanation as a hypothesis. Prefer measured daily development impact over a
single clean-build timing. If the dependency cost or compatibility work is
disproportionate, defer adoption and continue with the hand-written transport.
Do not create a sidecar merely to accommodate the SDK.

The trial is time-boxed. Its adopt/defer recommendation is due before the
Phase 4 module split begins, so the split is designed for the transport that
will actually be used. If the evidence is not complete by then, the default
is **defer**: Phase 4 proceeds on the hand-written transport and the trial
either closes or continues as research with no claim on the schedule. Fix
the benchmark host and toolchain at the start of the trial and keep them for
every measurement; a comparison across hosts is not a result.

## Phase 5 Add v2 and then explicit delivery

### First deliver the complete advertised baseline

Implement per-connection negotiation, insertion responses, own-user echoes,
standard state updates, replay/upserts, correct replacement semantics and
version-specific errors/config shapes. Use a rollout switch while the draft is
unstable. Do not advertise a baseline method set whose mandatory behavior is
only partly implemented.

The switch follows the existing ACP profile convention rather than adding a
CLI flag: a new `[acp]` configuration key with a `JCODE_ACP_*` environment
override, appended to the end of the `AcpConfig` struct so the core hunk is
additive. Off means the adapter negotiates v1 only, exactly as today. On means
v2 is offered during negotiation and v1 remains available. The contract names
the key.

Keep v1 operating alongside v2. V1 `_jcode/turn` remains its peer-view contract;
v2 uses standard state updates plus the interface contract's informational
origin/turn metadata. Do not run two competing busy/idle authorities in v2.

Preserve safe detachment explicitly. Revision 1 made v2 `session/close` stop
all session work, including a peer's turn, and relied on every client calling
`_jcode/detach` instead. Paseo can learn that; a third-party v2 client cannot,
and it will issue close on ordinary window closure. That would let any
standard client stop a TUI user's turn, which contradicts the Phase 1
journey "close an observing view without stopping the peer's turn".

The amended rule, specified in the contract: v2 close is **attachment-scoped
when peers are present**. If other live attachments exist, close cancels only
work this attachment originated, withdraws its pending input, sends its final
updates and idle state, and detaches; adopted or peer-origin turns continue.
If this is the sole live attachment, close applies the standard v2 semantics
in full. This is a deliberate, documented deviation, consistent with the
owner-aware v1 close. `_jcode/detach` remains the explicit "leave without
stopping anything" operation. Paseo still distinguishes leaving a view from
stopping and closing the session; the deviation protects clients that do not.

### Then deliver steer and queue

Implement the pinned delivery RFD directly, with advertised `steer` and `queue`.
Do not implement the abandoned `session/inject` design or an interim `_meta`
delivery API. Ordinary busy v2 prompts use the holding behavior specified by
the interface contract; they do not inherit v1 implicit steering.

Separate Stop, withdrawal, detach and close. Pending explicit-delivery messages
survive a turn stop according to the RFD; withdrawing a request removes that
message before insertion. Closing/detaching have their separately defined
effects. Preserve these distinctions across legacy v1/TUI cancellation too.

Reuse or minimally extend daemon scheduling so several adapters do not race
after observing idle. Establish one authoritative receipt/insertion order.
Keep generic scheduling primitives in core and ACP semantics in the adapter.
Do not build durable queued jobs or promise exactly-once recovery after an
ambiguous disconnect; those are outside this contract.

Completion means the interface contract's acceptance scenarios pass against
both the daemon and the consuming tool, including mixed-version peers,
withdrawal/insertion races, tool-safe steering, turn-boundary queue behavior,
detach/close and replay. Existing capabilities must remain available without
requiring v1 consumers to migrate immediately.

## Phase 6 Defer optional breadth

Only after the preceding capabilities are maintained and usable, evaluate new
auth flows, client filesystem/terminal execution, elicitation, per-tool cancel,
session fork/delete/archive and structured subagent/provider APIs.

Prioritize an actual consuming-tool need, not schema completeness. Existing
subagent launch remains supported without obligating the fork to implement
the experimental structured subagent protocol. Permission integration moves
earlier only if a real workflow cannot complete using existing daemon-side
approvals; specify that scope explicitly instead of implying it is already
supported.

## Integration and verification practice

Use the existing Desvio assembly model as it is now, not as the boundary
report describes it. Inspect `~/.jcode-fork/manifest.txt` and
`~/.jcode-fork/README.md` before changing carries; the report branch itself
is not the product assembly. The current model:

- `custom` is the retained fork baseline. It already contains the former
  `pr/*` stack and the `fork/*` feature branches. New work is a topic branch
  appended to the manifest after `custom`; once deliberately integrated into
  `custom`, its manifest line is removed.
- `mine` is the disposable generated assembly. Do not develop in its tree.
- The `pr/*` branches still exist as upstream candidates. Maintain them as
  **merge-forward branches**: fix D1 and D2 as new commits on the relevant
  `pr/*` branch, then merge that branch into `custom` (or append it as a carry
  and integrate). Never rebase a `pr/*` branch that `custom` has merged; a
  rebase turns the next integration into a full conflict replay. When a
  branch is submitted upstream, make a separately named rebased copy for the
  pull request and keep the merge-forward branch as the fork's record.
- Each reshaping item (R1 to R6) and each phase slice is its own topic
  branch, gated with `jcode-desvio build --bisect-gate` and then integrated.

Preserve unrelated work. Keep fixes, structural refactors, protocol behavior
and dependency migration in separate reviewable changes. Existing
authorizations and branch ownership rules govern publishing. A clean merge or
successful build is not runtime proof.

Build on existing verification rather than adding a new approval system:

- Adapter JSON-shape tests: `src/cli/acp/tests.rs`.
- Router lifecycle tests: `src/cli/acp/daemon_router_tests.rs`.
- Configuration tests: `src/cli/acp/session_configuration.rs`.
- Shared-session behavior: `tests/e2e/acp_shared_session.rs` and existing
  daemon ownership/cancellation/publication tests.
- Consumer fixtures for the target contract, so each side can develop before
  the other implementation lands.

Workers select named tests for their changed behavior, follow host memory and
`nice` guidance, and leave full suites/heavy builds to appropriate hosts or CI.
The fork workflow from Phase 0, run on the exact `custom` head, is the full
validation authority; until it is green there is no authority and claims are
limited to named local tests. For runtime checks use the
intended binary and a dedicated socket: the shared daemon otherwise silently
tests installed code rather than the new build.

For every completed slice, report the behavior changed, tests/scenarios run,
remaining limitations and any contract amendment. Documentation-only planning
does not establish a passing runtime baseline.

## Upstream and long-term maintenance

Offer independent core correctness fixes upstream as soon as they are ready.
The boundary report's upstream sequence is a useful starting point: terminal
publication, scoped cancellation events, stored history indices, repaired
cancellation primitives, idle steering announcement and slimmed turn origin.
Each contribution needs its own tests/helpers rather than accidental dependence
on an unmerged predecessor. Carry duplicate idle-announcement work only once.

Remove a carry after its behavior is actually present in the selected upstream
base, not merely because a PR has been accepted elsewhere. Revalidate the
assembled fork and preserve external ACP behavior through upstream changes.

### Moving the base

The pinned base will fall behind quickly. Move it deliberately rather than
continuously:

- **Triggers.** Bump at a phase boundary, or earlier when upstream lands a
  turn-lifecycle or ACP change the fork needs (a fix, or a change that would
  otherwise be duplicated by the next phase). Do not bump in the middle of a
  phase slice.
- **Procedure.** A bump is its own change: update `desvio.conf`, retrain
  rerere, reassemble with the bisect gate, run the fork workflow, and smoke
  the ACP shared-session scenarios on a dedicated socket. No behavior change
  rides along with a bump.
- **Measure the cost.** Keep the per-file conflict counts from each bump's
  assembly log under `~/.jcode-fork/state/`. This is the evidence the
  reshaping items and the freeze decision below depend on; without it, both
  are guesswork. It is a measurement, not a gate.
- **ACP is fork-owned, not upstream-frozen.** Upstream still fixes ACP bugs in
  `src/cli/acp.rs`, and the fork's copy diverges from the base by roughly
  1,700 added lines, so each such fix is a conflict. Reduce that surface by
  keeping fork-only logic in fork-only files (the Phase 4 split does this)
  and by reviewing upstream ACP commits at every bump for behavior the fork
  should port.

Judge maintainability by recurring conflicts in core hotspots, duplicated
state, testability and client compatibility. Raw changed-line counts are a
secondary measure. If upstream lifecycle drift makes following it uneconomic,
freeze the base temporarily and select upstream changes deliberately; do not
create another competing turn model to keep rebasing at any cost. Make that
call from the recorded conflict counts, not from a single painful bump.

The first implementation assignment is phase 0, then phase 1, with the SDK experiment and
consumer contract fixtures available as independent tracks. Finish each useful
slice before expanding scope; SDK adoption, upstream acceptance and optional
features must not hold existing ACP/TUI reliability hostage.
