use super::dispatch;
use super::provider_init::ProviderChoice;
use crate::protocol::{Request, ServerEvent, TurnEndStatus, TurnOrigin};
use crate::transport::{ReadHalf, WriteHalf};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::sync::mpsc;

const ACP_PROTOCOL_VERSION: u64 = 1;

const JSONRPC_PARSE_ERROR: i64 = -32700;
const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;
const JSONRPC_INTERNAL_ERROR: i64 = -32603;
const JSONRPC_SERVER_ERROR: i64 = -32000;

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

#[derive(Debug)]
struct JsonRpcMessage {
    id: Option<Value>,
    method: Option<String>,
    params: Value,
}

impl JsonRpcMessage {
    fn parse(line: &str) -> std::result::Result<Self, (i64, String)> {
        let value: Value =
            serde_json::from_str(line).map_err(|err| (JSONRPC_PARSE_ERROR, err.to_string()))?;
        let object = value.as_object().ok_or_else(|| {
            (
                JSONRPC_INVALID_REQUEST,
                "JSON-RPC message must be an object".to_string(),
            )
        })?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err((
                JSONRPC_INVALID_REQUEST,
                "JSON-RPC message must include jsonrpc=\"2.0\"".to_string(),
            ));
        }
        Ok(Self {
            id: object.get("id").cloned(),
            method: object
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_string),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        })
    }
}

struct DaemonSession {
    writer: Mutex<WriteHalf>,
    /// Shared with the router, which builds steer recovery requests itself.
    next_request_id: Arc<AtomicU64>,
    /// Client instance id of *this connection*: `{process}/{n}`. Two sessions
    /// in one adapter process must not look like each other's peer, so the
    /// per-process id is not enough.
    instance_id: String,
    /// Event routing state, shared with this connection's reader task. The
    /// reader owns clones of the shared state, never the session itself, so
    /// dropping the session really does stop reading the daemon.
    router: Arc<Mutex<RouterState>>,
    /// `session/prompt` requests in progress. A steering daemon takes several
    /// at once (each joins the running turn); otherwise at most one.
    prompts: AtomicUsize,
    catalog_refresh: Mutex<()>,
    ui_state: Arc<Mutex<SessionUiState>>,
    /// Workspace root reported by the ACP client, used to compose the
    /// project-local skill overlay for slash-skill resolution.
    working_dir: Option<PathBuf>,
    /// Sticky skill selection, resent with every prompt. The daemon treats a
    /// missing `active_skill` as "clear the selection", mirroring the TUI.
    active_skill: Mutex<Option<String>>,
    /// Peer output is held here until the attach handshake has replayed
    /// history, so live peer events cannot interleave with the replay.
    peer_gate: Arc<Mutex<PeerActionGate>>,
    /// Mapper state for the mirrored peer turn, shared by the reader and the
    /// attach flush so a tool call spanning that boundary keeps its input.
    peer_mapper: Arc<Mutex<Option<EventMapper>>>,
    reader_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// Peer actions buffered while a session attach is replaying history.
#[derive(Default)]
struct PeerActionGate {
    ready: bool,
    buffered: Vec<RouteAction>,
}

/// Upper bound on buffered peer actions. A mid-turn attach cannot replay
/// unbounded output; the newest events are the ones still on screen.
const MAX_BUFFERED_PEER_ACTIONS: usize = 512;

/// Drain held peer actions while keeping live dispatch ordered behind them.
///
/// The gate guard is held for the whole flush: a live action that arrives while
/// a held action is being rendered blocks in [`PeerActionGate::hold`] and is
/// therefore rendered after it, never between or ahead of the buffer.
async fn flush_peer_actions<'a, F>(
    gate: &'a Arc<Mutex<PeerActionGate>>,
    mut render: F,
) -> Result<()>
where
    F: FnMut(RouteAction) -> futures::future::BoxFuture<'a, Result<()>>,
{
    let mut guard = gate.lock().await;
    guard.ready = true;
    let buffered = std::mem::take(&mut guard.buffered);
    for action in buffered {
        render(action).await?;
    }
    drop(guard);
    Ok(())
}

/// Executes routing actions for one daemon connection: renders peer output,
/// sends the requests the router asks for, and runs its timers. Holds the
/// session only weakly, like the reader.
#[derive(Clone)]
struct RouteSink {
    acp: AcpRuntime,
    router: Arc<Mutex<RouterState>>,
    ui_state: Arc<Mutex<SessionUiState>>,
    gate: Arc<Mutex<PeerActionGate>>,
    peer_mapper: Arc<Mutex<Option<EventMapper>>>,
    session: std::sync::Weak<DaemonSession>,
}

impl RouteSink {
    fn apply(&self, actions: Vec<RouteAction>) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            for action in actions {
                match action {
                    RouteAction::Send(request) => {
                        let Some(session) = self.session.upgrade() else {
                            continue;
                        };
                        if let Err(err) = session.send(&request).await {
                            crate::logging::debug(&format!(
                                "acp: dropping a routed request: {err:#}"
                            ));
                        }
                    }
                    RouteAction::Wake { after, token } => {
                        let sink = self.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(after).await;
                            if sink.session.strong_count() == 0 {
                                return;
                            }
                            let actions = sink.router.lock().await.wake(token);
                            sink.apply(actions).await;
                        });
                    }
                    action => {
                        if let Err(err) = self
                            .acp
                            .dispatch_route_action(
                                &self.router,
                                &self.ui_state,
                                &self.gate,
                                &self.peer_mapper,
                                action,
                            )
                            .await
                        {
                            crate::logging::debug(&format!("acp: dropping route action: {err:#}"));
                        }
                    }
                }
            }
        })
    }
}

/// One `session/prompt` slot, released exactly once: before the prompt's
/// answer is written (so the client can send the next one at once), or on
/// drop if the prompt task ended early.
struct PromptTicket {
    session: Arc<DaemonSession>,
    prompt_id: Option<u64>,
    released: bool,
}

impl PromptTicket {
    fn new(session: Arc<DaemonSession>) -> Self {
        Self {
            session,
            prompt_id: None,
            released: false,
        }
    }

    async fn release(&mut self) {
        if std::mem::replace(&mut self.released, true) {
            return;
        }
        if let Some(prompt_id) = self.prompt_id {
            self.session.unregister(prompt_id).await;
        }
        self.session.prompts.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for PromptTicket {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.session.prompts.fetch_sub(1, Ordering::SeqCst);
        if let Some(prompt_id) = self.prompt_id {
            let router = Arc::clone(&self.session.router);
            tokio::spawn(async move { router.lock().await.unregister(prompt_id) });
        }
    }
}

impl PeerActionGate {
    /// The attach replay already showed the prompt of the held peer turn
    /// `turn_id`; drop its live user chunk.
    fn mark_prompt_replayed(&mut self, turn_id: &str) {
        if self.ready {
            return;
        }
        for action in &mut self.buffered {
            if let RouteAction::PeerTurnStarted {
                turn_id: held,
                prompt_already_replayed,
                ..
            } = action
                && held == turn_id
            {
                *prompt_already_replayed = true;
            }
        }
    }

    /// Hold `action` while output is deferred. Returns `true` when it was
    /// buffered, `false` when the caller should render it now.
    fn hold(&mut self, action: RouteAction) -> bool {
        if self.ready {
            return false;
        }
        if self.buffered.len() >= MAX_BUFFERED_PEER_ACTIONS {
            // Evict the oldest *streamed* event only. Dropping a turn's
            // `PeerTurnStarted`/`PeerTurnEnded` would leave the client with an
            // unbalanced `_jcode/turn` bracket, so brackets are never evicted.
            match self
                .buffered
                .iter()
                .position(|held| matches!(held, RouteAction::PeerTurnEvent(_)))
            {
                Some(index) => {
                    self.buffered.remove(index);
                }
                None => {
                    // Only brackets are held (one start/end per turn, so this
                    // needs thousands of turns): keep the newest event out
                    // rather than unbalancing a bracket.
                    if matches!(action, RouteAction::PeerTurnEvent(_)) {
                        crate::logging::debug(
                            "acp: dropping peer output while the attach flush is saturated",
                        );
                        return true;
                    }
                }
            }
        }
        self.buffered.push(action);
        true
    }
}

/// Session-scoped provider/model state used to surface ACP `configOptions`
/// (model selector, reasoning effort) and `usage_update` notifications.
#[derive(Clone, Debug, Default)]
struct SessionUiState {
    provider_name: Option<String>,
    model: Option<String>,
    available_models: Vec<String>,
    reasoning_effort: Option<String>,
    available_reasoning_efforts: Option<Vec<String>>,
}

#[derive(Debug, Default)]
struct TurnUsage {
    reported: bool,
    input_tokens: u64,
    output_tokens: u64,
    cached_read_tokens: Option<u64>,
    cached_write_tokens: Option<u64>,
    call_snapshot: (u64, u64, Option<u64>, Option<u64>),
}

impl TurnUsage {
    fn begin_call(&mut self) {
        self.call_snapshot = (0, 0, None, None);
    }

