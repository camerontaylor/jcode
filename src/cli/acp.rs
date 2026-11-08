use super::provider_init::ProviderChoice;
use super::{commands, dispatch};
use crate::protocol::{Request, ServerEvent, TurnEndStatus, TurnOrigin};
use crate::transport::{ReadHalf, WriteHalf};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::sync::mpsc;

const ACP_PROTOCOL_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpProfile {
    Standard,
    Extended,
    Full,
}

impl AcpProfile {
    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "extended" => Self::Extended,
            "full" => Self::Full,
            _ => Self::Standard,
        }
    }

    fn is_extended(self) -> bool {
        matches!(self, Self::Extended | Self::Full)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Extended => "extended",
            Self::Full => "full",
        }
    }
}

#[derive(Clone)]
struct AcpRuntime {
    stdout: Arc<Mutex<tokio::io::Stdout>>,
    sessions: Arc<Mutex<HashMap<String, Arc<DaemonSession>>>>,
    profile: AcpProfile,
    provider_choice: ProviderChoice,
    model: Option<String>,
    provider_profile: Option<String>,
    /// Unique identity of this adapter process. The daemon reports it in every
    /// turn origin, which is how a turn is recognised as this client's own
    /// rather than a peer's. A fixed value would make two ACP clients (or two
    /// adapters in one host) mistake each other's turns for their own.
    instance_id: String,
    /// Randomized request-id base. A peer's `done`/`error` carries the
    /// *originator's* connection-local id, and TUI ids start near zero.
    request_id_base: u64,
    /// Per-connection suffix for `instance_id`, so two sessions in one adapter
    /// process never mistake each other's turns for their own.
    connection_seq: Arc<AtomicU64>,
    /// Whether this connection's client advertised `clientCapabilities.session
    /// .notices` at `initialize`. ACP v1 forbids `notice` updates otherwise, so
    /// informational notices are withheld rather than faked as messages.
    client_session_notices: Arc<AtomicBool>,
}

