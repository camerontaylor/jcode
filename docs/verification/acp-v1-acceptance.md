# ACP v1 acceptance reconciliation

## Scope and provenance

This record reconciles the original Phase 0 through Phase 3 target against
integrated code and executable evidence. Historical planning prose is not a
list of missing implementation. The v2 track and optional features are not
part of v1 acceptance.

Starting source: `1bdb614590763855b85919fda839903457149e51`.
The `custom` merge, `6766a2ed419721594b62c12bb92dbc2c09b3df6a`, has an
identical tree (`git diff 1bdb61459 6766a2ed4` is empty).

Verified starting-head Fork CI:

- `42738049168`, pull request event, source `1bdb614590763855b85919fda839903457149e51`.
- `42740276146`, push event, source `6766a2ed419721594b62c12bb92dbc2c09b3df6a`.
- Both Quality Guardrails and Build & Test passed on each run.
- Run `42738049168` executed 143 ACP adapter tests, one separately selected
  private-daemon reexec test, and 100 e2e tests. One adapter test and seven
  e2e tests were ignored. These counts are not a claim that the ignored live
  provider and scale tests passed.

The acceptance repair/fixture head
`5fa1db619a52084128be56a15a024b3e831325a1` also passed both jobs in
Fork CI `42907124856` and `42907113965`. These runs include the newly selected
deterministic regressions, the Desktop capability repair and the skill-clear
fixture. Later changes, including the command workflow tests below, require
their own exact-head CI results. None of these agent-side runs certifies an
installed consumer release.

## Original criteria and existing evidence

The named tests below are existing evidence anchors. Consumer results are
reported separately, because an agent-side wire test cannot establish the
consumer's resulting state.

| Original criterion | Code-owned evidence anchor | Reconciliation |
| --- | --- | --- |
| Phase 0: fork quality and e2e workflow | `.github/workflows/fork-ci.yml`, runs above | Existing implementation, green starting-head CI |
| Phase 0: fork-owned budget baseline | `scripts/fork/`, Quality Guardrails | Existing implementation, green starting-head CI |
| Phase 0: contracts and pinned references | `docs/ACP_FORK_INTERFACE.md`, `docs/FORK_BOUNDARY_STRATEGY.md` | Existing documents, planning maintained in its companion |
| Phase 0: record a green `custom` baseline | Run `42740276146` | Verified on the exact starting `custom` head |
| Phase 1: attach, replay and peer observation | `acp_conformance_create_list_load`, `acp_conformance_interleavings`, `acp_client_mirrors_peer_turns_and_keeps_its_own_prompt_separate` | Passed in starting-head e2e CI |
| Phase 1: steer, adopt and recover stranded input | `acp_conformance_shared_observe_steer`, `acp_conformance_seeded_three_client_invariants`, retirement tests | Passed in starting-head e2e CI |
| Phase 1: stop then immediately prompt | `acp_conformance_interleavings`, `acp_conformance_stop_observed_bash_wait` | Passed in starting-head e2e CI |
| Phase 1: close an observing view without stopping work | `acp_conformance_close_owner_aware_v1`, `acp_conformance_observer_close_bracket_contract` | Passed in starting-head e2e CI |
| Phase 1: model, effort, skills and command discovery | `acp_conformance_config_commands_v1`, `acp_conformance_config_peer_update_contract`, `acp_skill_clear_round_trip` | Passed in starting-head e2e CI, consumer controls need separate evidence |
| Phase 1: compact history and launch a live subagent | `acp_conformance_compact_command_compacts_real_history`, `acp_conformance_subagent_command_spawns_live_worker` | Added actual daemon-workflow tests, named local run passed; final-head CI required |
| Phase 1 D1: atomic cancellation relative to replacement | `cancellation_dispatch_cannot_signal_a_successor`, `completed_turn_is_not_reported_interrupted_by_a_late_stop`, `late_stop_preserves_task_for_completion_reconciliation` | Existing deterministic regressions, added to Fork CI in this acceptance slice |
| Phase 1 D2: capture cancellation epoch at claim | `cancel_after_turn_claim_is_preserved` | Existing deterministic regression, added to Fork CI in this acceptance slice |
| Phase 1: completion, failure, panic and producer abort | `turn_event_channel_tests`, `acp_conformance_panicking_producer_is_failed`, `acp_conformance_aborted_producer_after_stop_is_cancelled` | Wire tests passed, deterministic publication regressions added to Fork CI |
| Phase 1: two steering producers and disconnect around insertion | `two_steering_producers_withdraw_on_either_side_of_insertion`, `acp_conformance_transport_loss_withdraws_waiting_steer`, `acp_conformance_transport_loss_preserves_inserted_work` | Socket regressions passed, deterministic core regression added to Fork CI |
| Phase 1: truthful initialize and attachment capability metadata | `initialize_advertises_jcode_capability_meta_in_every_profile`, `acp_capability_metadata_identifies_each_attachment`, `acp_conformance_negotiation_legacy_v1` | Passed in starting-head CI |
| Phase 1: mixed-version lifecycle delivery | `undecodable_lines_are_skipped_and_never_end_the_session`, `obsolete_turn_ended_is_not_a_daemon_wire_event` | R1 removed the competing event, adapter compatibility passed, protocol regressions added to Fork CI |
| Phase 2 R1: one authoritative lifecycle, compatible classifications | `docs/verification/r1-single-lifecycle.md`, classified scoped terminals in `wire/events.rs` and the ACP router | Integrated already, no `TurnEnded` daemon variant or duplicate retirement flag remains |
| Phase 2 R2: stored-session catalog facade | `translate/session_catalog.rs`, `acp_conformance_list_metadata_and_pagination` | Integrated facade, wire behavior passed, catalog regressions added to Fork CI |
| Phase 2 R3 through R6 | Fork boundary strategy | Opportunistic, not mandatory phase deliverables without measured recurring conflict burden |
| Phase 3: structured reasoning, plans and tool details | `acp_conformance_maps_reasoning_tool_calls_and_plan`, `event_mapper_reports_no_diff_for_a_failed_edit` | Passed in starting-head CI |
| Phase 3: prompt turn ID and role correlation | `acp_conformance_shared_observe_steer`, `acp_conformance_interleavings`, config-command non-turn result | Passed in starting-head e2e CI, consumer deduplication requires separate evidence |
| Phase 3: attachment-local skill selection and `skill: null` clear | `acp_skill_clear_round_trip`, ACP skill metadata unit tests | Passed in starting-head CI |
| Phase 3: shared transcript corpus with drift detection | `tests/fixtures/acp/`, `tests/e2e/test_support/acp_fixture.rs` | Agent-side replay passed, consumer-side replay requires separate evidence |
| Phase 3: tolerate session MCP servers, advertise limitation and notice | `acp_ignored_mcp_servers_notice_follows_the_response` | Passed in starting-head e2e CI |
| Phase 3: detach preserves work and withdraws only departing pending input | `acp_conformance_detach_originator`, `acp_conformance_detach_observer`, `acp_conformance_detach_adopted_steer`, `acp_conformance_detach_withdraws_pending_steer` | Passed in starting-head e2e CI |
| Phase 3: legacy setters and legacy clients | `acp_conformance_config_commands_v1`, `acp_conformance_negotiation_legacy_v1` | Passed in starting-head e2e CI |
| Phase 3 completion: Paseo fixture replay and real build on dedicated socket | Consumer acceptance results below | Not established by the starting-head agent CI alone |