    fn observe_snapshot(
        &mut self,
        input: u64,
        output: u64,
        cache_read: Option<u64>,
        cache_write: Option<u64>,
    ) {
        let previous = self.call_snapshot;
        self.add(
            input.saturating_sub(previous.0),
            output.saturating_sub(previous.1),
            cache_read.map(|value| value.saturating_sub(previous.2.unwrap_or(0))),
            cache_write.map(|value| value.saturating_sub(previous.3.unwrap_or(0))),
        );
        self.call_snapshot = (
            input.max(previous.0),
            output.max(previous.1),
            cache_read
                .map(|value| value.max(previous.2.unwrap_or(0)))
                .or(previous.2),
            cache_write
                .map(|value| value.max(previous.3.unwrap_or(0)))
                .or(previous.3),
        );
    }
    fn add(
        &mut self,
        input_tokens: u64,
        output_tokens: u64,
        cached_read_tokens: Option<u64>,
        cached_write_tokens: Option<u64>,
    ) {
        self.reported = true;
        self.input_tokens = self.input_tokens.saturating_add(input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(output_tokens);
        add_optional_tokens(&mut self.cached_read_tokens, cached_read_tokens);
        add_optional_tokens(&mut self.cached_write_tokens, cached_write_tokens);
    }

    fn to_acp(&self) -> Option<Value> {
        if !self.reported {
            return None;
        }

        let total_tokens = self
            .input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cached_read_tokens.unwrap_or(0))
            .saturating_add(self.cached_write_tokens.unwrap_or(0));
        let mut usage = json!({
            "totalTokens": total_tokens,
            "inputTokens": self.input_tokens,
            "outputTokens": self.output_tokens,
        });
        let object = usage.as_object_mut().expect("usage is an object");
        if let Some(tokens) = self.cached_read_tokens {
            object.insert("cachedReadTokens".to_string(), json!(tokens));
        }
        if let Some(tokens) = self.cached_write_tokens {
            object.insert("cachedWriteTokens".to_string(), json!(tokens));
        }
        Some(usage)
    }
}

fn add_optional_tokens(total: &mut Option<u64>, tokens: Option<u64>) {
    if let Some(tokens) = tokens {
        *total = Some(total.unwrap_or(0).saturating_add(tokens));
    }
}

fn prompt_response(stop_reason: &str, usage: &TurnUsage) -> Value {
    let mut response = json!({ "stopReason": stop_reason });
    if let Some(usage) = usage.to_acp() {
        response
            .as_object_mut()
            .expect("prompt response is an object")
            .insert("usage".to_string(), usage);
    }
    response
}

impl SessionUiState {
    fn from_history_fields(
        provider_name: Option<String>,
        provider_model: Option<String>,
        available_models: Vec<String>,
        available_reasoning_efforts: Option<Vec<String>>,
        reasoning_effort: Option<String>,
    ) -> Self {
        Self {
            provider_name,
            model: provider_model,
            available_models,
            reasoning_effort,
            available_reasoning_efforts,
        }
    }

    fn context_limit(&self) -> u64 {
        self.model
            .as_deref()
            .and_then(|model| {
                crate::provider::context_limit_for_model_with_provider(
                    model,
                    self.provider_name.as_deref(),
                )
            })
            .unwrap_or(crate::provider::DEFAULT_CONTEXT_LIMIT) as u64
    }
}

impl DaemonSession {
    fn new(
        session_id: impl Into<String>,
        working_dir: Option<PathBuf>,
        writer: WriteHalf,
        instance_id: String,
        next_request_id: u64,
    ) -> Self {
        let next_request_id = Arc::new(AtomicU64::new(next_request_id));
        let mut router = RouterState::new(instance_id.clone(), session_id);
        router.share_request_ids(Arc::clone(&next_request_id));
        Self {
            writer: Mutex::new(writer),
            next_request_id,
            instance_id: instance_id.clone(),
            router: Arc::new(Mutex::new(router)),
            prompts: AtomicUsize::new(0),
            catalog_refresh: Mutex::new(()),
            ui_state: Arc::new(Mutex::new(SessionUiState::default())),
            working_dir,
            active_skill: Mutex::new(None),
            peer_gate: Arc::new(Mutex::new(PeerActionGate::default())),
            peer_mapper: Arc::new(Mutex::new(None)),
            reader_task: std::sync::Mutex::new(None),
        }
    }

    /// Client instance id reported to the daemon for this connection.
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Effective skills for this session: global skills plus the workspace's
    /// project-local overlay, loaded fresh so newly installed skills resolve.
    fn skills(&self) -> crate::skill::SkillRegistry {
        crate::skill::SkillRegistry::load_for_working_dir(self.working_dir.as_deref())
            .unwrap_or_default()
    }

    async fn set_ui_state(&self, state: SessionUiState) {
        *self.ui_state.lock().await = state;
    }

    fn next_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Session id the daemon assigned. Authoritative only after the first
    /// `History` arrives, which is why the router owns it.
    async fn session_id(&self) -> String {
        self.router.lock().await.session_id().to_string()
    }

    async fn live_sessions(&self) -> Option<Vec<String>> {
        self.router
            .lock()
            .await
            .live_sessions()
            .map(<[String]>::to_vec)
    }

    async fn send(&self, request: &Request) -> Result<()> {
        let mut json = serde_json::to_string(request)?;
        json.push('\n');
        let mut writer = self.writer.lock().await;
        writer.write_all(json.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }

    async fn register(
        &self,
        request_id: u64,
        kind: PendingKind,
    ) -> mpsc::UnboundedReceiver<ServerEvent> {
        self.router.lock().await.register(request_id, kind)
    }

    async fn unregister(&self, request_id: u64) {
        self.router.lock().await.unregister(request_id);
    }

    /// Claim a prompt slot. `concurrent` prompts may share the session with
    /// others; any other prompt needs it to itself.
    fn try_begin_prompt(&self, concurrent: bool) -> bool {
        if concurrent {
            self.prompts.fetch_add(1, Ordering::SeqCst);
            return true;
        }
        self.prompts
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn prompt_active(&self) -> bool {
        self.prompts.load(Ordering::SeqCst) > 0
    }

    /// Everything a routing action needs, without keeping the session alive.
    fn sink(self: &Arc<Self>, acp: AcpRuntime) -> RouteSink {
        RouteSink {
            acp,
            router: Arc::clone(&self.router),
            ui_state: Arc::clone(&self.ui_state),
            gate: Arc::clone(&self.peer_gate),
            peer_mapper: Arc::clone(&self.peer_mapper),
            session: Arc::downgrade(self),
        }
    }

    /// Read the daemon connection for the lifetime of the session, routing
    /// every event. A single undecodable line never ends the session: a newer
    /// daemon's event that this build does not know is skipped and logged.
    fn spawn_reader(self: &Arc<Self>, reader: ReadHalf, acp: AcpRuntime) {
        // Only shared state, never `Arc<DaemonSession>`: a task holding the
        // session would keep it (and its socket) alive forever, because the
        // session's `Drop` is what aborts this task.
        let sink = self.sink(acp.clone());
        let router = Arc::clone(&self.router);
        let ui_state = Arc::clone(&self.ui_state);
        let session = Arc::downgrade(self);
        let (catalog_tx, mut catalog_rx) = tokio::sync::watch::channel(());
        let catalog_acp = acp.clone();
        tokio::spawn(async move {
            while catalog_rx.changed().await.is_ok() {
                let Some(session) = session.upgrade() else {
                    break;
                };
                match request_model_catalog(&session).await {
                    Ok(_) => {
                        if let Err(err) = catalog_acp.write_config_option_update(&session).await {
                            crate::logging::debug(&format!("acp: config update failed: {err}"));
                        }
                    }
                    Err(err) => {
                        crate::logging::debug(&format!("acp: catalog refresh failed: {err}"))
                    }
                }
            }
        });
        let handle = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(err) => {
                        crate::logging::debug(&format!("acp: daemon read failed: {err}"));
                        break;
                    }
                }
                let Some(event) = decode_daemon_line(&line) else {
                    continue;
                };
                if matches!(&event, ServerEvent::AvailableModelsUpdated { .. }) {
                    catalog_tx.send_replace(());
                }
                let catalog_reply = router.lock().await.is_catalog_reply(&event);
                if catalog_reply {
                    apply_catalog_state(&mut *ui_state.lock().await, &event);
                }
                let actions = router.lock().await.route(event);
                sink.apply(actions).await;
            }
            // Release every waiter so a dead connection fails prompts instead
            // of leaving them blocked forever, and close any peer turn still
            // open so the client does not keep a running turn.
            let closing = router.lock().await.close();
            sink.apply(closing).await;
        });
        if let Ok(mut slot) = self.reader_task.lock() {
            *slot = Some(handle);
        }
    }
}

