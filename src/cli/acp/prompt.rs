use super::*;

impl AcpRuntime {
    pub(super) async fn run_prompt(
        &self,
        rpc_id: Value,
        session: Arc<DaemonSession>,
        text: String,
        images: Vec<(String, String)>,
        selection: SkillSelection,
        ticket: &mut PromptTicket,
    ) -> Result<()> {
        // An explicit clear lands before the text is interpreted, so neither a
        // textual `/skill` invocation nor the daemon sees the old selection,
        // and a command prompt cannot leave it attached either.
        if selection == SkillSelection::Clear {
            *session.active_skill.lock().await = None;
        }

        let mut subagent = None;
        if let Some(command) = selected_slash_command(&text, &selection) {
            if let Ok(AcpSlashCommand::Subagent(spec)) = command {
                // A subagent launch is a streamed tool call, so it runs through
                // the prompt event loop below instead of returning one message.
                subagent = Some(spec);
            } else {
                return self
                    .run_command_prompt(rpc_id, &session, command, ticket)
                    .await;
            }
        }

        let skills = session.skills();
        let plan = if subagent.is_some() {
            // A subagent launch is not a skill prompt: the attachment keeps its
            // selection and the spec stays as written.
            PromptSkillPlan {
                skill: session.active_skill.lock().await.clone(),
                attach: false,
                text,
                selection_only: false,
            }
        } else {
            // Explicit client metadata wins. Otherwise resolve `/skill [prompt]`
            // (or `$skill [prompt]`, as some ACP hosts serialize command names)
            // against the session's registry. Unknown or path-like slash text is
            // passed through to the model unchanged.
            let sticky = session.active_skill.lock().await.clone();
            match plan_prompt_skill(&selection, sticky, &skills, text, !images.is_empty()) {
                Ok(plan) => plan,
                Err(err) => {
                    ticket.release().await;
                    self.write_error_value(rpc_id, JSONRPC_INVALID_PARAMS, err)
                        .await?;
                    return Ok(());
                }
            }
        };
        if plan.attach {
            *session.active_skill.lock().await = plan.skill.clone();
            if let Some(skill) = plan.skill.as_deref().and_then(|name| skills.get(name)) {
                let notice = format!("Activated skill: {} - {}", skill.name, skill.description);
                let session_id = session.session_id().await;
                self.write_notification(
                    "session/update",
                    json!({
                        "sessionId": session_id,
                        "update": agent_message_chunk_with_id(
                        format!("{notice}\n\n"),
                        format!("{session_id}:skill-notice:{rpc_id}"),
                    ),
                    }),
                )
                .await?;
            }
        }
        if plan.selection_only {
            // A selection-only action: the selection is the whole request (a
            // bare `/skill`, or `skill: null` with no text), like the TUI.
            ticket.release().await;
            self.write_result(rpc_id, prompt_response("end_turn", &TurnUsage::default()))
                .await?;
            return Ok(());
        }

        let prompt_id = session.next_id();
        let mut router = session.router.lock().await;
        let Some(mut prompt_rx) = router.register_open_prompt(prompt_id) else {
            drop(router);
            ticket.release().await;
            return self
                .write_result(rpc_id, prompt_response("cancelled", &TurnUsage::default()))
                .await;
        };
        ticket.prompt_id = Some(prompt_id);

        let request = match subagent {
            Some(spec) => Request::RunSubagent {
                id: prompt_id,
                prompt: spec.prompt,
                subagent_type: spec.subagent_type,
                model: spec.model,
                session_id: None,
            },
            None => prompt_request(&mut router, prompt_id, plan.text, images, plan.skill),
        };
        let send_result = session.send(&request).await;
        let follow_up = if send_result.is_ok() {
            router.prompt_sent(prompt_id)
        } else {
            Vec::new()
        };
        drop(router);
        if let Err(err) = send_result {
            ticket.release().await;
            return Err(err);
        }
        session.sink(self.clone()).apply(follow_up).await;
        self.stream_prompt_events(rpc_id, &session, prompt_id, &mut prompt_rx, ticket)
            .await
    }

