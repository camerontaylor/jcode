# ACP v1 traceability

Clause IDs are stable references; adjacent paragraphs and list items remain
part of the cited rule. Paragraph IDs name clause groups; `-I` and `-R` IDs identify individual
list items and table rows. V2 halves of mixed clauses remain deferred. Names below are Rust test function names in the e2e target.

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


## Remaining clause inventory

All contract prose, normative lists and tables have IDs. Introductory/source
material is indexed for completeness; it is distinguished from wire obligations.
`unit:` names run in the targeted ACP library step in Fork CI. Consumer/process
obligations cannot be asserted by an agent-side wire test; deferred v2 and SDK
clauses require no v1 implementation. A mapped test pins the adapter-facing
behavior of the clause, not a browser/client UI promise.

| Clause | Scope and executable evidence |
| --- | --- |
| C-OTHER-P01 | Context / source precedence / consumer release process |
| C-OTHER-P02 | Context / source precedence / consumer release process |
| C-OTHER-P03 | Context / source precedence / consumer release process |
| C-OTHER-P04 | Context / source precedence / consumer release process |
| C-SCOPE-P01 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P02 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-DISCOVERY-P01 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P02 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P03 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P04 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P05 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P06 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P07 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P08 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-OPERATIONS-P01 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P02 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-SHARED-P01 | acp_conformance_shared_observe_steer; acp_conformance_detach_withdraws_pending_steer; acp_conformance_seeded_three_client_invariants; acp_conformance_transport_loss_preserves_inserted_work; known-fail: acp_conformance_transport_loss_withdraws_waiting_steer |
| C-V2CLOSE-P01 | Deferred v2/SDK track; no v1 wire requirement |
| C-V2CLOSE-P02 | Deferred v2/SDK track; no v1 wire requirement |
| C-V2CLOSE-P03 | Deferred v2/SDK track; no v1 wire requirement |
| C-DETACH-P01 | acp_conformance_detach_params_and_idempotency; acp_conformance_detach_originator |
| C-CORRELATION-P01 | acp_conformance_shared_observe_steer; acp_conformance_config_commands_v1; consumer completion deduplication is consumer-side |
| C-CORRELATION-P02 | acp_conformance_shared_observe_steer; acp_conformance_config_commands_v1; consumer completion deduplication is consumer-side |
| C-PEER-P01 | acp_conformance_interleavings; acp_conformance_seeded_three_client_invariants |
| C-V2DELIVERY-P01 | Deferred v2/SDK track; no v1 wire requirement |
| C-V2DELIVERY-P02 | Deferred v2/SDK track; no v1 wire requirement |
| C-V2DELIVERY-P03 | Deferred v2/SDK track; no v1 wire requirement |
| C-V2DELIVERY-P04 | Deferred v2/SDK track; no v1 wire requirement |
| C-V2META-P01 | Deferred v2/SDK track; no v1 wire requirement |
| C-V2META-P02 | Deferred v2/SDK track; no v1 wire requirement |
| C-V2META-P03 | Deferred v2/SDK track; no v1 wire requirement |
| C-CONFIG-P01 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-CONFIG-P02 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-CONFIG-P03 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-CONFIG-P04 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-CONFIG-P05 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-CONFIG-P06 | Deferred v2 command insertion/echo |
| C-CONFIG-P07 | acp_skill_clear_round_trip; unit: skill_selection_reads_the_canonical_metadata_field |
| C-CONTENT-P01 | acp_conformance_maps_reasoning_tool_calls_and_plan; unit: prompt_from_params_accepts_text_images_and_resources, event_mapper_reports_no_diff_for_a_failed_edit; diagnostics: initialize_advertises_jcode_capability_meta_in_every_profile; client rendering is consumer-side |
| C-CONTENT-P02 | acp_conformance_maps_reasoning_tool_calls_and_plan; unit: prompt_from_params_accepts_text_images_and_resources, event_mapper_reports_no_diff_for_a_failed_edit; diagnostics: initialize_advertises_jcode_capability_meta_in_every_profile; client rendering is consumer-side |
| C-CONTENT-P03 | acp_conformance_maps_reasoning_tool_calls_and_plan; unit: prompt_from_params_accepts_text_images_and_resources, event_mapper_reports_no_diff_for_a_failed_edit; diagnostics: initialize_advertises_jcode_capability_meta_in_every_profile; client rendering is consumer-side |
| C-CONTENT-P04 | acp_conformance_maps_reasoning_tool_calls_and_plan; unit: prompt_from_params_accepts_text_images_and_resources, event_mapper_reports_no_diff_for_a_failed_edit; diagnostics: initialize_advertises_jcode_capability_meta_in_every_profile; client rendering is consumer-side |
| C-ELICIT-P01 | `acp_conformance_elicitation_form_v1`; `acp_conformance_elicitation_requires_capability_and_opt_in`; adapter `client_requests::tests` |
| C-ELICIT-P02 | `acp_conformance_elicitation_peer_winner_and_turn_cancel` (includes Desktop and late attach); broker `elicitation::tests` |
| C-ELICIT-P03 | `acp_conformance_elicitation_form_v1`; `applet_elicitation_capability_updates_locked_tool_surface`; `applet_requires_explicit_name_and_disabled_wins` |
| C-ELICIT-P04 | Consumer-side review/accept/decline/cancel UI remains consumer-owned; agent evidence: `acp_conformance_elicitation_requires_capability_and_opt_in`, `compiler_rejects_duplicates_credentials_and_unsupported_nodes`, `acp_conformance_elicitation_form_v1` |
| C-ERRORS-P01 | acp_conformance_framing_ids_and_errors; acp_ignored_mcp_servers_notice_follows_the_response; acp_conformance_negotiation_legacy_v1; unit: initialize_advertises_jcode_capability_meta_in_every_profile; authentication/approval UI is consumer-side |
| C-ERRORS-P02 | acp_conformance_framing_ids_and_errors; acp_ignored_mcp_servers_notice_follows_the_response; acp_conformance_negotiation_legacy_v1; unit: initialize_advertises_jcode_capability_meta_in_every_profile; authentication/approval UI is consumer-side |
| C-ERRORS-P03 | acp_conformance_framing_ids_and_errors; acp_ignored_mcp_servers_notice_follows_the_response; acp_conformance_negotiation_legacy_v1; unit: initialize_advertises_jcode_capability_meta_in_every_profile; authentication/approval UI is consumer-side |
| C-ERRORS-P04 | acp_conformance_framing_ids_and_errors; acp_ignored_mcp_servers_notice_follows_the_response; acp_conformance_negotiation_legacy_v1; unit: initialize_advertises_jcode_capability_meta_in_every_profile; authentication/approval UI is consumer-side |
| C-ERRORS-P05 | acp_conformance_framing_ids_and_errors; acp_ignored_mcp_servers_notice_follows_the_response; acp_conformance_negotiation_legacy_v1; unit: initialize_advertises_jcode_capability_meta_in_every_profile; authentication/approval UI is consumer-side |
| C-ACCEPTANCE-P01 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P01 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P02 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P03 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-SDK-P01 | Deferred v2/SDK track; no v1 wire requirement |
| C-SDK-P02 | Deferred v2/SDK track; no v1 wire requirement |
| C-SDK-P03 | Deferred v2/SDK track; no v1 wire requirement |
| C-SDK-P04 | Deferred v2/SDK track; no v1 wire requirement |
| C-SDK-P05 | Deferred v2/SDK track; no v1 wire requirement |
| C-REFERENCE-P01 | Context / source precedence / consumer release process |
| C-SOURCES-P01 | Context / source precedence / consumer release process |
| C-SOURCES-P02 | Context / source precedence / consumer release process |
| C-SOURCES-P02-I1 | Context / source precedence / consumer release process |
| C-SOURCES-P02-I2 | Context / source precedence / consumer release process |
| C-SOURCES-P02-I3 | Context / source precedence / consumer release process |
| C-SOURCES-P02-I4 | Context / source precedence / consumer release process |
| C-SOURCES-P02-I5 | Context / source precedence / consumer release process |
| C-SOURCES-P03 | Context / source precedence / consumer release process |
| C-SOURCES-P04 | Context / source precedence / consumer release process |
| C-STAGES-P01-R1 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R2 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R3 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R4 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R5 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R6 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R7 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R8 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R9 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R10 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-STAGES-P01-R11 | V1 scope: acp_capability_metadata_identifies_each_attachment; v2 roadmap deferred |
| C-DISCOVERY-P04-R1 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P04-R2 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P04-R3 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P04-R4 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P04-R5 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-DISCOVERY-P04-R6 | acp_conformance_negotiation_legacy_v1; acp_capability_metadata_identifies_each_attachment; unit: initialize_advertises_jcode_capability_meta_in_every_profile |
| C-OPERATIONS-P01-R1 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P01-R2 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P01-R3 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P01-R4 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P01-R5 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P01-R6 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P01-R7 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P01-R8 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P01-R9 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-OPERATIONS-P01-R10 | acp_conformance_create_list_load; acp_conformance_list_metadata_and_pagination; acp_conformance_close_owner_aware_v1; acp_conformance_config_commands_v1; UI/pagination cursor handling is consumer-side |
| C-SHARED-P01-R1 | acp_conformance_shared_observe_steer; acp_conformance_detach_withdraws_pending_steer; acp_conformance_seeded_three_client_invariants; acp_conformance_transport_loss_preserves_inserted_work; known-fail: acp_conformance_transport_loss_withdraws_waiting_steer |
| C-SHARED-P01-R2 | acp_conformance_shared_observe_steer; acp_conformance_detach_withdraws_pending_steer; acp_conformance_seeded_three_client_invariants; acp_conformance_transport_loss_preserves_inserted_work; known-fail: acp_conformance_transport_loss_withdraws_waiting_steer |
| C-SHARED-P01-R3 | acp_conformance_shared_observe_steer; acp_conformance_detach_withdraws_pending_steer; acp_conformance_seeded_three_client_invariants; acp_conformance_transport_loss_preserves_inserted_work; known-fail: acp_conformance_transport_loss_withdraws_waiting_steer |
| C-SHARED-P01-R4 | acp_conformance_shared_observe_steer; acp_conformance_detach_withdraws_pending_steer; acp_conformance_seeded_three_client_invariants; acp_conformance_transport_loss_preserves_inserted_work; known-fail: acp_conformance_transport_loss_withdraws_waiting_steer |
| C-CORRELATION-P01-I1 | acp_conformance_shared_observe_steer; acp_conformance_config_commands_v1; consumer completion deduplication is consumer-side |
| C-CORRELATION-P01-I2 | acp_conformance_shared_observe_steer; acp_conformance_config_commands_v1; consumer completion deduplication is consumer-side |
| C-CORRELATION-P01-I3 | acp_conformance_shared_observe_steer; acp_conformance_config_commands_v1; consumer completion deduplication is consumer-side |
| C-CORRELATION-P01-I4 | acp_conformance_shared_observe_steer; acp_conformance_config_commands_v1; consumer completion deduplication is consumer-side |
| C-CONFIG-P01-I1 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-CONFIG-P01-I2 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-CONFIG-P01-I3 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-CONFIG-P01-I4 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-CONFIG-P01-I5 | acp_conformance_config_commands_v1; acp_conformance_config_peer_update_contract; acp_conformance_busy_config_errors_are_atomic; acp_conformance_peer_config_serializes_after_active_turn; acp_skill_clear_round_trip; unit: advertised_commands_parse_to_real_dispatch_variants, unknown_escaped_and_path_like_slash_text_is_untouched, a_busy_session_rejects_an_explicit_skill_selection_before_insertion |
| C-ERRORS-P04-I1 | acp_conformance_framing_ids_and_errors; acp_ignored_mcp_servers_notice_follows_the_response; acp_conformance_negotiation_legacy_v1; unit: initialize_advertises_jcode_capability_meta_in_every_profile; authentication/approval UI is consumer-side |
| C-ERRORS-P04-I2 | acp_conformance_framing_ids_and_errors; acp_ignored_mcp_servers_notice_follows_the_response; acp_conformance_negotiation_legacy_v1; unit: initialize_advertises_jcode_capability_meta_in_every_profile; authentication/approval UI is consumer-side |
| C-ERRORS-P04-I3 | acp_conformance_framing_ids_and_errors; acp_ignored_mcp_servers_notice_follows_the_response; acp_conformance_negotiation_legacy_v1; unit: initialize_advertises_jcode_capability_meta_in_every_profile; authentication/approval UI is consumer-side |
| C-ERRORS-P04-I4 | acp_conformance_framing_ids_and_errors; acp_ignored_mcp_servers_notice_follows_the_response; acp_conformance_negotiation_legacy_v1; unit: initialize_advertises_jcode_capability_meta_in_every_profile; authentication/approval UI is consumer-side |
| C-CORPUS-P01-I1 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P01-I2 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P01-I3 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P01-I4 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P02-I1 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P02-I2 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P02-I3 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P02-I4 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P02-I5 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P02-I6 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P02-I7 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-CORPUS-P02-I8 | Corpus scenarios 01–05, 07–11 through acp_conformance tests; v2 scenario 06 deferred; consumer replay is Paseo-side |
| C-SDK-P03-R1 | Deferred v2/SDK track; no v1 wire requirement |
| C-SDK-P03-R2 | Deferred v2/SDK track; no v1 wire requirement |
| C-SDK-P03-R3 | Deferred v2/SDK track; no v1 wire requirement |
| C-SDK-P03-R4 | Deferred v2/SDK track; no v1 wire requirement |
| C-SDK-P03-R5 | Deferred v2/SDK track; no v1 wire requirement |
| C-SDK-P03-R6 | Deferred v2/SDK track; no v1 wire requirement |

