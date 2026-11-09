use super::*;

pub fn end_session(provider_end: &str, model_end: &str) {
    end_session_with_reason(provider_end, model_end, SessionEndReason::NormalExit);
}

/// Owned terminal telemetry: the process session slot has already been captured.
/// Emitting may scan the filesystem and must run outside async registry locks.
pub struct DeferredLifecycleEvent(Box<dyn FnOnce() + Send>);

impl DeferredLifecycleEvent {
    pub fn emit(self) {
        (self.0)();
    }
}

pub fn prepare_session_end(
    provider: &str,
    model: &str,
    reason: SessionEndReason,
    crashed: bool,
    background: bool,
) -> Option<DeferredLifecycleEvent> {
    prepare_lifecycle_event(
        if crashed {
            "session_crash"
        } else {
            "session_end"
        },
        provider,
        model,
        reason,
        true,
        background,
    )
}

pub fn end_session_with_reason(provider_end: &str, model_end: &str, reason: SessionEndReason) {
    emit_lifecycle_event("session_end", provider_end, model_end, reason, true);
}

pub fn record_crash(provider_end: &str, model_end: &str, reason: SessionEndReason) {
    emit_lifecycle_event("session_crash", provider_end, model_end, reason, true);
}

pub(super) fn emit_lifecycle_event(
    event_name: &'static str,
    provider_end: &str,
    model_end: &str,
    reason: SessionEndReason,
    clear_state: bool,
) {
    if let Some(report) = prepare_lifecycle_event(
        event_name,
        provider_end,
        model_end,
        reason,
        clear_state,
        false,
    ) {
        report.emit();
    }
}

pub(super) fn prepare_lifecycle_event(
    event_name: &'static str,
    provider_end: &str,
    model_end: &str,
    reason: SessionEndReason,
    clear_state: bool,
    background: bool,
) -> Option<DeferredLifecycleEvent> {
    // Deferred reporting consumes the ended slot even when telemetry is
    // disabled or its installation ID later becomes unavailable. Retaining a
    // dead slot would let a later report close a successor's session instead.
    let id = if background {
        None
    } else {
        if !is_enabled() {
            return None;
        }
        Some(get_or_create_id()?)
    };
    let ended_at = Instant::now();
    let delivery = if background || matches!(reason, SessionEndReason::Superseded) {
        DeliveryMode::Background
    } else {
        DeliveryMode::Blocking(BLOCKING_LIFECYCLE_TIMEOUT)
    };
    let state = {
        let mut guard = match SESSION_STATE.lock() {
            Ok(guard) => guard,
            Err(_) => return None,
        };
        if let (Some(id), Some(active)) = (id.as_deref(), guard.as_mut()) {
            finalize_current_turn(id, active, ended_at, reason.as_str(), delivery);
        }
        if clear_state {
            guard.take()?
        } else {
            guard.as_ref()?.clone()
        }
    };
    let snapshot = LifecycleSnapshot {
        duration: ended_at.duration_since(state.started_at),
        ended_at_utc: Utc::now(),
        state,
    };
    let provider_end = provider_end.to_owned();
    let model_end = model_end.to_owned();
    Some(DeferredLifecycleEvent(Box::new(move || {
        let mut snapshot = snapshot;
        let id = match id {
            Some(id) => id,
            None => {
                if !is_enabled() {
                    return;
                }
                let Some(id) = get_or_create_id() else {
                    return;
                };
                finalize_current_turn(
                    &id,
                    &mut snapshot.state,
                    ended_at,
                    reason.as_str(),
                    delivery,
                );
                id
            }
        };
        emit_lifecycle_snapshot(
            event_name,
            &provider_end,
            &model_end,
            reason,
            delivery,
            id,
            snapshot,
        );
    })))
}

struct LifecycleSnapshot {
    state: SessionTelemetry,
    duration: Duration,
    ended_at_utc: chrono::DateTime<Utc>,
}