    pub(super) async fn run_command_prompt(
        &self,
        rpc_id: Value,
        session: &Arc<DaemonSession>,
        command: Result<AcpSlashCommand>,
        ticket: &mut PromptTicket,
    ) -> Result<()> {
        let response = match command {
            Ok(command) => self.run_session_command(session, command).await,
            Err(err) => Err(err),
        };
        ticket.release().await;
        let response = response?;
        let session_id = session.session_id().await;
        self.write_notification(
            "session/update",
            json!({
                "sessionId": session_id,
                "update": agent_message_chunk_with_id(
                    response,
                    format!("{session_id}:command:{rpc_id}"),
                ),
            }),
        )
        .await?;
        self.write_result(rpc_id, prompt_response("end_turn", &TurnUsage::default()))
            .await?;
        Ok(())
    }

    /// Relay daemon events for `prompt_id` until it completes, then answer the
    /// ACP `session/prompt` request.
    /// Relay this adapter's own turn events to the ACP client until the prompt
    /// completes, then answer the ACP `session/prompt` request.
    ///
    /// Peer turns never reach this channel: the connection's reader routes them
    /// to the peer mirror, so a prompt's result covers exactly its own turn.
    pub(super) async fn stream_prompt_events(
        &self,
        rpc_id: Value,
        session: &Arc<DaemonSession>,
        prompt_id: u64,
        prompt_rx: &mut RoutedReceiver,
        ticket: &mut PromptTicket,
    ) -> Result<()> {
        let session_id = session.session_id().await;
        let scope = format!("{session_id}:own:{prompt_id}");
        let mut mapper = EventMapper::with_scope(session_id.clone(), self.profile, scope.clone())
            .with_working_dir(session.working_dir.clone());
        let mut stop_reason = "end_turn".to_string();
        let mut turn_usage = TurnUsage::default();
        let mut steers = 0u32;
        loop {
            let delivery = match prompt_rx.recv().await {
                Some(delivery) => delivery,
                None => {
                    ticket.release().await;
                    anyhow::bail!("Jcode daemon disconnected");
                }
            };
            let event = match delivery {
                RoutedEvent::Event(event) => event,
                RoutedEvent::LocalTurnEnd {
                    identity,
                    status,
                    message,
                } => {
                    self.write_turn_notification(
                        &session_id,
                        &turn_id_string(&identity),
                        turn_end_state(status),
                        None,
                        message.as_deref(),
                    )
                    .await?;
                    continue;
                }
            };
            if self.profile.is_extended() {
                self.write_jcode_extension_event(&session_id, &event)
                    .await?;
            }
            match event {
                ServerEvent::Ack { .. } => {}
                ServerEvent::Done { id } if id == prompt_id => break,
                ServerEvent::Interrupted => {
                    stop_reason = "cancelled".to_string();
                }
                ServerEvent::Error { id, message, .. } if id == prompt_id => {
                    ticket.release().await;
                    self.write_error_value(rpc_id, JSONRPC_SERVER_ERROR, message)
                        .await?;
                    return Ok(());
                }
                // Another client steered the turn answering this prompt.
                ServerEvent::SoftInterruptInjected { content, .. } => {
                    if !content.trim().is_empty() {
                        self.write_notification(
                            "session/update",
                            user_message_chunk(
                                &session_id,
                                format!("{scope}:steer:{steers}"),
                                &content,
                            ),
                        )
                        .await?;
                    }
                    steers += 1;
                }
                ServerEvent::KvCacheRequest { .. } | ServerEvent::RetryRollback { .. } => {
                    turn_usage.begin_call();
                }
                ServerEvent::TokenUsage {
                    input,
                    output,
                    cache_read_input,
                    cache_creation_input,
                    ..
                } => {
                    turn_usage.observe_snapshot(
                        input,
                        output,
                        cache_read_input,
                        cache_creation_input,
                    );
                    self.write_usage_update(
                        &session_id,
                        &session.ui_state,
                        input,
                        cache_read_input,
                        cache_creation_input,
                    )
                    .await?;
                }
                other => {
                    for update in mapper.map_event(other) {
                        self.write_notification(
                            "session/update",
                            json!({
                                "sessionId": session_id,
                                "update": update,
                            }),
                        )
                        .await?;
                    }
                }
            }
        }

        let router = session.router.lock().await;
        let response = streamed_prompt_response(
            &stop_reason,
            &turn_usage,
            router.prompt_correlation(prompt_id),
            router.prompt_was_turnless(prompt_id),
        );
        drop(router);
        ticket.release().await;
        self.write_result(rpc_id, response).await?;
        Ok(())
    }