## Ambiguities and proposed wording (owner approval pending)

1. **Prompt result versus own output.** Observed: v1 responses contain stopReason,
   usage and correlation; text arrives in session/update. Steers share subsequent
   turn output. Proposal: “Only updates from the correlated turn belong to a
   prompt wait; settling a steer does not create another copy of turn output.”
2. **Repeated load during an active observation.** Observed: two starts without
   an intervening terminal (ACP-LOAD-BRACKET). Proposal: “Reloading an existing
   attachment must reuse the open bracket, or close it before opening another;
   replay precedes newly released live output.” Either policy satisfies the
   invariant; the suite does not choose between them.
3. **Replay duplicates and IDs.** Observed: clean replay preserves each stored
   row once, including two intentionally equal user texts; stable distinct IDs
   survive repeated load. Proposal: “Replay emits each retained stored message
   once in stored order; equal text does not imply equal message identity.”
4. **Permission changes during Stop.** Observed: attachment metadata reflects
   effective daemon capabilities; legacy stubs report false. Proposal:
   “Evaluate permission and observed identity atomically when processing Stop;
   false permits withdrawal of this attachment's waiting input only.” A capability
   snapshot changing mid-turn is not currently exposed as an ACP update.
5. **Session MCP notice timing.** Observed: exactly one negotiated notice follows
   the attachment result. Proposal: clarify that ordering applies on this
   connection and does not place the notice in durable replay history.