impl Drop for DaemonSession {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.reader_task.lock()
            && let Some(handle) = slot.take()
        {
            handle.abort();
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
                self.sessions
                    .lock()
                    .await
                    .insert(session_id.clone(), session);
                let mut result = json!({ "sessionId": session_id });
                insert_session_configuration(&mut result, &state);
                self.write_result(id, result).await?;
                self.write_available_commands(&session_id).await?;
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
                self.sessions.lock().await.insert(attached_id, session);
                let mut result = json!({});
                insert_session_configuration(&mut result, &state);
                self.write_result(id, result).await?;
                self.write_available_commands(&session_id).await?;
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
        let explicit_skill = explicit_skill_from_params(&message.params);
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

        let runtime = self.clone();
        tokio::spawn(async move {
            let mut ticket = PromptTicket::new(Arc::clone(&session));
            let result = runtime
                .run_prompt(
                    id.clone(),
                    session,
                    text,
                    images,
                    explicit_skill,
                    &mut ticket,
                )
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

    async fn ensure_daemon(&self) -> Result<()> {
        if dispatch::server_is_running().await {
            return Ok(());
        }
        dispatch::spawn_server(
            &self.provider_choice,
            self.model.as_deref(),
            self.provider_profile.as_deref(),
        )
        .await
    }

    async fn connect_daemon(&self) -> Result<(ReadHalf, WriteHalf)> {
        self.ensure_daemon().await?;
        let stream = crate::server::connect_socket(&crate::server::socket_path()).await?;
        Ok(stream.into_split())
    }

    /// Wrap a daemon connection: start its reader, then learn whether the
    /// daemon attributes turns (`turn_origin` in `Pong::capabilities`) before
    /// subscribing, so the subscription can ask for turn identity.
    async fn start_daemon_session(
        &self,
        session_id: &str,
        working_dir: Option<PathBuf>,
        reader: ReadHalf,
        writer: WriteHalf,
    ) -> Result<Arc<DaemonSession>> {
        let connection = self.connection_seq.fetch_add(1, Ordering::Relaxed);
        let session = Arc::new(DaemonSession::new(
            session_id,
            working_dir,
            writer,
            connection_instance_id(&self.instance_id, connection),
            self.request_id_base,
        ));
        session.spawn_reader(reader, self.clone());
        let capabilities = probe_capabilities(&session).await?;
        let mut router = session.router.lock().await;
        router.set_turn_origin(capabilities.turn_origin);
        router.set_soft_interrupt_ids(capabilities.soft_interrupt_ids);
        drop(router);
        Ok(session)
    }

    async fn create_new_session(&self, cwd: PathBuf) -> Result<Arc<DaemonSession>> {
        let (reader, writer) = self.connect_daemon().await?;
        self.create_new_session_connected(cwd, reader, writer).await
    }

    async fn create_new_session_connected(
        &self,
        cwd: PathBuf,
        reader: ReadHalf,
        writer: WriteHalf,
    ) -> Result<Arc<DaemonSession>> {
        let session = self
            .start_daemon_session("", Some(cwd.clone()), reader, writer)
            .await?;
        let subscribe_id = session.next_id();
        let mut subscribe_rx = session.register(subscribe_id, PendingKind::Reply).await;
        session
            .send(&Request::Subscribe {
                system_prompt: None,
                supports_pdf_panels: false,
                supports_user_response: false,
                supports_server_failover: true,
                supports_turn_identity: session.router.lock().await.supports_turn_origin(),
                crash_on_disconnect: false,
                continue_on_disconnect: false,
                id: subscribe_id,
                working_dir: Some(cwd.display().to_string()),
                selfdev: None,
                target_session_id: None,
                client_instance_id: Some(session.instance_id().to_string()),
                client_has_local_history: false,
                history_base: None,
                allow_session_takeover: false,
                terminal_env: crate::terminal_launch::snapshot_client_terminal_env(),
                client_name: None,
            })
            .await?;
        wait_for_done(&mut subscribe_rx, subscribe_id).await?;
        session.unregister(subscribe_id).await;
        let history = request_history(&session).await?;
        let ui_state = match history {
            ServerEvent::History {
                provider_name,
                provider_model,
                available_models,
                available_reasoning_efforts,
                reasoning_effort,
                ..
            } => SessionUiState::from_history_fields(
                provider_name,
                provider_model,
                available_models,
                available_reasoning_efforts,
                reasoning_effort,
            ),
            other => anyhow::bail!("expected history after session creation, got {other:?}"),
        };
        session.set_ui_state(ui_state).await;
        self.release_peer_actions(&session).await?;
        Ok(session)
    }

    async fn attach_existing_session(
        &self,
        target_session_id: String,
        cwd: PathBuf,
        replay_history: bool,
    ) -> Result<Arc<DaemonSession>> {
        let (reader, writer) = self.connect_daemon().await?;
        self.attach_existing_session_connected(
            target_session_id,
            cwd,
            replay_history,
            reader,
            writer,
        )
        .await
    }

    async fn attach_existing_session_connected(
        &self,
        target_session_id: String,
        cwd: PathBuf,
        replay_history: bool,
        reader: ReadHalf,
        writer: WriteHalf,
    ) -> Result<Arc<DaemonSession>> {
        let session = self
            .start_daemon_session(&target_session_id, Some(cwd.clone()), reader, writer)
            .await?;
        let resume_id = session.next_id();
        let mut resume_rx = session.register(resume_id, PendingKind::Reply).await;
        session
            .send(&Request::Subscribe {
                system_prompt: None,
                supports_pdf_panels: false,
                supports_user_response: false,
                supports_server_failover: true,
                supports_turn_identity: session.router.lock().await.supports_turn_origin(),
                crash_on_disconnect: false,
                continue_on_disconnect: false,
                id: resume_id,
                working_dir: Some(cwd.display().to_string()),
                selfdev: None,
                target_session_id: Some(target_session_id.clone()),
                client_instance_id: Some(session.instance_id().to_string()),
                client_has_local_history: false,
                history_base: None,
                allow_session_takeover: false,
                terminal_env: crate::terminal_launch::snapshot_client_terminal_env(),
                client_name: None,
            })
            .await?;

        let mut ui_state = SessionUiState::default();
        loop {
            let Some(event) = resume_rx.recv().await else {
                anyhow::bail!("Jcode daemon disconnected during session attach");
            };
            match event {
                ServerEvent::Ack { .. } => {}
                ServerEvent::History {
                    session_id,
                    messages,
                    provider_name,
                    provider_model,
                    available_models,
                    available_reasoning_efforts,
                    reasoning_effort,
                    ..
                } => {
                    ui_state = SessionUiState::from_history_fields(
                        provider_name,
                        provider_model,
                        available_models,
                        available_reasoning_efforts,
                        reasoning_effort,
                    );
                    if replay_history {
                        self.replay_history(&session_id, messages).await?;
                    }
                }
                ServerEvent::Done { id } if id == resume_id => break,
                ServerEvent::Error { id, message, .. } if id == resume_id => {
                    anyhow::bail!(message);
                }
                // Every non-reply event is routed (and rendered) by the
                // connection reader, so only replies can reach this waiter.
                _ => {}
            }
        }

        session.unregister(resume_id).await;
        session.set_ui_state(ui_state).await;
        // Flush what arrived during the handshake after the replay, and before
        // the caller answers `session/load`.
        self.release_peer_actions(&session).await?;
        Ok(session)
    }

    async fn replay_history(
        &self,
        session_id: &str,
        messages: Vec<crate::protocol::HistoryMessage>,
    ) -> Result<()> {
        for update in history_replay_updates(session_id, &messages) {
            self.write_notification("session/update", update).await?;
        }
        Ok(())
    }

    async fn run_prompt(
        &self,
        rpc_id: Value,
        session: Arc<DaemonSession>,
        text: String,
        images: Vec<(String, String)>,
        explicit_skill: Option<String>,
        ticket: &mut PromptTicket,
    ) -> Result<()> {
        let mut subagent = None;
        if let Some(command) = parse_acp_slash_command(&text) {
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

        let mut text = text;
        let selected_skill = if subagent.is_some() {
            None
        } else {
            // Explicit client metadata wins. Otherwise resolve `/skill [prompt]`
            // (or `$skill [prompt]`, as some ACP hosts serialize command names)
            // against the session's registry. Unknown or path-like slash text is
            // passed through to the model unchanged.
            let skills = session.skills();
            match explicit_skill {
                Some(name) => Some(name),
                None => match resolve_acp_skill_invocation(&skills, &text) {
                    Some((name, prompt)) => {
                        text = prompt.unwrap_or_default();
                        Some(name)
                    }
                    None => None,
                },
            }
        };
        if let Some(name) = selected_skill {
            let skills = session.skills();
            let Some(skill) = skills.get(&name) else {
                ticket.release().await;
                self.write_error_value(
                    rpc_id,
                    JSONRPC_INVALID_PARAMS,
                    format!("Unknown skill: {name}"),
                )
                .await?;
                return Ok(());
            };
            let notice = format!("Activated skill: {} - {}", skill.name, skill.description);
            *session.active_skill.lock().await = Some(name);
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
            if text.trim().is_empty() && images.is_empty() {
                // Bare `/skill`: select it for the following prompts, like the TUI.
                ticket.release().await;
                self.write_result(rpc_id, prompt_response("end_turn", &TurnUsage::default()))
                    .await?;
                return Ok(());
            }
        }
        let active_skill = session.active_skill.lock().await.clone();

        let prompt_id = session.next_id();
        let mut prompt_rx = session.register(prompt_id, PendingKind::Prompt).await;
        ticket.prompt_id = Some(prompt_id);

        let request = match subagent {
            Some(spec) => Request::RunSubagent {
                id: prompt_id,
                prompt: spec.prompt,
                subagent_type: spec.subagent_type,
                model: spec.model,
                session_id: None,
            },
            None => prompt_request(
                &mut *session.router.lock().await,
                prompt_id,
                text,
                images,
                active_skill,
            ),
        };
        let send_result = session.send(&request).await;
        if let Err(err) = send_result {
            ticket.release().await;
            return Err(err);
        }
        let follow_up = session.router.lock().await.prompt_sent(prompt_id);
        session.sink(self.clone()).apply(follow_up).await;
        self.stream_prompt_events(rpc_id, &session, prompt_id, &mut prompt_rx, ticket)
            .await
    }

    async fn run_command_prompt(
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
    async fn stream_prompt_events(
        &self,
        rpc_id: Value,
        session: &Arc<DaemonSession>,
        prompt_id: u64,
        prompt_rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
        ticket: &mut PromptTicket,
    ) -> Result<()> {
        let session_id = session.session_id().await;
        let scope = format!("{session_id}:own:{prompt_id}");
        let mut mapper = EventMapper::with_scope(session_id.clone(), self.profile, scope.clone());
        let mut stop_reason = "end_turn".to_string();
        let mut turn_usage = TurnUsage::default();
        let mut steers = 0u32;
        loop {
            let event = match next_routed(prompt_rx).await {
                Ok(event) => event,
                Err(err) => {
                    ticket.release().await;
                    return Err(err);
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
                // This prompt adopted a peer turn announced by `_jcode/turn`
                // "started": close that bracket after the turn's output and
                // before the answer.
                ServerEvent::TurnEnded {
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

        ticket.release().await;
        self.write_result(rpc_id, prompt_response(&stop_reason, &turn_usage))
            .await?;
        Ok(())
    }

    async fn run_session_command(
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
    async fn write_usage_update(
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

    async fn write_config_option_update(&self, session: &DaemonSession) -> Result<()> {
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

    async fn write_result(&self, id: Value, result: Value) -> Result<()> {
        self.write_value(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .await
    }

    async fn write_error_value(&self, id: Value, code: i64, message: String) -> Result<()> {
        self.write_value(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message,
            }
        }))
        .await
    }

    async fn write_notification(&self, method: &str, params: Value) -> Result<()> {
        self.write_value(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn write_jcode_extension_event(
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
    async fn dispatch_route_action(
        &self,
        router: &Arc<Mutex<RouterState>>,
        ui_state: &Arc<Mutex<SessionUiState>>,
        gate: &Arc<Mutex<PeerActionGate>>,
        peer_mapper: &Arc<Mutex<Option<EventMapper>>>,
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
        self.apply_peer_action(router, ui_state, peer_mapper, action)
            .await
    }

    /// Flush peer output held during an attach handshake, then let live peer
    /// events through.
    async fn release_peer_actions(&self, session: &Arc<DaemonSession>) -> Result<()> {
        flush_peer_actions(&session.peer_gate, |action| {
            Box::pin(async move {
                self.apply_peer_action(
                    &session.router,
                    &session.ui_state,
                    &session.peer_mapper,
                    action,
                )
                .await
            })
        })
        .await
    }

    /// Render one peer-turn action for the ACP client.
    async fn apply_peer_action(
        &self,
        router: &Arc<Mutex<RouterState>>,
        ui_state: &Arc<Mutex<SessionUiState>>,
        peer_mapper: &Arc<Mutex<Option<EventMapper>>>,
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
                *peer_mapper.lock().await = Some(EventMapper::with_scope(
                    session_id.clone(),
                    self.profile,
                    format!("{session_id}:peer:{turn_id}"),
                ));
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
    async fn write_turn_notification(
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
    async fn apply_session_state(
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

    async fn write_value(&self, value: Value) -> Result<()> {
        let mut stdout = self.stdout.lock().await;
        let mut line = serde_json::to_string(&value)?;
        line.push('\n');
        stdout.write_all(line.as_bytes()).await?;
        stdout.flush().await?;
        Ok(())
    }
}

/// `session/update` payload for a `usage_update` notification.
fn usage_update_payload(
    session_id: &str,
    state: &SessionUiState,
    input: u64,
    cache_read_input: Option<u64>,
    cache_creation_input: Option<u64>,
) -> Value {
    let provider_name = state.provider_name.clone().unwrap_or_default();
    let used = crate::compaction::effective_context_tokens_from_usage(
        &provider_name,
        input,
        cache_read_input,
        cache_creation_input,
    );
    json!({
        "sessionId": session_id,
        "update": {
            "sessionUpdate": "usage_update",
            "used": used,
            "size": state.context_limit(),
        }
    })
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

/// Wire name of a turn origin kind.
fn turn_origin_kind(origin: &TurnOrigin) -> &'static str {
    match origin.kind {
        crate::protocol::TurnOriginKind::Client => "client",
        crate::protocol::TurnOriginKind::Swarm => "swarm",
        crate::protocol::TurnOriginKind::Background => "background",
        crate::protocol::TurnOriginKind::Relay => "relay",
        crate::protocol::TurnOriginKind::Debug => "debug",
    }
}

/// Randomness without a new dependency: `RandomState` is seeded from OS
/// entropy per instance, so hashing distinct inputs gives unrelated values.
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

/// `session/update` for the prompt that started a mirrored peer turn.
fn peer_user_message_chunk(session_id: &str, turn_id: &str, content: &str) -> Value {
    user_message_chunk(session_id, format!("{session_id}:{turn_id}:user"), content)
}

/// `session/update` showing another client's message.
fn user_message_chunk(session_id: &str, message_id: String, content: &str) -> Value {
    json!({
        "sessionId": session_id,
        "update": {
            "sessionUpdate": "user_message_chunk",
            "messageId": message_id,
            "content": { "type": "text", "text": content },
        }
    })
}

/// `_jcode/turn` state of an ended turn.
fn turn_end_state(status: TurnEndStatus) -> &'static str {
    match status {
        TurnEndStatus::Completed => "completed",
        TurnEndStatus::Failed => "failed",
        TurnEndStatus::Cancelled => "canceled",
    }
}

/// `session/update` notifications that replay stored history.
///
/// Each message carries a `messageId`, so a client groups replayed chunks the
/// same way it groups live ones and never merges two consecutive same-role
/// messages. The id is derived from the *stored* message's own index whenever
/// the daemon reports one: unlike the position in `messages`, that index does
/// not move when rendering skips stored messages (tool-only rounds) or when the
/// client asked for a delta, so a reload cannot overwrite the wrong item.
/// Synthetic rows (which have no stored message) fall back to their position.
fn history_replay_updates(
    session_id: &str,
    messages: &[crate::protocol::HistoryMessage],
) -> Vec<Value> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let update_name = match message.role.as_str() {
                "user" => "user_message_chunk",
                "assistant" => "agent_message_chunk",
                _ => "agent_message_chunk",
            };
            let message_id = match message.stored_index {
                Some(stored) => format!("{session_id}:msg:{stored}"),
                None => format!("{session_id}:history:{index}"),
            };
            json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": update_name,
                    "messageId": message_id,
                    "content": {
                        "type": "text",
                        "text": message.content,
                    }
                }
            })
        })
        .collect()
}

/// How long to wait for the daemon's `Pong` before treating it as a daemon
/// without turn attribution.
const CAPABILITY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The request that sends prompt `prompt_id`, tracked by the router until a
/// turn takes it.
///
/// A steering daemon gets a soft interrupt, like the TUI and SDK clients send:
/// an idle session runs it as a new turn, a busy one injects it into the
/// running turn (ours or a peer's) instead of rejecting it. A soft interrupt
/// cannot carry the sticky skill selection, so such a prompt goes as a
/// `Message` first and steers only if the session turns out to be busy. A
/// legacy daemon always gets a `Message`.
fn prompt_request(
    router: &mut RouterState,
    prompt_id: u64,
    text: String,
    images: Vec<(String, String)>,
    active_skill: Option<String>,
) -> Request {
    if router.steering() && active_skill.is_none() {
        router.track_prompt(
            prompt_id,
            PromptDelivery::Steer,
            text.clone(),
            images.clone(),
        );
        // Untracked only once the connection closed; the send then fails.
        if let Some(request) = router.steer_request(prompt_id) {
            return request;
        }
    }
    router.track_prompt(
        prompt_id,
        PromptDelivery::Message,
        text.clone(),
        images.clone(),
    );
    Request::Message {
        id: prompt_id,
        content: text,
        images,
        system_reminder: None,
        active_skill,
        no_reply: false,
    }
}

/// The daemon capabilities the adapter adapts to, from `Pong`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DaemonCapabilities {
    /// Turns are attributed (`turn_origin`).
    turn_origin: bool,
    /// Soft interrupts carry client ids (`soft_interrupt_ids`); with
    /// `turn_origin`, prompts are steered.
    soft_interrupt_ids: bool,
}

impl DaemonCapabilities {
    fn from_pong(capabilities: &[String]) -> Self {
        let has = |name: &str| capabilities.iter().any(|capability| capability == name);
        Self {
            turn_origin: has(crate::protocol::SERVER_CAPABILITY_TURN_ORIGIN),
            soft_interrupt_ids: has("soft_interrupt_ids"),
        }
    }
}

/// The daemon's capabilities, from `Pong`.
///
/// Sent before `Subscribe`: the daemon answers a `Ping` on a fresh connection
/// and keeps it open for the subscription. A daemon that does not answer in
/// time, or does not advertise `turn_origin`, is read with the legacy flat
/// routing.
async fn probe_capabilities(session: &Arc<DaemonSession>) -> Result<DaemonCapabilities> {
    let id = session.next_id();
    let mut rx = session.register(id, PendingKind::Reply).await;
    let result = async {
        session.send(&Request::Ping { id }).await?;
        let reply = tokio::time::timeout(CAPABILITY_PROBE_TIMEOUT, async {
            loop {
                match next_routed(&mut rx).await? {
                    ServerEvent::Pong { capabilities, .. } => {
                        return Ok::<_, anyhow::Error>(DaemonCapabilities::from_pong(
                            &capabilities,
                        ));
                    }
                    ServerEvent::Error { .. } => return Ok(DaemonCapabilities::default()),
                    _ => {}
                }
            }
        })
        .await;
        match reply {
            Ok(reply) => reply,
            Err(_) => {
                crate::logging::debug("acp: daemon did not answer the capability probe");
                Ok(DaemonCapabilities::default())
            }
        }
    }
    .await;
    session.unregister(id).await;
    result
}

/// Next routed event for a waiter. A closed channel means the daemon
/// connection ended, which callers must surface as an error instead of waiting
/// forever.
async fn next_routed(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Result<ServerEvent> {
    match rx.recv().await {
        Some(event) => Ok(event),
        None => anyhow::bail!("Jcode daemon disconnected"),
    }
}

async fn wait_for_done(
    rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
    request_id: u64,
) -> Result<()> {
    loop {
        match next_routed(rx).await? {
            ServerEvent::Ack { .. } => {}
            ServerEvent::Done { id } if id == request_id => return Ok(()),
            ServerEvent::Error { id, message, .. } if id == request_id => anyhow::bail!(message),
            _ => {}
        }
    }
}

async fn request_history(session: &Arc<DaemonSession>) -> Result<ServerEvent> {
    let id = session.next_id();
    let mut rx = session.register(id, PendingKind::Reply).await;
    session.send(&Request::GetHistory { id }).await?;
    let result = loop {
        match next_routed(&mut rx).await {
            Ok(ServerEvent::Ack { .. }) => {}
            Ok(event @ ServerEvent::History { .. }) => break Ok(event),
            Ok(ServerEvent::Error { message, .. }) => break Err(anyhow::anyhow!(message)),
            Ok(_) => {}
            Err(err) => break Err(err),
        }
    };
    session.unregister(id).await;
    result
}

async fn request_model_catalog(session: &Arc<DaemonSession>) -> Result<ServerEvent> {
    let _refresh = session.catalog_refresh.lock().await;
    let id = session.next_id();
    let mut rx = session.register(id, PendingKind::Catalog).await;
    session
        .send(&Request::GetModelCatalog {
            id,
            subscribe_usage_updates: false,
        })
        .await?;
    let result = loop {
        match next_routed(&mut rx).await {
            Ok(ServerEvent::Ack { .. }) => {}
            Ok(event @ ServerEvent::History { .. }) => break Ok(event),
            Ok(ServerEvent::Error { message, .. }) => break Err(anyhow::anyhow!(message)),
            Ok(_) => {}
            Err(err) => break Err(err),
        }
    };
    session.unregister(id).await;
    result
}

mod daemon_router;
use daemon_router::*;

mod session_configuration;
use session_configuration::*;

async fn wait_for_model_changed(
    session: &DaemonSession,
    rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
    request_id: u64,
) -> Result<()> {
    loop {
        match next_routed(rx).await? {
            ServerEvent::Ack { .. } => {}
            ServerEvent::ModelChanged {
                id,
                model,
                provider_name,
                reasoning_effort,
                available_reasoning_efforts,
                error,
                ..
            } if id == request_id => {
                if let Some(error) = error {
                    anyhow::bail!(error);
                }
                let mut state = session.ui_state.lock().await;
                state.reasoning_effort = reasoning_effort;
                state.available_reasoning_efforts = available_reasoning_efforts;
                state.model = Some(model);
                if provider_name.is_some() {
                    state.provider_name = provider_name;
                }
                return Ok(());
            }
            ServerEvent::Error { id, message, .. } if id == request_id => {
                anyhow::bail!(message)
            }
            _ => {}
        }
    }
}

/// Manual compaction only starts a background summary; the daemon applies it
/// at the start of the next turn, so the result message is the whole reply.
async fn wait_for_compact_result(
    rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
    request_id: u64,
) -> Result<String> {
    loop {
        match next_routed(rx).await? {
            ServerEvent::CompactResult {
                id,
                message,
                success,
            } if id == request_id => {
                if success {
                    return Ok(message);
                }
                anyhow::bail!(message)
            }
            ServerEvent::Error { id, message, .. } if id == request_id => {
                anyhow::bail!(message)
            }
            _ => {}
        }
    }
}

async fn wait_for_effort_changed(
    session: &DaemonSession,
    rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
    request_id: u64,
) -> Result<()> {
    loop {
        match next_routed(rx).await? {
            ServerEvent::Ack { .. } => {}
            ServerEvent::ReasoningEffortChanged { id, effort, error } if id == request_id => {
                if let Some(error) = error {
                    anyhow::bail!(error);
                }
                let mut state = session.ui_state.lock().await;
                state.reasoning_effort = effort;
                return Ok(());
            }
            ServerEvent::Error { id, message, .. } if id == request_id => {
                anyhow::bail!(message)
            }
            _ => {}
        }
    }
}

struct EventMapper {
    session_id: String,
    profile: AcpProfile,
    current_tool_id: Option<String>,
    tool_inputs: HashMap<String, String>,
    /// Namespace for the `messageId` of streamed assistant messages, e.g.
    /// `{session}:own:{request id}`. Chunks of one message share an id so
    /// clients group them, and the next message in the same turn gets the next
    /// index.
    message_scope: String,
    message_index: u64,
}

impl EventMapper {
    /// A mapper whose streamed messages are scoped to `{session}:stream`.
    #[cfg(test)]
    fn new(session_id: String, profile: AcpProfile) -> Self {
        let scope = format!("{session_id}:stream");
        Self::with_scope(session_id, profile, scope)
    }

    fn with_scope(session_id: String, profile: AcpProfile, message_scope: String) -> Self {
        Self {
            session_id,
            profile,
            current_tool_id: None,
            tool_inputs: HashMap::new(),
            message_scope,
            message_index: 0,
        }
    }

    /// Stable id for the assistant message currently being streamed.
    fn message_id(&self) -> String {
        format!("{}:{}", self.message_scope, self.message_index)
    }

    fn agent_chunk(&self, text: String) -> Value {
        agent_message_chunk_with_id(text, self.message_id())
    }

    fn map_event(&mut self, event: ServerEvent) -> Vec<Value> {
        // Routing already classified the event by its turn scope; anything
        // still wrapped is unwrapped here only for mapping.
        match super::commands::unwrap_turn_event(event) {
            // One assistant message ended; the next chunk starts a new one.
            ServerEvent::MessageEnd { .. } => {
                self.message_index = self.message_index.saturating_add(1);
                Vec::new()
            }
            ServerEvent::TextDelta { text } => vec![self.agent_chunk(text)],
            ServerEvent::TextReplace { text } => vec![self.agent_chunk(text)],
            // ACP chunks cannot retract prior output. Surface the corrected
            // source links rather than silently discarding provider metadata.
            ServerEvent::TextSuffixReplace { new_text, .. } => {
                vec![self.agent_chunk(format!("\n{new_text}"))]
            }
            ServerEvent::ToolStart { id, name } => {
                self.current_tool_id = Some(id.clone());
                self.tool_inputs.entry(id.clone()).or_default();
                vec![json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": id,
                    "title": tool_title(&name),
                    "kind": tool_kind(&name),
                    "status": "pending",
                })]
            }
            ServerEvent::ToolInput { id, delta } => {
                // Older daemons omit the ID. Only those events should use the
                // most recently started tool, since keyed streams can overlap.
                let Some(tool_id) = id.or_else(|| self.current_tool_id.clone()) else {
                    return Vec::new();
                };
                let buffer = self.tool_inputs.entry(tool_id.clone()).or_default();
                buffer.push_str(&delta);
                let Some(raw_input) = parse_json_object(buffer) else {
                    return Vec::new();
                };
                vec![json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": tool_id,
                    "rawInput": raw_input,
                })]
            }
            ServerEvent::ToolExec { id, name } => {
                self.current_tool_id = Some(id.clone());
                let mut update = json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": id,
                    "title": tool_title(&name),
                    "kind": tool_kind(&name),
                    "status": "in_progress",
                });
                if let Some(input) = self
                    .tool_inputs
                    .get(update["toolCallId"].as_str().unwrap_or_default())
                    && let Some(raw_input) = parse_json_object(input)
                    && let Some(object) = update.as_object_mut()
                {
                    object.insert("rawInput".to_string(), raw_input);
                }
                vec![update]
            }
            ServerEvent::ToolDone {
                id,
                name,
                output,
                error,
            } => vec![json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": id,
                "title": tool_title(&name),
                "kind": tool_kind(&name),
                "status": if error.is_some() { "failed" } else { "completed" },
                "content": [{
                    "type": "content",
                    "content": {
                        "type": "text",
                        "text": output,
                    }
                }],
                "rawOutput": {
                    "output": output,
                    "error": error,
                }
            })],
            ServerEvent::GeneratedImage {
                id,
                path,
                output_format,
                revised_prompt,
                ..
            } => vec![json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": id,
                "status": "completed",
                "content": [{
                    "type": "content",
                    "content": {
                        "type": "text",
                        "text": format!("Generated image: {path} ({output_format}){}", revised_prompt.map(|prompt| format!("\nRevised prompt: {prompt}")).unwrap_or_default()),
                    }
                }]
            })],
            // Shown in every profile: a context summary replacing history is
            // something the user needs to see, manual or automatic.
            ServerEvent::Compaction { trigger, .. } => {
                vec![self.agent_chunk(format!("\n[Jcode compacted context: {trigger}]\n"))]
            }
            ServerEvent::SessionRenamed { display_title, .. } => vec![json!({
                "sessionUpdate": "session_info_update",
                "title": display_title,
            })],
            ServerEvent::McpStatus { servers } if self.profile.is_extended() => {
                vec![self.agent_chunk(format!("\n[Jcode MCP status: {}]\n", servers.join(", ")))]
            }
            _ => {
                let _ = &self.session_id;
                Vec::new()
            }
        }
    }
}