    pub(super) async fn run_session_command(
        &self,
        session: &Arc<DaemonSession>,
        command: AcpSlashCommand,
    ) -> Result<String> {
        match command {
            AcpSlashCommand::Model(None) => {
                let state = session.ui_state.lock().await;
                Ok(match state.model.as_deref() {
                    Some(model) => format!("Current model: `{model}`"),
                    None => "The daemon did not report a current model.".to_string(),
                })
            }
            AcpSlashCommand::Model(Some(model)) => {
                let id = session.next_id();
                let mut rx = session.register(id, PendingKind::Reply).await;
                session
                    .send(&Request::SetModel {
                        id,
                        model: model.clone(),
                    })
                    .await?;
                let result = wait_for_model_changed(session, &mut rx, id).await;
                session.unregister(id).await;
                result?;
                self.write_config_option_update(session).await?;
                let selected = session.ui_state.lock().await.model.clone().unwrap_or(model);
                Ok(format!("Switched model to `{selected}`."))
            }
            AcpSlashCommand::Models => {
                request_model_catalog(session).await?;
                // The reader has already applied this snapshot in event order.
                let (current, models) = {
                    let state = session.ui_state.lock().await;
                    (state.model.clone(), state.available_models.clone())
                };
                self.write_config_option_update(session).await?;
                Ok(format_model_catalog(current.as_deref(), &models))
            }
            AcpSlashCommand::Effort(None) => {
                let state = session.ui_state.lock().await;
                let current = state
                    .reasoning_effort
                    .as_deref()
                    .unwrap_or("provider default");
                let available = available_efforts(&state);
                if available.is_empty() {
                    Ok(format!("Current reasoning effort: `{current}`."))
                } else {
                    Ok(format!(
                        "Current reasoning effort: `{current}`. Available: {}.",
                        available
                            .iter()
                            .map(|effort| format!("`{effort}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
            }
            AcpSlashCommand::Effort(Some(effort)) => {
                let Some(effort) = normalize_effort(&*session.ui_state.lock().await, &effort)?
                else {
                    return Ok(
                        "Reasoning effort ignored: the current model does not support it."
                            .to_string(),
                    );
                };
                let id = session.next_id();
                let mut rx = session.register(id, PendingKind::Reply).await;
                session
                    .send(&Request::SetReasoningEffort {
                        id,
                        effort: effort.clone(),
                        target_session_id: None,
                    })
                    .await?;
                let result = wait_for_effort_changed(session, &mut rx, id).await;
                session.unregister(id).await;
                result?;
                self.write_config_option_update(session).await?;
                let selected = session
                    .ui_state
                    .lock()
                    .await
                    .reasoning_effort
                    .clone()
                    .unwrap_or(effort);
                Ok(format!("Set reasoning effort to `{selected}`."))
            }
            AcpSlashCommand::Compact => {
                let id = session.next_id();
                let mut rx = session.register(id, PendingKind::Reply).await;
                session.send(&Request::Compact { id }).await?;
                let result = wait_for_compact_result(&mut rx, id).await;
                session.unregister(id).await;
                result
            }
            AcpSlashCommand::Subagent(_) => {
                anyhow::bail!("/subagent streams through the prompt loop")
            }
        }
    }

    /// `usage_update` for one reported token usage event. Shared by this
    /// adapter's own turn and the peer-turn mirror, so both show the same
    /// context gauge.
    pub(super) async fn write_usage_update(
        &self,
        session_id: &str,
        ui_state: &Arc<Mutex<SessionUiState>>,
        input: u64,
        cache_read_input: Option<u64>,
        cache_creation_input: Option<u64>,
    ) -> Result<()> {
        let payload = {
            let state = ui_state.lock().await;
            usage_update_payload(
                session_id,
                &state,
                input,
                cache_read_input,
                cache_creation_input,
            )
        };
        self.write_notification("session/update", payload).await
    }

    pub(super) async fn write_config_option_update(&self, session: &DaemonSession) -> Result<()> {
        let config_options = session_config_options(&*session.ui_state.lock().await);
        let session_id = session.session_id().await;
        self.write_notification(
            "session/update",
            json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "config_option_update",
                    "configOptions": config_options,
                }
            }),
        )
        .await
    }
}

impl AcpRuntime {
    pub(super) async fn write_jcode_extension_event(
        &self,
        session_id: &str,
        event: &ServerEvent,
    ) -> Result<()> {
        self.write_notification(
            "_jcode/server_event",
            json!({
                "sessionId": session_id,
                "event": serde_json::to_value(event).unwrap_or(Value::Null),
            }),
        )
        .await
    }

    /// Handle one routing action produced by the connection's reader.
    ///
    /// Session state is applied immediately; peer output is held back until the
    /// attach handshake finished, so a mid-turn attach cannot interleave live
    /// peer events with the history replay.
    pub(super) async fn dispatch_route_action(
        &self,
        router: &Arc<Mutex<RouterState>>,
        ui_state: &Arc<Mutex<SessionUiState>>,
        gate: &Arc<Mutex<PeerActionGate>>,
        peer_mapper: &Arc<Mutex<Option<EventMapper>>>,
        working_dir: Option<&Path>,
        action: RouteAction,
    ) -> Result<()> {
        if let RouteAction::SessionState(change) = action {
            let session_id = router.lock().await.session_id().to_string();
            return self
                .apply_session_state(ui_state, &session_id, change)
                .await;
        }
        if let RouteAction::PeerPromptReplayed { turn_id } = &action {
            // Only meaningful while the attach replay is still holding the
            // announcement; once released, the prompt has been shown already.
            gate.lock().await.mark_prompt_replayed(turn_id);
            return Ok(());
        }
        if gate.lock().await.hold(action.clone()) {
            return Ok(());
        }
        self.apply_peer_action(router, ui_state, peer_mapper, working_dir, action)
            .await
    }

    /// Flush peer output held during an attach handshake, then let live peer
    /// events through.
    pub(super) async fn release_peer_actions(&self, session: &Arc<DaemonSession>) -> Result<()> {
        flush_peer_actions(&session.peer_gate, |action| {
            Box::pin(async move {
                self.apply_peer_action(
                    &session.router,
                    &session.ui_state,
                    &session.peer_mapper,
                    session.working_dir.as_deref(),
                    action,
                )
                .await
            })
        })
        .await
    }

    /// Render one peer-turn action for the ACP client.
    pub(super) async fn apply_peer_action(
        &self,
        router: &Arc<Mutex<RouterState>>,
        ui_state: &Arc<Mutex<SessionUiState>>,
        peer_mapper: &Arc<Mutex<Option<EventMapper>>>,
        working_dir: Option<&Path>,
        action: RouteAction,
    ) -> Result<()> {
        let session_id = router.lock().await.session_id().to_string();
        match action {
            RouteAction::PeerTurnStarted {
                turn_id,
                origin,
                user_content,
                prompt_already_replayed,
            } => {
                // A fresh mapper per peer turn: tool-call ids and streamed
                // inputs must not leak between turns or into this adapter's own
                // prompt mapping.
                *peer_mapper.lock().await = Some(
                    EventMapper::with_scope(
                        session_id.clone(),
                        self.profile,
                        format!("{session_id}:peer:{turn_id}"),
                    )
                    .with_working_dir(working_dir.map(Path::to_path_buf)),
                );
                self.write_turn_notification(&session_id, &turn_id, "started", Some(&origin), None)
                    .await?;
                // A mid-turn attach already replayed this prompt from the
                // snapshot; announcing it again would show it twice.
                let content = if prompt_already_replayed {
                    None
                } else {
                    user_content
                };
                if let Some(content) = content.filter(|content| !content.trim().is_empty()) {
                    self.write_notification(
                        "session/update",
                        peer_user_message_chunk(&session_id, &turn_id, &content),
                    )
                    .await?;
                }
            }
            RouteAction::PeerTurnEvent(ServerEvent::TokenUsage {
                input,
                cache_read_input,
                cache_creation_input,
                ..
            }) => {
                self.write_usage_update(
                    &session_id,
                    ui_state,
                    input,
                    cache_read_input,
                    cache_creation_input,
                )
                .await?;
            }
            RouteAction::PeerTurnEvent(event) => {
                let updates = {
                    let mut mapper = peer_mapper.lock().await;
                    mapper
                        .get_or_insert_with(|| {
                            // Only reachable if a peer turn's start was missed;
                            // a per-connection counter still gives the turn's
                            // chunks one stable namespace.
                            let scope = format!(
                                "{session_id}:peer-stream:{}",
                                self.connection_seq.fetch_add(1, Ordering::Relaxed)
                            );
                            EventMapper::with_scope(session_id.clone(), self.profile, scope)
                                .with_working_dir(working_dir.map(Path::to_path_buf))
                        })
                        .map_event(event)
                };
                for update in updates {
                    self.write_notification(
                        "session/update",
                        json!({ "sessionId": session_id, "update": update }),
                    )
                    .await?;
                }
            }
            RouteAction::PeerTurnEnded {
                turn_id,
                status,
                message,
            } => {
                *peer_mapper.lock().await = None;
                self.write_turn_notification(
                    &session_id,
                    &turn_id,
                    turn_end_state(status),
                    None,
                    message.as_deref(),
                )
                .await?;
            }
            RouteAction::PeerSteer {
                turn_id,
                index,
                content,
            } => {
                if !content.trim().is_empty() {
                    self.write_notification(
                        "session/update",
                        user_message_chunk(
                            &session_id,
                            format!("{session_id}:{turn_id}:steer:{index}"),
                            &content,
                        ),
                    )
                    .await?;
                }
            }
            RouteAction::PeerTurnAdopted { turn_id } => {
                // The prompt that adopted the turn maps the rest of it.
                crate::logging::debug(&format!("acp: prompt adopted peer turn {turn_id}"));
                *peer_mapper.lock().await = None;
            }
            RouteAction::Plan { payload } => {
                let update = {
                    let mut state = ui_state.lock().await;
                    plan_update(&mut state.last_plan, &payload)
                };
                if let Some(update) = update {
                    self.write_notification(
                        "session/update",
                        json!({ "sessionId": session_id, "update": update }),
                    )
                    .await?;
                }
            }
            // Executed by `RouteSink::apply`, never rendered.
            RouteAction::Send(_) | RouteAction::Wake { .. } => {}
            RouteAction::SessionState(change) => {
                self.apply_session_state(ui_state, &session_id, change)
                    .await?;
            }
            RouteAction::PeerPromptReplayed { .. } => {}
        }
        Ok(())
    }

    /// `_jcode/turn`: the peer-turn bracket. Sent in every profile; ACP
    /// extension notifications are ignored by clients that do not know them,
    /// and standard clients still receive the content through `session/update`.
    /// Only peer turns get one: a `session/prompt` response already brackets the
    /// client's own turn.
    pub(super) async fn write_turn_notification(
        &self,
        session_id: &str,
        turn_id: &str,
        state: &str,
        origin: Option<&TurnOrigin>,
        error: Option<&str>,
    ) -> Result<()> {
        let mut params = json!({
            "sessionId": session_id,
            "turnId": turn_id,
            "state": state,
        });
        if let Some(object) = params.as_object_mut() {
            if let Some(origin) = origin {
                let mut wire = json!({ "kind": turn_origin_kind(origin) });
                if let Some(object) = wire.as_object_mut()
                    && let Some(instance_id) = origin.client_instance_id.as_deref()
                {
                    object.insert(
                        "clientInstanceId".to_string(),
                        Value::String(instance_id.to_string()),
                    );
                }
                object.insert("origin".to_string(), wire);
            }
            if let Some(error) = error {
                object.insert("error".to_string(), json!({ "message": error }));
            }
        }
        self.write_notification("_jcode/turn", params).await
    }

    /// Apply a session-wide state change another client caused, then surface it
    /// through the same notifications this adapter uses for its own changes.
    pub(super) async fn apply_session_state(
        &self,
        ui_state: &Arc<Mutex<SessionUiState>>,
        session_id: &str,
        change: SessionStateChange,
    ) -> Result<()> {
        let config_options = {
            let mut state = ui_state.lock().await;
            match change {
                SessionStateChange::Model {
                    model,
                    provider_name,
                    reasoning_effort,
                    available_reasoning_efforts,
                } => {
                    state.reasoning_effort = reasoning_effort;
                    state.available_reasoning_efforts = available_reasoning_efforts;
                    state.model = Some(model);
                    if provider_name.is_some() {
                        state.provider_name = provider_name;
                    }
                    Some(session_config_options(&state))
                }
                SessionStateChange::Effort { effort } => {
                    state.reasoning_effort = effort;
                    Some(session_config_options(&state))
                }
                SessionStateChange::Renamed { title } => {
                    drop(state);
                    self.write_notification(
                        "session/update",
                        json!({
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "session_info_update",
                                "title": title,
                            }
                        }),
                    )
                    .await?;
                    None
                }
            }
        };
        if let Some(config_options) = config_options
            && !config_options.is_empty()
        {
            self.write_notification(
                "session/update",
                json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "config_option_update",
                        "configOptions": config_options,
                    }
                }),
            )
            .await?;
        }
        Ok(())
    }
}