## Gaps closed by this acceptance slice

The starting workflow executed adapter and wire tests but did not execute the
original deterministic app-core cancellation and publication regressions.
The existing tests are now selected explicitly in Fork CI. This closes a
verification omission, not a missing production implementation. The filters
cover the turn event channel, cancellation at claim, turn ownership, protocol
turn identity and the stored-session catalog facade.

The acceptance run also exposed a real Desktop response-tool allow-list defect.
The explicit UI capability registered `user_response`, but the turn tool filter
then hid it. Commit `c92bdffad54e2a360db2591b6ce6cc70df97e308` preserves that
capability through both definition filtering and dispatch validation, without
overriding an explicit SDK enabled/disabled tool list. The exact named
`user_response_socket_registration_follows_explicit_capability` regression
passed after repair and is selected in Fork CI.

Commit `5fa1db619a52084128be56a15a024b3e831325a1` adds the real-wire
`12-skill-selection-clear-v1.jsonl` transcript. The skill round-trip test
passed both recording and normal replay. The named corpus-integrity test
passed across all 12 transcript files, checking direction, contiguous sequence,
JSON-RPC shape, paired requests/responses and an executable replay reference
for every file. This is agent-side evidence, not consumer replay evidence.

The new `commands_v1.rs` tests close a separate evidence gap: command
advertisement and parsing did not establish actual daemon scheduling. The
compaction test now witnesses a real summarization call and proves that the
next provider request replaces older history with that summary while retaining
recent turns. The subagent test separates the launch tool receipt from the
spawned child's mid-turn attach, gated live output and completed terminal.
The named `commands_v1` filter passed all three selected tests (the two new
workflows plus the existing config-command regression). This is mock-provider
workflow evidence, not a claim about provider-native compaction, visible-window
spawning or a paid external model.

The historical D3-pending notes in `p1-cancel-race.md` are superseded by the
integrated R1 result in `r1-single-lifecycle.md`. Keeping the shared
`TurnEndStatus` enum as classified scoped-terminal metadata is intentional,
not a second lifecycle or unfinished removal of `TurnEnded`.

## Consumer and final-head acceptance

In progress. Do not infer completed Phase 3 acceptance, consumer release
acceptance, or an opened v2 gate from the starting-head results above.