fn parse_json_object(input: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(input).ok()?;
    value.as_object()?;
    Some(value)
}

fn compatibility_option_value(
    params: &Value,
    value_fields: &[&str],
    method: &str,
) -> std::result::Result<String, String> {
    if !params.is_object() {
        return Err(format!("{method} params must be an object"));
    }
    value_fields
        .iter()
        .find_map(|field| {
            params
                .get(*field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            format!(
                "{method} requires a non-empty string {}",
                value_fields.join(" or ")
            )
        })
}

fn initialize_result(params: &Value, profile: AcpProfile) -> Value {
    // We only speak exactly ACP_PROTOCOL_VERSION; the response pins to our
    // version regardless of the `protocolVersion` the client requested.
    let _ = params;
    let protocol_version = ACP_PROTOCOL_VERSION;
    let mut agent_capabilities = json!({
        "loadSession": true,
        "promptCapabilities": {
            "image": true,
            "audio": false,
            "embeddedContext": true,
        },
        "mcpCapabilities": {
            "http": false,
            "sse": false,
        },
        "sessionCapabilities": {
            "close": {},
            "resume": {},
            "list": {},
        }
    });

    if profile.is_extended()
        && let Some(object) = agent_capabilities.as_object_mut()
    {
        object.insert(
            "_meta".to_string(),
            json!({
                "jcode": {
                    "profile": profile.as_str(),
                    "extensions": ["raw_server_event"]
                }
            }),
        );
    }

    json!({
        "protocolVersion": protocol_version,
        "agentCapabilities": agent_capabilities,
        "agentInfo": {
            "name": "jcode",
            "title": "Jcode",
            "version": jcode_build_meta::pkg_version(),
        },
        "authMethods": [],
    })
}

/// `session/list` page size.
const SESSION_LIST_PAGE: usize = 50;

/// Position of the last entry of a returned page, so the next page resumes
/// exactly after it. Encoded as an opaque string for the client.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionListCursor {
    updated_at_ms: Option<i64>,
    session_id: String,
}

