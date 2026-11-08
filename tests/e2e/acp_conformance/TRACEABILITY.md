# ACP v1 traceability

Clause IDs are stable references; adjacent paragraphs and list items remain
part of the cited rule. This index is a work in progress, not a claim of full
normative coverage. Names below are Rust test function names in the e2e target.

| Clause | Tests |
| --- | --- |
| C-TRANSPORT-1 | acp_conformance_framing_ids_and_errors |
| C-PROFILE-1, C-CAPS-1 | acp_capability_metadata_identifies_each_attachment |
| C-REPLAY-1 | acp_conformance_create_list_load |
| C-STOP-1 | acp_conformance_shared_observe_steer, acp_conformance_interleavings |
| C-CLOSE-1 | acp_conformance_close_owner_aware_v1 |
| C-DETACH-1, C-DETACH-2 | acp_conformance_detach_originator, acp_conformance_detach_observer, acp_conformance_detach_adopted_steer, acp_conformance_detach_withdraws_pending_steer |
| C-DETACH-3 | acp_conformance_detach_params_and_idempotency |
| C-STEER-1, C-STEER-2 | acp_conformance_shared_observe_steer |
| C-CORRELATION-1 | acp_conformance_interleavings, acp_conformance_config_commands_v1 |
| C-CORRELATION-2, C-BUSY-1, C-BUSY-2 | acp_conformance_negotiation_legacy_v1 |
| C-PEER-1, C-PEER-2, C-PEER-3, C-PEER-4 | acp_conformance_interleavings, acp_conformance_observer_close_bracket_contract |
| C-PEER-5 | acp_client_mirrors_peer_turns_and_keeps_its_own_prompt_separate |
| C-CONFIG-1 | acp_conformance_config_commands_v1, acp_conformance_config_peer_update_contract |
| C-SKILL-1 | acp_skill_clear_round_trip |
| C-CORPUS-1 | acp_conformance_create_list_load, acp_conformance_shared_observe_steer, acp_conformance_close_owner_aware_v1, acp_conformance_config_commands_v1, acp_conformance_negotiation_legacy_v1 |

## Open interpretations (proposals, not amendments)

- Prompt results and “own text”: v1 returns stopReason and correlation; output
  arrives through session/update. Proposal: distinguish request settlement
  from output ownership explicitly; multiple steers share subsequent output.
- Replay grouping does not explicitly forbid duplicate first messages.
  Live QA reports a duplicate first user message. Proposal: each stored user
  message appears exactly once in each complete replay, in insertion order.
- Peer user updates during an owned turn: the contract requires other
  attachments to receive peer-user updates; live QA reports reverse steer text
  missing in the owner's ACP view. Proposal: require each inserted peer steer
  to emit one user_message_chunk before the output it influences, including
  on the turn owner's connection.
- Stop permission changes during observation: no snapshot boundary is named.
  Existing legacy tests refuse unsupported peer control; the live QA run
  advertised stopObservedTurn=true yet continued execution. Proposal: evaluate
  permission and observed turn identity atomically when processing cancel;
  false permits only withdrawal of the caller's waiting input.

## Live QA defects awaiting deterministic reproductions

Evidence: atlas `.cache/paseo-jcode-e2e/REPORT.md`, 2026-11-08, baseline
0f9a3e5de. Fix lane: `fork/acp-e2e-defects`.

- QA-STOP: observed peer Stop returns idle while execution continues.
- QA-STEER: peer steer into own turn missing from ACP user updates.
- QA-REPLAY: first user message duplicated on load.

Ignored targets: `acp_known_defect_stop_observed_peer`,
`acp_known_defect_reverse_steer_is_mirrored`, and
`acp_known_defect_replay_first_user_once`. We assert intended behavior rather
than enshrining the wrong behavior in a passing companion. These probes still
need verification against the live QA reproductions. In particular,
a provider-stream cancellation case does not reproduce a tool-wait cancel.
Do not treat passing provider-only scenarios as disproving the QA evidence.

## Interleaving scope

`acp_conformance_interleavings` exhaustively enumerates 12 short sequences
(two owners, three terminal actions, two attachment timings). No random seeds
are used. It verifies one non-nested lifecycle pair, matching terminal identity,
origin response correlation, own output and request settlement. It does not yet
enumerate steer, detach, load replay or successor cancellation interleavings.