6. **Transport loss versus detach.** Observed coverage separately checks pending
   withdrawal and inserted-work preservation. Proposal: specify whether abrupt
   loss must settle internal prompt waits identically to explicit detach, since
   no response can reach the lost transport.

## QA localization and regression disposition

Authoritative investigation: `docs/ACP_E2E_DEFECT_LOCALIZATION.md` at
`39fb7620589835d0c7f1a47653ab74f594b56eaa` on `fork/acp-e2e-defects`.
The original atlas report was observational and its hypotheses are superseded.

- **QA-STOP**, confirmed jcode defect: the 500 ms cancel reset races bash's
  750 ms handoff. `acp_conformance_stop_observed_bash_wait` runs a real bash
  file gate and asserts canceled retirement without further model output.
  Ignored on this lane's baseline; fix d46434b52 belongs to the separate lane.
  Enabled companion `acp_known_defect_stop_observed_peer` pins the working
  provider-stream case. We do not make wrong cancellation behavior a passing
  requirement. The ignored regression can be enabled after integrating the fix.
- **QA-STEER**, consumer defect: jcode emits the reverse steer. The corrected,
  enabled `acp_known_defect_reverse_steer_is_mirrored` sends an actual daemon
  SoftInterrupt, observes admission, releases the provider safe point and checks
  text, distinct steer message identity, output order, and the canonical wire.
  The earlier probe used Message instead of SoftInterrupt and waited before
  releasing the safe point; its timeout was a test defect, not wire evidence.