impl SessionListCursor {
    fn encode(&self) -> String {
        json!({
            "updatedAt": self.updated_at_ms,
            "sessionId": self.session_id,
        })
        .to_string()
    }

    fn decode(raw: &str) -> std::result::Result<Self, String> {
        let value: Value = serde_json::from_str(raw)
            .map_err(|_| "session/list cursor is not a cursor this server issued".to_string())?;
        let session_id = value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|session_id| !session_id.is_empty())
            .ok_or_else(|| "session/list cursor is missing sessionId".to_string())?;
        Ok(Self {
            updated_at_ms: value.get("updatedAt").and_then(Value::as_i64),
            session_id: session_id.to_string(),
        })
    }
}

fn session_list_cursor(params: &Value) -> std::result::Result<Option<SessionListCursor>, String> {
    match params.get("cursor").and_then(Value::as_str) {
        Some(raw) if !raw.trim().is_empty() => SessionListCursor::decode(raw).map(Some),
        _ => Ok(None),
    }
}

/// `cwd` is optional for `session/list`; when present it must be absolute, like
/// every other ACP path.
fn optional_cwd_from_params(params: &Value) -> std::result::Result<Option<PathBuf>, String> {
    match params.get("cwd").and_then(Value::as_str) {
        Some(cwd) if !cwd.trim().is_empty() => {
            let path = PathBuf::from(cwd);
            if !path.is_absolute() {
                return Err(format!("ACP cwd must be absolute: {}", path.display()));
            }
            Ok(Some(path))
        }
        _ => Ok(None),
    }
}