fn emit_lifecycle_snapshot(
    event_name: &'static str,
    provider_end: &str,
    model_end: &str,
    reason: SessionEndReason,
    delivery: DeliveryMode,
    id: String,
    snapshot: LifecycleSnapshot,
) {
    let LifecycleSnapshot {
        state,
        duration,
        ended_at_utc,
    } = snapshot;
    let errors = current_error_counts(&state);
    if !session_has_meaningful_activity(&state, &errors) {
        return;
    }
    if !state.start_event_sent {
        let _ = emit_session_start_for_state(id.clone(), &state, delivery);
    }
    let session_success = state.had_assistant_response
        || state.executed_tool_successes > 0
        || state.tests_passed > 0
        || state.file_write_calls > 0;
    let abandoned_before_response = state.had_user_prompt
        && !state.had_assistant_response
        && state.executed_tool_successes == 0;
    let workflow_coding_used = state.file_write_calls > 0 || state.tool_cat_write > 0;
    let workflow_research_used = state.feature_web_used || state.tool_cat_web > 0;
    let workflow_tests_used = state.tests_run > 0 || state.tests_passed > 0;
    let workflow_background_used = state.feature_background_used;
    let workflow_subagent_used = state.feature_subagent_used || state.tool_cat_subagent > 0;
    let workflow_swarm_used = state.feature_swarm_used || state.tool_cat_swarm > 0;
    let workflow_chat_only = state.had_user_prompt
        && !workflow_coding_used
        && !workflow_research_used
        && !workflow_tests_used
        && !workflow_background_used
        && !workflow_subagent_used
        && !workflow_swarm_used;
    let project_profile = detect_project_profile();
    let (active_days_7d, active_days_30d) = update_active_days(&id);
    let days_since_install = days_since_install(&id);
    let (schema_version, build_channel, git_checkout, ci, from_cargo) = telemetry_envelope();
    let session_stop_reason = infer_session_stop_reason(
        event_name,
        reason,
        &state,
        &errors,
        duration.as_secs(),
        session_success,
        abandoned_before_response,
        workflow_coding_used,
    );
    let agent_role = infer_agent_role(&state);
    let time_to_first_agent_action_ms = time_to_first_agent_action_ms(&state);
    let time_to_first_useful_action_ms = time_to_first_useful_action_ms(&state);
    let todo_event = todo_session_event(
        &state,
        reason,
        schema_version,
        build_channel.clone(),
        git_checkout,
        ci,
        from_cargo,
    );
    let event = SessionLifecycleEvent {
        event_id: new_event_id(),
        id,
        session_id: state.session_id.clone(),
        event: event_name,
        version: version(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        provider_start: state.provider_start,
        provider_end: sanitize_telemetry_label(provider_end),
        model_start: state.model_start,
        model_end: sanitize_telemetry_label(model_end),
        provider_switches: state.provider_switches,
        model_switches: state.model_switches,
        duration_mins: duration.as_secs() / 60,
        duration_secs: duration.as_secs(),
        turns: state.turns,
        had_user_prompt: state.had_user_prompt,
        had_assistant_response: state.had_assistant_response,
        assistant_responses: state.assistant_responses,
        first_assistant_response_ms: state.first_assistant_response_ms,
        first_tool_call_ms: state.first_tool_call_ms,
        first_tool_success_ms: state.first_tool_success_ms,
        first_file_edit_ms: state.first_file_edit_ms,
        first_test_pass_ms: state.first_test_pass_ms,
        tool_calls: state.tool_calls,
        tool_failures: state.tool_failures,
        executed_tool_calls: state.executed_tool_calls,
        executed_tool_successes: state.executed_tool_successes,
        executed_tool_failures: state.executed_tool_failures,
        tool_latency_total_ms: state.tool_latency_total_ms,
        tool_latency_max_ms: state.tool_latency_max_ms,
        file_write_calls: state.file_write_calls,
        tests_run: state.tests_run,
        tests_passed: state.tests_passed,
        input_tokens: state.input_tokens,
        output_tokens: state.output_tokens,
        cache_read_input_tokens: state.cache_read_input_tokens,
        cache_creation_input_tokens: state.cache_creation_input_tokens,
        total_tokens: state.total_tokens,
        feature_memory_used: state.feature_memory_used,
        feature_swarm_used: state.feature_swarm_used,
        feature_web_used: state.feature_web_used,
        feature_email_used: state.feature_email_used,
        feature_mcp_used: state.feature_mcp_used,
        feature_side_panel_used: state.feature_side_panel_used,
        feature_goal_used: state.feature_goal_used,
        feature_selfdev_used: state.feature_selfdev_used,
        feature_background_used: state.feature_background_used,
        feature_subagent_used: state.feature_subagent_used,
        feature_todo_used: state.feature_todo_used,
        unique_mcp_servers: state.unique_mcp_servers.len() as u32,
        session_success,
        abandoned_before_response,
        session_stop_reason,
        agent_role,
        parent_session_id: state.parent_session_id.clone(),
        agent_active_ms_total: state.agent_active_ms_total,
        agent_model_ms_total: state.agent_model_ms_total,
        agent_tool_ms_total: state.agent_tool_ms_total,
        session_idle_ms_total: state.session_idle_ms_total,
        agent_blocked_ms_total: state.agent_blocked_ms_total,
        time_to_first_agent_action_ms,
        time_to_first_useful_action_ms,
        spawned_agent_count: state.spawned_agent_count,
        background_task_count: state.background_task_count,
        background_task_completed_count: state.background_task_completed_count,
        subagent_task_count: state.subagent_task_count,
        subagent_success_count: state.subagent_success_count,
        swarm_task_count: state.swarm_task_count,
        swarm_success_count: state.swarm_success_count,
        user_cancelled_count: state.user_cancelled_count,
        transport_https: state.transport_https,
        transport_persistent_ws_fresh: state.transport_persistent_ws_fresh,
        transport_persistent_ws_reuse: state.transport_persistent_ws_reuse,
        transport_cli_subprocess: state.transport_cli_subprocess,
        transport_native_http2: state.transport_native_http2,
        transport_other: state.transport_other,
        tool_cat_read_search: state.tool_cat_read_search,
        tool_cat_write: state.tool_cat_write,
        tool_cat_shell: state.tool_cat_shell,
        tool_cat_web: state.tool_cat_web,
        tool_cat_memory: state.tool_cat_memory,
        tool_cat_subagent: state.tool_cat_subagent,
        tool_cat_swarm: state.tool_cat_swarm,
        tool_cat_email: state.tool_cat_email,
        tool_cat_side_panel: state.tool_cat_side_panel,
        tool_cat_goal: state.tool_cat_goal,
        tool_cat_mcp: state.tool_cat_mcp,
        tool_cat_other: state.tool_cat_other,
        tool_cat_todo: state.tool_cat_todo,
        todo_gate_ownership_count: state.todo_gate_ownership_count,
        todo_gate_feedback_loop_count: state.todo_gate_feedback_loop_count,
        todo_gate_alignment_count: state.todo_gate_alignment_count,
        todo_gate_intent_count: state.todo_gate_intent_count,
        todo_gate_completion_count: state.todo_gate_completion_count,
        todo_gate_spike_count: state.todo_gate_spike_count,
        command_login_used: state.command_login_used,
        command_model_used: state.command_model_used,
        command_usage_used: state.command_usage_used,
        command_resume_used: state.command_resume_used,
        command_memory_used: state.command_memory_used,
        command_swarm_used: state.command_swarm_used,
        command_goal_used: state.command_goal_used,
        command_selfdev_used: state.command_selfdev_used,
        command_feedback_used: state.command_feedback_used,
        command_other_used: state.command_other_used,
        workflow_chat_only,
        workflow_coding_used,
        workflow_research_used,
        workflow_tests_used,
        workflow_background_used,
        workflow_subagent_used,
        workflow_swarm_used,
        project_repo_present: project_profile.repo_present,
        project_lang_rust: project_profile.lang_rust,
        project_lang_js_ts: project_profile.lang_js_ts,
        project_lang_python: project_profile.lang_python,
        project_lang_go: project_profile.lang_go,
        project_lang_markdown: project_profile.lang_markdown,
        project_lang_mixed: project_profile.mixed(),
        days_since_install,
        active_days_7d,
        active_days_30d,
        session_start_hour_utc: utc_hour(state.started_at_utc),
        session_start_weekday_utc: utc_weekday(state.started_at_utc),
        session_end_hour_utc: utc_hour(ended_at_utc),
        session_end_weekday_utc: utc_weekday(ended_at_utc),
        previous_session_gap_secs: state.previous_session_gap_secs,
        sessions_started_24h: state.sessions_started_24h,
        sessions_started_7d: state.sessions_started_7d,
        active_sessions_at_start: state.active_sessions_at_start,
        other_active_sessions_at_start: state.other_active_sessions_at_start,
        max_concurrent_sessions: state.max_concurrent_sessions,
        multi_sessioned: state.max_concurrent_sessions > 1
            || state.other_active_sessions_at_start > 0,
        resumed_session: state.resumed_session,
        end_reason: reason.as_str(),
        schema_version,
        build_channel,
        is_git_checkout: git_checkout,
        is_ci: ci,
        ran_from_cargo: from_cargo,
        errors,
    };
    if let Ok(payload) = serde_json::to_value(&event) {
        let _ = send_payload(payload, delivery);
    }
    if let Ok(payload) = serde_json::to_value(&todo_event) {
        let _ = send_payload(payload, delivery);
    }
    if session_success {
        emit_onboarding_step_once("first_session_success", None, None);
    }
}

pub(super) fn todo_session_event(
    state: &SessionTelemetry,
    reason: SessionEndReason,
    schema_version: u32,
    build_channel: String,
    is_git_checkout: bool,
    is_ci: bool,
    ran_from_cargo: bool,
) -> TodoSessionEvent {
    let todo = &state.todo;
    TodoSessionEvent {
        event_id: new_event_id(),
        id: state.correlation_id.clone(),
        correlation_id: state.correlation_id.clone(),
        event: "todo_session",
        version: version(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        session_end_reason: reason.as_str(),
        todos_created: todo.todos_created,
        todos_completed: todo.todos_completed,
        todos_abandoned: todo.abandoned(),
        todo_updates: todo.todo_updates,
        groups_completed: todo.groups_completed,
        groups_total: todo.groups_total,
        max_todo_list_size: todo.max_todo_list_size,
        confidence_min: todo.confidence.min,
        confidence_mean: todo.confidence.mean,
        confidence_count: todo.confidence.count,
        completion_confidence_min: todo.completion_confidence.min,
        completion_confidence_mean: todo.completion_confidence.mean,
        completion_confidence_count: todo.completion_confidence.count,
        understands_user_intent_min: todo.understands_user_intent.min,
        understands_user_intent_mean: todo.understands_user_intent.mean,
        understands_user_intent_count: todo.understands_user_intent.count,
        closed_feedback_loop_min: todo.closed_feedback_loop.min,
        closed_feedback_loop_mean: todo.closed_feedback_loop.mean,
        closed_feedback_loop_count: todo.closed_feedback_loop.count,
        feedback_loop_relevance_min: todo.feedback_loop_relevance.min,
        feedback_loop_relevance_mean: todo.feedback_loop_relevance.mean,
        feedback_loop_relevance_count: todo.feedback_loop_relevance.count,
        feedback_loop_coverage_min: todo.feedback_loop_coverage.min,
        feedback_loop_coverage_mean: todo.feedback_loop_coverage.mean,
        feedback_loop_coverage_count: todo.feedback_loop_coverage.count,
        end_to_end_ownership_min: todo.end_to_end_ownership.min,
        end_to_end_ownership_mean: todo.end_to_end_ownership.mean,
        end_to_end_ownership_count: todo.end_to_end_ownership.count,
        schema_version,
        build_channel,
        is_git_checkout,
        is_ci,
        ran_from_cargo,
    }
}