- **QA-REPLAY**, no demonstrated jcode defect: original duplicate rows predated
  import. Enabled `acp_known_defect_replay_first_user_once` imports a raw daemon
  session with two deliberate equal user prompts, verifies one replay per row,
  grouping/order, distinct IDs and repeat-load stability. Deduplicating equal
  text would fail this test and hide legitimate repeated submissions.
- **ACP-LOAD-BRACKET**, newly found jcode defect: repeated mid-turn load opens
  nested observation brackets. Ignored regression
  `acp_conformance_repeated_midturn_load_has_one_bracket` preserves the failing
  schedule. The ordinary generator explicitly detaches before loading an
  already-attached view. This restriction is a known coverage gap, not a proposed
  contract rule. Fix is assigned to the separate defect lane.

## Generator and corpus accounting

- `acp_conformance_interleavings`: 12 exhaustive owner/attachment/terminal cases.
- `acp_conformance_seeded_three_client_invariants`: four fixed seeds (0–3), six
  operation schedules each, three ACP clients, two turns per schedule. Owner,
  attach timing, and observer departure vary by seed. Every schedule includes a
  stale idle cancel and successor. `JCODE_ACP_INTERLEAVING_SEEDS=N` expands the
  deterministic prefix locally. Default execution is about four seconds.