/// Canonical form of a directory for comparison. Falls back to the given path
/// when it no longer exists, so a deleted working directory still matches its
/// own stored sessions.
fn canonical_dir(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// [`canonical_dir`] memoised for one `session/list` call.
fn canonical_dir_cached(cache: &mut HashMap<PathBuf, PathBuf>, path: &Path) -> PathBuf {
    if let Some(canonical) = cache.get(path) {
        return canonical.clone();
    }
    let canonical = canonical_dir(path);
    cache.insert(path.to_path_buf(), canonical.clone());
    canonical
}

/// Build the `session/list` response: filter by `cwd`, newest first, one page
/// at a time, with an opaque cursor for the rest.
fn session_list_result(
    mut entries: Vec<jcode_harness_api_server::session_catalog::SessionCatalogEntry>,
    cwd: Option<&Path>,
    cursor: Option<&SessionListCursor>,
    live: Option<&[String]>,
) -> Value {
    // ACP requires a string `cwd`, so a stored session with no recorded
    // working directory cannot be listed at all.
    entries.retain(|entry| entry.working_dir.is_some());
    if let Some(cwd) = cwd {
        // Canonicalise at most once per distinct directory: the catalog can
        // hold thousands of sessions, and each `canonicalize` is a syscall.
        let mut canonical: HashMap<PathBuf, PathBuf> = HashMap::new();
        let wanted = canonical_dir_cached(&mut canonical, cwd);
        entries.retain(|entry| {
            let Some(working_dir) = entry.working_dir.as_deref() else {
                return false;
            };
            let working_dir = Path::new(working_dir);
            // An exact match needs no syscall at all.
            working_dir == cwd || canonical_dir_cached(&mut canonical, working_dir) == wanted
        });
    }
    entries.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    if let Some(cursor) = cursor {
        entries.retain(|entry| entry_sorts_after_cursor(entry, cursor));
    }
    let has_more = entries.len() > SESSION_LIST_PAGE;
    entries.truncate(SESSION_LIST_PAGE);
    let sessions: Vec<Value> = entries
        .iter()
        .map(|entry| session_list_entry(entry, live))
        .collect();
    let next_cursor = has_more
        .then(|| {
            entries.last().map(|entry| {
                SessionListCursor {
                    updated_at_ms: entry.updated_at_ms,
                    session_id: entry.session_id.clone(),
                }
                .encode()
            })
        })
        .flatten();
    let mut result = json!({ "sessions": sessions });
    if let Some(next_cursor) = next_cursor
        && let Some(object) = result.as_object_mut()
    {
        object.insert("nextCursor".to_string(), Value::String(next_cursor));
    }
    result
}

/// Whether `entry` sorts strictly after `cursor` in the newest-first listing
/// (descending `updatedAt`, then ascending `sessionId`).
fn entry_sorts_after_cursor(
    entry: &jcode_harness_api_server::session_catalog::SessionCatalogEntry,
    cursor: &SessionListCursor,
) -> bool {
    match entry.updated_at_ms.cmp(&cursor.updated_at_ms) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => entry.session_id > cursor.session_id,
    }
}