impl AcpRuntime {
    fn new(
        profile: AcpProfile,
        provider_choice: ProviderChoice,
        model: Option<String>,
        provider_profile: Option<String>,
    ) -> Self {
        Self {
            stdout: Arc::new(Mutex::new(tokio::io::stdout())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            profile,
            provider_choice,
            model,
            provider_profile,
            instance_id: format!("acp-{}", random_uuid_v4()),
            request_id_base: random_request_id_base(),
            connection_seq: Arc::new(AtomicU64::new(0)),
            client_session_notices: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn run(self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Ok(());
            }
            if line.trim().is_empty() {
                continue;
            }

            let message = match JsonRpcMessage::parse(&line) {
                Ok(message) => message,
                Err((code, message)) => {
                    self.write_error_value(
                        Value::Null,
                        code,
                        format!("Invalid JSON-RPC request: {message}"),
                    )
                    .await?;
                    continue;
                }
            };

            self.handle_message(message).await?;
        }
    }

    async fn handle_message(&self, message: JsonRpcMessage) -> Result<()> {
        let Some(method) = message.method.as_deref() else {
            if let Some(id) = message.id {
                self.write_error_value(
                    id,
                    JSONRPC_INVALID_REQUEST,
                    "JSON-RPC request missing method".to_string(),
                )
                .await?;
            }
            return Ok(());
        };

        match method {
            "initialize" => {
                // Record what this connection may receive before answering: a
                // v1 client that did not advertise `session.notices` must never
                // see a `notice` update.
                self.client_session_notices.store(
                    client_advertises_session_notices(&message.params),
                    Ordering::SeqCst,
                );
                if let Some(id) = message.id {
                    self.write_result(id, initialize_result(&message.params, self.profile))
                        .await?;
                }
            }
            "session/new" => self.handle_session_new(message).await?,
            "session/load" => self.handle_session_load(message, true).await?,
            "session/resume" => self.handle_session_load(message, false).await?,
            "session/prompt" => self.handle_session_prompt(message).await?,
            "session/cancel" => self.handle_session_cancel(message).await?,
            "_jcode/detach" => self.handle_session_detach(message).await?,
            "session/close" => self.handle_session_close(message).await?,
            "session/list" => self.handle_session_list(message).await?,
            "session/set_config_option" => self.handle_set_config_option(message).await?,
            "session/set_model" => {
                self.handle_compat_config_option(
                    message,
                    CONFIG_ID_MODEL,
                    &["modelId", "model"],
                    "session/set_model",
                )
                .await?
            }
            "session/set_reasoning_effort" => {
                self.handle_compat_config_option(
                    message,
                    CONFIG_ID_EFFORT,
                    &["effort", "reasoningEffort"],
                    "session/set_reasoning_effort",
                )
                .await?
            }
            _ if method.starts_with('_') => {
                if let Some(id) = message.id {
                    self.write_error_value(
                        id,
                        JSONRPC_METHOD_NOT_FOUND,
                        format!("Unsupported Jcode ACP extension method: {method}"),
                    )
                    .await?;
                }
            }
            _ => {
                if let Some(id) = message.id {
                    self.write_error_value(
                        id,
                        JSONRPC_METHOD_NOT_FOUND,
                        format!("Unsupported ACP method: {method}"),
                    )
                    .await?;
                }
            }
        }

        Ok(())
    }

    async fn handle_session_new(&self, message: JsonRpcMessage) -> Result<()> {
        let Some(id) = message.id else {
            return Ok(());
        };
        let cwd = match cwd_from_params(&message.params) {
            Ok(cwd) => cwd,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        if let Err(err) = validate_acp_mcp_servers(&message.params) {
            self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                .await?;
            return Ok(());
        }

        match self.create_new_session(cwd).await {
            Ok(session) => {
                let session_id = session.session_id().await;
                let state = session.ui_state.lock().await.clone();
                let mut result = json!({ "sessionId": session_id });
                insert_session_configuration(&mut result, &state);
                insert_jcode_session_meta(
                    &mut result,
                    session.instance_id(),
                    session.shared_control_support().await,
                );
                self.sessions
                    .lock()
                    .await
                    .insert(session_id.clone(), session);
                self.write_result(id, result).await?;
                self.write_available_commands(&session_id).await?;
                self.write_ignored_mcp_servers_notice(&session_id, &message.params)
                    .await?;
            }
            Err(err) => {
                self.write_error_value(
                    id,
                    JSONRPC_INTERNAL_ERROR,
                    format!("Failed to create Jcode session: {err:#}"),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Report the client-supplied `mcpServers` this attachment ignores, once,
    /// after the response that created or attached the session. A notice never
    /// changes the response and an empty list says nothing.
    async fn write_ignored_mcp_servers_notice(
        &self,
        session_id: &str,
        params: &Value,
    ) -> Result<()> {
        let notices = self.client_session_notices.load(Ordering::SeqCst);
        if let Some(notice) = ignored_mcp_servers_notice(session_id, params, notices) {
            self.write_notification("session/update", notice).await?;
        }
        Ok(())
    }

    async fn handle_session_load(
        &self,
        message: JsonRpcMessage,
        replay_history: bool,
    ) -> Result<()> {
        let Some(id) = message.id else {
            return Ok(());
        };
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        let cwd = match cwd_from_params(&message.params) {
            Ok(cwd) => cwd,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        if let Err(err) = validate_acp_mcp_servers(&message.params) {
            self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                .await?;
            return Ok(());
        }

        match self
            .attach_existing_session(session_id.clone(), cwd, replay_history)
            .await
        {
            Ok(session) => {
                let state = session.ui_state.lock().await.clone();
                let attached_id = session.session_id().await;
                let mut result = json!({});
                insert_session_configuration(&mut result, &state);
                insert_jcode_session_meta(
                    &mut result,
                    session.instance_id(),
                    session.shared_control_support().await,
                );
                self.sessions.lock().await.insert(attached_id, session);
                self.write_result(id, result).await?;
                self.write_available_commands(&session_id).await?;
                self.write_ignored_mcp_servers_notice(&session_id, &message.params)
                    .await?;
            }
            Err(err) => {
                self.write_error_value(
                    id,
                    JSONRPC_INTERNAL_ERROR,
                    format!("Failed to attach Jcode session '{session_id}': {err:#}"),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn handle_session_prompt(&self, message: JsonRpcMessage) -> Result<()> {
        let Some(id) = message.id else {
            return Ok(());
        };
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        let selection = match skill_selection_from_params(&message.params) {
            Ok(selection) => selection,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        let (text, images) = match prompt_from_params(&message.params) {
            Ok(prompt) => prompt,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get(&session_id).cloned()
        };
        let Some(session) = session else {
            self.write_error_value(
                id,
                JSONRPC_INVALID_PARAMS,
                format!("Unknown ACP session id: {session_id}"),
            )
            .await?;
            return Ok(());
        };

        // A skill change cannot ride a steer, and a prompt the daemon rejects
        // as busy is resent as one, which would drop the change. Reject before
        // anything is inserted so the client can retry when the session is
        // idle, instead of believing a selection that never applied.
        let busy = session.prompt_active() || session.router.lock().await.turn_open();
        if let Some(err) = busy_skill_selection_error(&session_id, &selection, busy) {
            self.write_error_value(id, JSONRPC_SERVER_ERROR, err)
                .await?;
            return Ok(());
        }

        // A steering daemon injects a second prompt into the running turn, so
        // prompts may overlap. Slash commands and legacy daemons still take
        // the session one prompt at a time.
        let concurrent =
            session.router.lock().await.steering() && parse_acp_slash_command(&text).is_none();
        if !session.try_begin_prompt(concurrent) {
            self.write_error_value(
                id,
                JSONRPC_SERVER_ERROR,
                format!("Session {session_id} is already processing a prompt"),
            )
            .await?;
            return Ok(());
        }

        session.prompt_tasks.fetch_add(1, Ordering::SeqCst);
        let runtime = self.clone();
        tokio::spawn(async move {
            let completed_session = Arc::clone(&session);
            let mut ticket = PromptTicket::new(Arc::clone(&session));
            let result = runtime
                .run_prompt(id.clone(), session, text, images, selection, &mut ticket)
                .await;
            ticket.release().await;
            if let Err(err) = result {
                let _ = runtime
                    .write_error_value(
                        id,
                        JSONRPC_INTERNAL_ERROR,
                        format!("Prompt failed: {err:#}"),
                    )
                    .await;
            }
            completed_session
                .prompt_tasks
                .fetch_sub(1, Ordering::SeqCst);
            completed_session.prompt_tasks_done.notify_one();
        });
        Ok(())
    }

    async fn handle_session_cancel(&self, message: JsonRpcMessage) -> Result<()> {
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => {
                if let Some(id) = message.id {
                    self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                        .await?;
                }
                return Ok(());
            }
        };
        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get(&session_id).cloned()
        };
        if let Some(session) = session {
            // Prompts still waiting for a turn are withdrawn from the daemon's
            // queue and answer as cancelled. Sent before the cancel, so a steer
            // the daemon has just started as a turn is the turn it stops.
            let withdraw = session.router.lock().await.stop();
            session.sink(self.clone()).apply(withdraw).await;
            // A stop targets the turn this view shows running: our own (or one
            // our prompt adopted), or the peer turn being mirrored (stopping a
            // TUI turn from the ACP client cancels it everywhere). Scoping it
            // means a cancel that arrives after that turn ended cannot hit the
            // next one. With no turn known (an older daemon, or a prompt not
            // yet accepted) it stays session-wide.
            let turn = session.router.lock().await.active_turn_identity();
            let cancel_id = session.next_id();
            let _ = session
                .send(&Request::Cancel {
                    id: cancel_id,
                    turn,
                })
                .await;
        }
        if let Some(id) = message.id {
            self.write_result(id, json!({})).await?;
        }
        Ok(())
    }

    async fn handle_session_detach(&self, message: JsonRpcMessage) -> Result<()> {
        let Some(id) = message.id else {
            return Ok(());
        };
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => {
                return self
                    .write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await;
            }
        };
        let session = self.sessions.lock().await.get(&session_id).cloned();
        if let Some(session) = session {
            if !session.shared_control_support().await.detach {
                return self
                    .write_error_value(
                        id,
                        JSONRPC_METHOD_NOT_FOUND,
                        "Detach is unavailable on this daemon".into(),
                    )
                    .await;
            }
            // Acknowledged before releasing the socket: dropping the adapter
            // alone would otherwise invoke owner disconnect cancellation.
            let prepare_id = session.next_id();
            let mut reply = session.register(prepare_id, PendingKind::Reply).await;
            let prepared = async {
                session
                    .send(&Request::PrepareDisconnect {
                        id: prepare_id,
                        preserve_work: true,
                    })
                    .await?;
                wait_for_done(&mut reply, prepare_id).await
            }
            .await;
            session.unregister(prepare_id).await;
            if let Err(err) = prepared {
                return self
                    .write_error_value(
                        id,
                        JSONRPC_INTERNAL_ERROR,
                        format!("Failed to prepare detach: {err:#}"),
                    )
                    .await;
            }
            self.sessions.lock().await.remove(&session_id);
            let actions = session.router.lock().await.detach_view();
            session.sink(self.clone()).apply(actions).await;
            loop {
                let notified = session.prompt_tasks_done.notified();
                if session.prompt_tasks.load(Ordering::SeqCst) == 0 {
                    break;
                }
                notified.await;
            }
            // Join both writers before answering, including catalog refreshes
            // that already upgraded their weak session reference.
            for task in [&session.catalog_task, &session.reader_task] {
                let handle = task.lock().ok().and_then(|mut slot| slot.take());
                if let Some(handle) = handle {
                    handle.abort();
                    let _ = handle.await;
                }
            }
            session.router.lock().await.close();
        }
        self.write_result(id, json!({})).await
    }

    async fn handle_session_close(&self, message: JsonRpcMessage) -> Result<()> {
        let Some(id) = message.id else {
            return Ok(());
        };
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        let session = self.sessions.lock().await.remove(&session_id);
        if let Some(session) = session {
            // Closing this view is not a user stop, so it must never interrupt
            // another client's turn: it cancels only a turn this view started,
            // scoped to its identity so the daemon ignores the cancel if that
            // turn already ended and a peer's turn has started in the
            // meantime. A turn our prompt adopted from a peer keeps running;
            // the prompt stops following it. Prompts still waiting for a turn
            // are withdrawn from the daemon's queue.
            let actions = session.router.lock().await.close_view();
            session.sink(self.clone()).apply(actions).await;
        }
        self.write_result(id, json!({})).await?;
        Ok(())
    }

    /// `session/list`: stored sessions the client can attach to, newest first.
    async fn handle_session_list(&self, message: JsonRpcMessage) -> Result<()> {
        let Some(id) = message.id else {
            return Ok(());
        };
        let cwd = match optional_cwd_from_params(&message.params) {
            Ok(cwd) => cwd,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        let cursor = match session_list_cursor(&message.params) {
            Ok(cursor) => cursor,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        let live = self.live_sessions().await;
        let entries = jcode_harness_api_server::session_catalog::stored_session_catalog(None);
        let result = session_list_result(entries, cwd.as_deref(), cursor.as_ref(), live.as_deref());
        self.write_result(id, result).await
    }

    /// Session ids the daemon currently has live, from the `History` snapshots
    /// this adapter already received. `None` when no snapshot is available, in
    /// which case `_meta.jcode.live` is omitted rather than guessed.
    async fn live_sessions(&self) -> Option<Vec<String>> {
        let sessions: Vec<Arc<DaemonSession>> =
            self.sessions.lock().await.values().cloned().collect();
        let mut live: Option<Vec<String>> = None;
        for session in sessions {
            if let Some(ids) = session.live_sessions().await {
                let live = live.get_or_insert_with(Vec::new);
                live.extend(ids);
            }
        }
        if let Some(live) = live.as_mut() {
            live.sort_unstable();
            live.dedup();
        }
        live
    }

    async fn handle_set_config_option(&self, message: JsonRpcMessage) -> Result<()> {
        let Some(id) = message.id else {
            return Ok(());
        };
        let session_id = match required_session_id(&message.params) {
            Ok(session_id) => session_id,
            Err(err) => {
                self.write_error_value(id, JSONRPC_INVALID_PARAMS, err)
                    .await?;
                return Ok(());
            }
        };
        let config_id = message
            .params
            .get("configId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let value = message
            .params
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string);
        let (Some(config_id), Some(value)) = (config_id, value) else {
            self.write_error_value(
                id,
                JSONRPC_INVALID_PARAMS,
                "session/set_config_option requires string configId and value".to_string(),
            )
            .await?;
            return Ok(());
        };

        let session = {
            let sessions = self.sessions.lock().await;
            sessions.get(&session_id).cloned()
        };
        let Some(session) = session else {
            self.write_error_value(
                id,
                JSONRPC_INVALID_PARAMS,
                format!("Unknown ACP session id: {session_id}"),
            )
            .await?;
            return Ok(());
        };
        if session.prompt_active() {
            self.write_error_value(
                id,
                JSONRPC_SERVER_ERROR,
                format!("Session {session_id} is processing a prompt; retry when it finishes"),
            )
            .await?;
            return Ok(());
        }

        let request_id = session.next_id();
        let mut rx = session.register(request_id, PendingKind::Reply).await;
        let apply_result = match config_id.as_str() {
            CONFIG_ID_MODEL => {
                session
                    .send(&Request::SetModel {
                        id: request_id,
                        model: value.clone(),
                    })
                    .await?;
                wait_for_model_changed(&session, &mut rx, request_id).await
            }
            CONFIG_ID_EFFORT => {
                let normalized = normalize_effort(&*session.ui_state.lock().await, &value);
                match normalized {
                    Ok(Some(effort)) => {
                        session
                            .send(&Request::SetReasoningEffort {
                                id: request_id,
                                effort,
                                target_session_id: None,
                            })
                            .await?;
                        wait_for_effort_changed(&session, &mut rx, request_id).await
                    }
                    Ok(None) => Ok(()),
                    Err(error) => Err(error),
                }
            }
            other => Err(anyhow::anyhow!("Unknown config option id: {other}")),
        };
        session.unregister(request_id).await;

        match apply_result {
            Ok(()) => {
                let config_options = session_config_options(&*session.ui_state.lock().await);
                // The spec requires the full option set in the response itself.
                self.write_result(id, json!({ "configOptions": config_options }))
                    .await?;
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
                .await?;
            }
            Err(err) => {
                self.write_error_value(
                    id,
                    JSONRPC_SERVER_ERROR,
                    format!("Failed to set {config_id}: {err:#}"),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Compatibility entry points used by ACP hosts that implemented the
    /// pre-configOptions model and reasoning controls. Normalize them through
    /// the standard config option path so both interfaces stay in sync.
    async fn handle_compat_config_option(
        &self,
        mut message: JsonRpcMessage,
        config_id: &str,
        value_fields: &[&str],
        method: &str,
    ) -> Result<()> {
        let value = match compatibility_option_value(&message.params, value_fields, method) {
            Ok(value) => value,
            Err(error) => {
                if let Some(id) = message.id {
                    self.write_error_value(id, JSONRPC_INVALID_PARAMS, error)
                        .await?;
                }
                return Ok(());
            }
        };
        let Some(params) = message.params.as_object_mut() else {
            if let Some(id) = message.id {
                self.write_error_value(
                    id,
                    JSONRPC_INVALID_PARAMS,
                    format!("{method} params must be an object"),
                )
                .await?;
            }
            return Ok(());
        };
        params.insert("configId".to_string(), Value::String(config_id.to_string()));
        params.insert("value".to_string(), Value::String(value));
        self.handle_set_config_option(message).await
    }

    async fn write_available_commands(&self, session_id: &str) -> Result<()> {
        let skills = {
            let sessions = self.sessions.lock().await;
            sessions.get(session_id).map(|session| session.skills())
        }
        .unwrap_or_default();
        let mut commands = acp_available_commands();
        commands.extend(acp_skill_commands(&skills));
        self.write_notification(
            "session/update",
            json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": commands,
                }
            }),
        )
        .await
    }
}

/// Client instance id of one daemon connection.
///
/// The daemon reports the id in every turn origin, so it must be unique per
/// connection, not per adapter process: a process that attaches the same
/// session twice would otherwise recognise its other connection's turn as its
/// own.
fn connection_instance_id(process_instance_id: &str, connection: u64) -> String {
    format!("{process_instance_id}/{connection}")
}
fn random_u64(tag: u64) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(tag);
    hasher.write_u32(std::process::id());
    if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(elapsed.as_nanos());
    }
    hasher.finish()
}

/// Request ids start in the high half of `u64` at a random offset. A peer's
/// `done`/`error` carries the originator's connection-local id, and TUI ids
/// start near zero, so this keeps the two spaces apart.
fn random_request_id_base() -> u64 {
    (random_u64(0x5eed_0001) as u32 as u64) << 32
}

/// Random RFC 4122 version 4 UUID, used to make each adapter process a distinct
/// daemon client instance.
fn random_uuid_v4() -> String {
    let high = random_u64(0x5eed_0002);
    let low = random_u64(0x5eed_0003);
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&high.to_be_bytes());
    bytes[8..].copy_from_slice(&low.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

mod daemon_router;
use daemon_router::*;

mod session_configuration;
use session_configuration::*;

pub(crate) async fn run_acp_command(
    provider_choice: ProviderChoice,
    model: Option<String>,
    provider_profile: Option<String>,
    explicit_tool_profile: bool,
) -> Result<()> {
    crate::env::set_var("JCODE_NON_INTERACTIVE", "1");
    let acp_config = crate::config::config().acp.clone();
    if !explicit_tool_profile {
        crate::env::set_var("JCODE_TOOL_PROFILE", acp_config.tool_profile.trim());
        crate::config::invalidate_config_cache();
    }
    let profile = AcpProfile::parse(&acp_config.profile);
    AcpRuntime::new(profile, provider_choice, model, provider_profile)
        .run()
        .await
}

#[cfg(test)]
mod turn_envelope_tests {
    use super::*;
    use crate::protocol::TurnIdentity;

    fn envelope(event: ServerEvent) -> ServerEvent {
        ServerEvent::TurnEvent {
            end_message: None,
            end: None,
            identity: TurnIdentity {
                runtime_generation: "runtime-test".into(),
                turn_id: 7,
            },
            event: Box::new(event),
        }
    }

    #[test]
    fn mapper_preserves_explicit_wrapped_text_and_tool_events() {
        for event in [
            ServerEvent::TextDelta {
                text: "hello".into(),
            },
            ServerEvent::ToolStart {
                id: "tool-1".into(),
                name: "read".into(),
            },
        ] {
            let plain =
                EventMapper::new("session".into(), AcpProfile::Standard).map_event(event.clone());
            let wrapped = EventMapper::new("session".into(), AcpProfile::Standard)
                .map_event(envelope(envelope(event)));
            assert!(!plain.is_empty());
            assert_eq!(wrapped, plain);
        }
    }

    #[tokio::test]
    async fn reader_unwraps_terminal_events_before_request_waiters() {
        // A daemon without `turn_origin` is read flat, so a wrapped terminal is
        // unwrapped before it reaches the waiter.
        let (client, peer) = crate::transport::stream_pair().unwrap();
        let (reader, writer) = client.into_split();
        let runtime = AcpRuntime::new(AcpProfile::Standard, ProviderChoice::Claude, None, None);
        let session = Arc::new(DaemonSession::new(
            "session",
            None,
            writer,
            "acp-test/0".to_string(),
            1,
        ));
        session.spawn_reader(reader, runtime);
        let mut rx = session.register(42, PendingKind::Reply).await;
        let (_, mut peer_writer) = peer.into_split();
        let event = envelope(envelope(ServerEvent::Done { id: 42 }));
        let mut json = serde_json::to_string(&event).unwrap();
        json.push('\n');
        peer_writer.write_all(json.as_bytes()).await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_done(&mut rx, 42),
        )
        .await
        .expect("wrapped Done must terminate request waiter")
        .unwrap();
    }
}

#[cfg(test)]
mod tests;

mod transport;
use transport::*;
mod session;
use session::*;
mod presentation;
use presentation::*;
mod requests;
use requests::*;
mod prompt;