- Operations: prompt, steer, cancel, detach, close, load; each schedule is isolated
  in its own session. Failures print seed plus the short reproducing schedule.
  There is no claim of automatic delta-debugging: independent schedules avoid
  irrelevant prefixes; the repeated-load regression isolates that minimal cause.
- Invariants: no nested brackets; every start has exactly one terminal per local
  bracket (reattachment may reopen the same shared identity); no client left
  running; origin/steer response correlation; terminal identity; no unanswered
  requests; exact origin output before/after steer; targeted cancellation and
  successor completion; detach preserves the authoritative witness's completion.
- Corpus files 01–05 and 07–11: ten files. 06 is the deferred v2 delivery scenario.
  New 09 is active-turn attach, 10 reverse steer, 11 clean imported repeated user
  rows. All are live recorded and structurally replayed via the existing helper.

## Additional retirement probes and precise coverage limits

- `acp_conformance_busy_config_errors_are_atomic`: an owner with a gated active
  prompt rejects model/config/effort changes; after retirement, a fresh load
  shows the original complete options and effective model.
- `acp_conformance_peer_config_serializes_after_active_turn`: a peer's valid
  model setter is deferred until the authoritative active agent lock is released;
  it succeeds with full effective options, and the active provider call retains
  the old model. This is observed behavior, not a new requirement that all busy
  setters must serialize. **Proposed wording for owner approval:** “Busy config
  may be rejected or serialized; a rejected request has no side effects, and a
  serialized request applies only after active turn retirement.” Existing prose
  tells consumers to submit while idle and handle busy responses, but does not
  explicitly require busy rejection. We therefore do not classify serialization
  itself as a defect or assert an arbitrary response deadline.
- `acp_conformance_transport_loss_preserves_inserted_work`: abrupt origin ACP
  child loss closes the daemon socket; a surviving witness receives normal
  completion of the same inserted turn.
- **ACP-LOST-STEER**, new known defect:
  `acp_conformance_transport_loss_withdraws_waiting_steer` confirms queue admission
  before killing the steering ACP child, while the provider remains gated before
  its insertion safe point. After allowing cleanup and releasing the gate, the
  baseline makes a second provider call for the disconnected attachment's input.
  This execution assertion (2 calls instead of 1), not the cleanup timeout,
  demonstrates the failed pending-input withdrawal. The assertion is ignored on
  this baseline and assigned to the separate fix lane. Already-inserted steers
  are intentionally outside this withdrawal expectation.

Indexed does not mean independently tested end to end. Remaining limits:

- C-CONFIG-P04: compact/subagent discovery and parsing are pinned; full daemon
  compaction scheduling and spawned-worker lifetime are not exercised by this
  ACP suite. Those mechanisms are outside the new boundary harness; no claim
  that launch completion equals worker completion is tested here.
- C-CONTENT-P04 and the client-side halves of list/config/capability clauses:
  consumer rendering, own-echo suppression, diagnostic deduplication, reconnect
  UI and capability gating belong to the Paseo consumer tests.
- `stopObservedTurn=false` implies `observe=false` in the current capability
  matrix. Legacy wire tests and the unit matrix pin that state; no unsupported
  observable-but-forbidden-Stop daemon capability combination is invented.
- The generator covers short, valid bounded schedules, not all arbitrary
  unbounded histories. Abrupt transport loss is tested separately; repeated load
  without detach is retained as a known-fail schedule. Provider faults and many
  queued steers remain in existing focused tests rather than randomized here.
- Source/version governance, deferred v2/SDK and consumer-only clauses have
  explicit non-wire dispositions in the inventory. Wording proposals above
  remain proposals; none changes the authoritative contract's meaning.