fn session_list_entry(
    entry: &jcode_harness_api_server::session_catalog::SessionCatalogEntry,
    live: Option<&[String]>,
) -> Value {
    let mut session = json!({
        "sessionId": entry.session_id,
        // Entries without a working directory are filtered out before this
        // point: ACP requires a string here.
        "cwd": entry.working_dir.as_deref().unwrap_or_default(),
    });
    let Some(object) = session.as_object_mut() else {
        return session;
    };
    if let Some(title) = entry.title.as_deref().filter(|title| !title.is_empty()) {
        object.insert("title".to_string(), Value::String(title.to_string()));
    }
    if let Some(updated_at) = entry
        .updated_at_ms
        .and_then(chrono::DateTime::from_timestamp_millis)
    {
        object.insert(
            "updatedAt".to_string(),
            Value::String(updated_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        );
    }
    if let Some(live) = live {
        object.insert(
            "_meta".to_string(),
            json!({
                "jcode": { "live": live.iter().any(|id| id == &entry.session_id) }
            }),
        );
    }
    session
}

fn cwd_from_params(params: &Value) -> std::result::Result<PathBuf, String> {
    let cwd = match params.get("cwd").and_then(Value::as_str) {
        Some(cwd) if !cwd.trim().is_empty() => PathBuf::from(cwd),
        _ => std::env::current_dir().map_err(|err| err.to_string())?,
    };
    if !cwd.is_absolute() {
        return Err(format!("ACP cwd must be absolute: {}", cwd.display()));
    }
    Ok(cwd)
}

fn required_session_id(params: &Value) -> std::result::Result<String, String> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Missing required sessionId".to_string())
}

fn validate_acp_mcp_servers(params: &Value) -> std::result::Result<(), String> {
    match params.get("mcpServers") {
        None | Some(Value::Null) => Ok(()),
        // Session-scoped MCP is not supported yet, but rejecting this required
        // ACP field prevents hosts with MCP servers from creating a session.
        Some(Value::Array(_)) => Ok(()),
        Some(_) => Err("ACP mcpServers must be an array".to_string()),
    }
}

/// Read an explicit skill selection from ACP prompt metadata. Accepts
/// `_meta.jcode.skill`, `_meta.jcode.activeSkill`, `_meta.skill`, and
/// `_meta.activeSkill`.
fn explicit_skill_from_params(params: &Value) -> Option<String> {
    let meta = params.get("_meta")?;
    [meta.get("jcode"), Some(meta)]
        .into_iter()
        .flatten()
        .flat_map(|object| ["skill", "activeSkill", "active_skill"].map(|key| object.get(key)))
        .flatten()
        .filter_map(Value::as_str)
        .map(|name| name.trim().trim_start_matches(['/', '$']).trim())
        .find(|name| !name.is_empty())
        .map(str::to_string)
}

/// Resolve `/name [prompt]` or `$name [prompt]` to a registered skill. Returns
/// `None` for unknown names and path-like text so it reaches the model as-is.
fn resolve_acp_skill_invocation(
    skills: &crate::skill::SkillRegistry,
    text: &str,
) -> Option<(String, Option<String>)> {
    let trimmed = text.trim_end();
    // Like built-in commands, a leading space escapes skill interpretation.
    let normalized = if let Some(rest) = trimmed.strip_prefix('$') {
        format!("/{rest}")
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        return None;
    };
    let invocation = skills.resolve_invocation(&normalized)?;
    skills.get(invocation.name)?;
    Some((
        invocation.name.to_string(),
        invocation.prompt.map(str::to_string),
    ))
}

/// ACP `availableCommands` entries for installed skills. Skills whose names
/// contain whitespace or shadow built-in commands are not advertised (they can
/// still be invoked by typing the slash command).
fn acp_skill_commands(skills: &crate::skill::SkillRegistry) -> Vec<Value> {
    const BUILTIN: [&str; 5] = ["model", "models", "effort", "compact", "subagent"];
    skills
        .list()
        .into_iter()
        .filter(|skill| {
            !skill.name.is_empty()
                && !skill.name.contains(char::is_whitespace)
                && !BUILTIN.contains(&skill.name.as_str())
        })
        .map(|skill| {
            json!({
                "name": skill.name,
                "description": skill.description,
                "input": { "hint": "request for this skill (optional)" },
            })
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum AcpSlashCommand {
    Model(Option<String>),
    Models,
    Effort(Option<String>),
    Compact,
    Subagent(AcpSubagentSpec),
}

#[derive(Debug, PartialEq, Eq)]
struct AcpSubagentSpec {
    prompt: String,
    subagent_type: String,
    model: Option<String>,
}

/// Parse `/subagent [--model <id>] [--type <kind>] <prompt>`. Flags are only
/// recognized before the prompt, so prompt text may contain `--`.
fn parse_acp_subagent_spec(argument: Option<String>) -> Result<AcpSubagentSpec> {
    let argument = argument.unwrap_or_default();
    let mut rest = argument.trim_start();
    let mut subagent_type = "general".to_string();
    let mut model = None;
    loop {
        let (flag, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let target = match flag {
            "--model" => &mut model,
            "--type" => {
                let (value, after) = split_flag_value(flag, after)?;
                subagent_type = value;
                rest = after;
                continue;
            }
            flag if flag.starts_with("--") => anyhow::bail!("Unknown /subagent flag {flag}"),
            _ => break,
        };
        let (value, after) = split_flag_value(flag, after)?;
        *target = Some(value);
        rest = after;
    }
    let prompt = rest.trim();
    if prompt.is_empty() {
        anyhow::bail!("Missing prompt. Usage: /subagent [--model <id>] [--type <kind>] <prompt>");
    }
    Ok(AcpSubagentSpec {
        prompt: prompt.to_string(),
        subagent_type,
        model,
    })
}

fn split_flag_value<'a>(flag: &str, after: &'a str) -> Result<(String, &'a str)> {
    let after = after.trim_start();
    let (value, rest) = after.split_once(char::is_whitespace).unwrap_or((after, ""));
    if value.is_empty() {
        anyhow::bail!("Missing value for {flag}");
    }
    Ok((value.to_string(), rest.trim_start()))
}

fn parse_acp_slash_command(text: &str) -> Option<Result<AcpSlashCommand>> {
    // A leading space is the ACP client convention for escaping slash command
    // interpretation and sending the text to the model literally.
    let trimmed = text.trim_end();
    let body = trimmed.strip_prefix('/')?;
    let mut parts = body.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let argument = parts
        .next()
        .map(str::trim)
        .filter(|argument| !argument.is_empty())
        .map(str::to_string);
    match name {
        "model" => Some(Ok(AcpSlashCommand::Model(argument))),
        "models" if argument.is_none() => Some(Ok(AcpSlashCommand::Models)),
        "models" => Some(Err(anyhow::anyhow!("/models does not accept an argument"))),
        "effort" => Some(Ok(AcpSlashCommand::Effort(argument))),
        "compact" if argument.is_none() => Some(Ok(AcpSlashCommand::Compact)),
        "compact" => Some(Err(anyhow::anyhow!("/compact does not accept an argument"))),
        "subagent" => Some(parse_acp_subagent_spec(argument).map(AcpSlashCommand::Subagent)),
        _ => None,
    }
}

fn format_model_catalog(current: Option<&str>, models: &[String]) -> String {
    if models.is_empty() {
        return match current {
            Some(current) => format!("Current model: `{current}`. No model catalog was reported."),
            None => "The active provider did not report a model catalog.".to_string(),
        };
    }
    let mut output = String::from("Available models:\n");
    for model in models {
        let selected = if Some(model.as_str()) == current {
            " (current)"
        } else {
            ""
        };
        output.push_str(&format!("- `{model}`{selected}\n"));
    }
    output.pop();
    output
}

fn prompt_from_params(
    params: &Value,
) -> std::result::Result<(String, Vec<(String, String)>), String> {
    let prompt = params
        .get("prompt")
        .and_then(Value::as_array)
        .ok_or_else(|| "Missing required prompt array".to_string())?;
    let mut text_parts = Vec::new();
    let mut images = Vec::new();

    for block in prompt {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text_parts.push(text.to_string());
                }
            }
            Some("image") => {
                let mime_type = block
                    .get("mimeType")
                    .or_else(|| block.get("mime_type"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Image content block missing mimeType".to_string())?;
                let data = block
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Image content block missing data".to_string())?;
                images.push((mime_type.to_string(), data.to_string()));
            }
            Some("resource") => {
                if let Some(resource) = block.get("resource") {
                    text_parts.push(format_resource_block(resource));
                }
            }
            Some("resource_link") => {
                let uri = block.get("uri").and_then(Value::as_str).unwrap_or("");
                let name = block.get("name").and_then(Value::as_str).unwrap_or(uri);
                text_parts.push(format!("[Resource link: {name} <{uri}>]"));
            }
            Some(other) => {
                return Err(format!(
                    "Unsupported ACP prompt content block type: {other}"
                ));
            }
            None => return Err("Prompt content block missing type".to_string()),
        }
    }

    Ok((text_parts.join("\n\n"), images))
}

fn format_resource_block(resource: &Value) -> String {
    let uri = resource
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or("resource");
    if let Some(text) = resource.get("text").and_then(Value::as_str) {
        format!("[Embedded resource: {uri}]\n{text}")
    } else if let Some(blob) = resource.get("blob").and_then(Value::as_str) {
        let mime = resource
            .get("mimeType")
            .or_else(|| resource.get("mime_type"))
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream");
        format!(
            "[Embedded binary resource: {uri} ({mime}, {} base64 bytes)]",
            blob.len()
        )
    } else {
        format!("[Embedded resource: {uri}]")
    }
}

/// `agent_message_chunk` with a stable `messageId`.
///
/// Clients group streamed chunks into one message per id and fall back to
/// positional grouping when the id is missing, which merges consecutive
/// same-role messages (two assistant messages separated only by tool calls, for
/// example). Every streamed message therefore carries one.
fn agent_message_chunk_with_id(text: String, message_id: String) -> Value {
    json!({
        "sessionUpdate": "agent_message_chunk",
        "messageId": message_id,
        "content": {
            "type": "text",
            "text": text,
        }
    })
}

fn tool_title(name: &str) -> String {
    match name {
        "bash" => "Running shell command".to_string(),
        "read" => "Reading file".to_string(),
        "write" => "Writing file".to_string(),
        "edit" | "multiedit" | "patch" | "apply_patch" | "replace" => "Editing files".to_string(),
        "agentgrep" | "grep" | "glob" | "ls" => "Searching workspace".to_string(),
        "webfetch" | "websearch" => "Fetching web content".to_string(),
        other => other.replace('_', " "),
    }
}

pub(crate) fn tool_kind(name: &str) -> &'static str {
    match name {
        "read" => "read",
        "write" | "edit" | "multiedit" | "patch" | "apply_patch" | "replace" => "edit",
        "bash" | "bg" | "selfdev" => "execute",
        "agentgrep" | "grep" | "glob" | "ls" | "session_search" | "conversation_search" => "search",
        "webfetch" | "websearch" | "codesearch" => "fetch",
        _ => "other",
    }
}

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
