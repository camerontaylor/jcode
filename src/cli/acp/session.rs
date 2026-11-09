use super::*;

mod reconnect;

pub(super) struct DaemonSession {
    pub(super) writer: Mutex<WriteHalf>,
    pub(super) form_supported: bool,
    pub(super) applet_elicitation: bool,
    pub(super) disconnected: AtomicBool,
    /// Captured once: recovery must never discover or launch a different daemon.
    pub(super) socket_path: Option<PathBuf>,
    /// Shared with the router, which builds steer recovery requests itself.
    pub(super) next_request_id: Arc<AtomicU64>,
    /// Client instance id of this logical attachment: `{process}/{n}`. Socket
    /// replacement retains ownership; separate sessions
    /// in one adapter process must not look like each other's peer, so the
    /// per-process id is not enough.
    pub(super) instance_id: String,
    /// Event routing state, shared with this connection's reader task. The
    /// reader owns clones of the shared state, never the session itself, so
    /// dropping the session really does stop reading the daemon.
    pub(super) router: Arc<Mutex<RouterState>>,
    /// `session/prompt` requests in progress. A steering daemon takes several
    /// at once (each joins the running turn); otherwise at most one.
    pub(super) prompts: AtomicUsize,
    pub(super) prompt_tasks: AtomicUsize,
    pub(super) prompt_tasks_done: tokio::sync::Notify,
    pub(super) catalog_refresh: Mutex<()>,
    pub(super) ui_state: Arc<Mutex<SessionUiState>>,
    /// Workspace root reported by the ACP client, used to compose the
    /// project-local skill overlay for slash-skill resolution.
    pub(super) working_dir: Option<PathBuf>,
    /// Sticky skill selection, resent with every prompt. The daemon treats a
    /// missing `active_skill` as "clear the selection", mirroring the TUI.
    pub(super) active_skill: Mutex<Option<String>>,
    /// Peer output is held here until the attach handshake has replayed
    /// history, so live peer events cannot interleave with the replay.
    pub(super) peer_gate: Arc<Mutex<PeerActionGate>>,
    /// Mapper state for the mirrored peer turn, shared by the reader and the
    /// attach flush so a tool call spanning that boundary keeps its input.
    pub(super) peer_mapper: Arc<Mutex<Option<EventMapper>>>,
    pub(super) reader_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(super) catalog_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// Peer actions buffered while a session attach is replaying history.
#[derive(Default)]
pub(super) struct PeerActionGate {
    pub(super) ready: bool,
    pub(super) buffered: Vec<RouteAction>,
}

/// Upper bound on buffered peer actions. A mid-turn attach cannot replay
/// unbounded output; the newest events are the ones still on screen.
pub(super) const MAX_BUFFERED_PEER_ACTIONS: usize = 512;

/// Drain held peer actions while keeping live dispatch ordered behind them.
///
/// The gate guard is held for the whole flush: a live action that arrives while
/// a held action is being rendered blocks in [`PeerActionGate::hold`] and is
/// therefore rendered after it, never between or ahead of the buffer.
pub(super) async fn flush_peer_actions<'a, F>(
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
pub(super) struct RouteSink {
    pub(super) acp: AcpRuntime,
    pub(super) router: Arc<Mutex<RouterState>>,
    pub(super) ui_state: Arc<Mutex<SessionUiState>>,
    pub(super) gate: Arc<Mutex<PeerActionGate>>,
    pub(super) peer_mapper: Arc<Mutex<Option<EventMapper>>>,
    /// Workspace root the daemon resolves tool paths against, so mirrored tool
    /// calls carry absolute locations and diff paths.
    pub(super) working_dir: Option<PathBuf>,
    pub(super) session: std::sync::Weak<DaemonSession>,
}

impl RouteSink {
    pub(super) fn apply(&self, actions: Vec<RouteAction>) -> futures::future::BoxFuture<'_, ()> {
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
                                self.working_dir.as_deref(),
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
pub(super) struct PromptTicket {
    pub(super) session: Arc<DaemonSession>,
    pub(super) prompt_id: Option<u64>,
    pub(super) released: bool,
}

impl PromptTicket {
    pub(super) fn new(session: Arc<DaemonSession>) -> Self {
        Self {
            session,
            prompt_id: None,
            released: false,
        }
    }

    pub(super) async fn release(&mut self) {
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
    pub(super) fn mark_prompt_replayed(&mut self, turn_id: &str) {
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
    pub(super) fn hold(&mut self, action: RouteAction) -> bool {
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
pub(super) struct SessionUiState {
    pub(super) provider_name: Option<String>,
    pub(super) model: Option<String>,
    pub(super) available_models: Vec<String>,
    pub(super) reasoning_effort: Option<String>,
    pub(super) available_reasoning_efforts: Option<Vec<String>>,
    /// The plan entries this connection last sent. A plan is session state, so
    /// the dedup lives here rather than in a per-turn mapper.
    pub(super) last_plan: Option<Vec<Value>>,
}

#[derive(Debug, Default)]
pub(super) struct TurnUsage {
    pub(super) reported: bool,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) cached_read_tokens: Option<u64>,
    pub(super) cached_write_tokens: Option<u64>,
    pub(super) call_snapshot: (u64, u64, Option<u64>, Option<u64>),
}

impl TurnUsage {
    pub(super) fn begin_call(&mut self) {
        self.call_snapshot = (0, 0, None, None);
    }

    pub(super) fn observe_snapshot(
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
    pub(super) fn add(
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

    pub(super) fn to_acp(&self) -> Option<Value> {
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

pub(super) fn add_optional_tokens(total: &mut Option<u64>, tokens: Option<u64>) {
    if let Some(tokens) = tokens {
        *total = Some(total.unwrap_or(0).saturating_add(tokens));
    }
}

pub(super) fn prompt_response(stop_reason: &str, usage: &TurnUsage) -> Value {
    let mut response =
        json!({ "stopReason": stop_reason, "_meta": {"jcode": {"turnId": null, "role": "none"}} });
    if let Some(usage) = usage.to_acp() {
        response
            .as_object_mut()
            .expect("prompt response is an object")
            .insert("usage".to_string(), usage);
    }
    response
}

/// Missing attribution on streamed work means unknown, not a turnless action.
pub(super) fn streamed_prompt_response(
    stop_reason: &str,
    usage: &TurnUsage,
    correlation: Option<(String, &'static str)>,
    turnless: bool,
) -> Value {
    let mut response = prompt_response(stop_reason, usage);
    if let Some((turn_id, role)) = correlation {
        response["_meta"]["jcode"] = json!({"turnId":turn_id,"role":role});
    } else if !turnless && let Some(object) = response.as_object_mut() {
        object.remove("_meta");
    }
    response
}

impl SessionUiState {
    pub(super) fn from_history_fields(
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
            last_plan: None,
        }
    }

    pub(super) fn context_limit(&self) -> u64 {
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
    pub(super) fn new(
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
            form_supported: false,
            applet_elicitation: false,
            disconnected: AtomicBool::new(false),
            socket_path: None,
            next_request_id,
            instance_id: instance_id.clone(),
            router: Arc::new(Mutex::new(router)),
            prompts: AtomicUsize::new(0),
            prompt_tasks: AtomicUsize::new(0),
            prompt_tasks_done: tokio::sync::Notify::new(),
            catalog_refresh: Mutex::new(()),
            ui_state: Arc::new(Mutex::new(SessionUiState::default())),
            working_dir,
            active_skill: Mutex::new(None),
            peer_gate: Arc::new(Mutex::new(PeerActionGate::default())),
            peer_mapper: Arc::new(Mutex::new(None)),
            reader_task: std::sync::Mutex::new(None),
            catalog_task: std::sync::Mutex::new(None),
        }
    }

    /// Client instance id reported to the daemon for this connection.
    pub(super) fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Effective `sharedControl` support of this attachment, from the daemon
    /// capabilities probed before subscribing.
    pub(super) async fn shared_control_support(&self) -> SharedControlSupport {
        let router = self.router.lock().await;
        SharedControlSupport::from_router(&router)
    }

    /// Effective skills for this session: global skills plus the workspace's
    /// project-local overlay, loaded fresh so newly installed skills resolve.
    pub(super) fn skills(&self) -> crate::skill::SkillRegistry {
        crate::skill::SkillRegistry::load_for_working_dir(self.working_dir.as_deref())
            .unwrap_or_default()
    }

    pub(super) async fn set_ui_state(&self, state: SessionUiState) {
        *self.ui_state.lock().await = state;
    }

    pub(super) fn next_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Session id the daemon assigned. Authoritative only after the first
    /// `History` arrives, which is why the router owns it.
    pub(super) async fn session_id(&self) -> String {
        self.router.lock().await.session_id().to_string()
    }

    pub(super) async fn live_sessions(&self) -> Option<Vec<String>> {
        self.router
            .lock()
            .await
            .live_sessions()
            .map(<[String]>::to_vec)
    }

    pub(super) async fn send(&self, request: &Request) -> Result<()> {
        let mut json = serde_json::to_string(request)?;
        json.push('\n');
        let mut writer = self.writer.lock().await;
        anyhow::ensure!(
            !self.disconnected.load(Ordering::SeqCst),
            "Daemon transport unavailable; request was not retried"
        );
        writer.write_all(json.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }

    pub(super) async fn register(&self, request_id: u64, kind: PendingKind) -> RoutedReceiver {
        self.router.lock().await.register(request_id, kind)
    }

    pub(super) async fn unregister(&self, request_id: u64) {
        self.router.lock().await.unregister(request_id);
    }

    /// Claim a prompt slot. `concurrent` prompts may share the session with
    /// others; any other prompt needs it to itself.
    pub(super) fn try_begin_prompt(&self, concurrent: bool) -> bool {
        if concurrent {
            self.prompts.fetch_add(1, Ordering::SeqCst);
            return true;
        }
        self.prompts
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(super) fn prompt_active(&self) -> bool {
        self.prompts.load(Ordering::SeqCst) > 0
    }

    /// Everything a routing action needs, without keeping the session alive.
    pub(super) fn sink(self: &Arc<Self>, acp: AcpRuntime) -> RouteSink {
        RouteSink {
            acp,
            router: Arc::clone(&self.router),
            ui_state: Arc::clone(&self.ui_state),
            gate: Arc::clone(&self.peer_gate),
            peer_mapper: Arc::clone(&self.peer_mapper),
            working_dir: self.working_dir.clone(),
            session: Arc::downgrade(self),
        }
    }

    /// Read the daemon connection for the lifetime of the session, routing
    /// every event. A single undecodable line never ends the session: a newer
    /// daemon's event that this build does not know is skipped and logged.
    pub(super) fn spawn_reader(self: &Arc<Self>, reader: ReadHalf, acp: AcpRuntime) {
        // Only shared state, never `Arc<DaemonSession>`: a task holding the
        // session would keep it (and its socket) alive forever, because the
        // session's `Drop` is what aborts this task.
        let sink = self.sink(acp.clone());
        let router = Arc::clone(&self.router);
        let ui_state = Arc::clone(&self.ui_state);
        let session = Arc::downgrade(self);
        let reader_session = session.clone();
        let (catalog_tx, mut catalog_rx) = tokio::sync::watch::channel(());
        let catalog_acp = acp.clone();
        let catalog_handle = tokio::spawn(async move {
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
        if let Ok(mut slot) = self.catalog_task.lock() {
            *slot = Some(catalog_handle);
        }
        let handle = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                let bytes = match reader.read_line(&mut line).await {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        crate::logging::debug(&format!("acp: daemon read failed: {err}"));
                        0
                    }
                };
                if bytes == 0 {
                    let Some(session) = reader_session.upgrade() else {
                        break;
                    };
                    session.disconnected.store(true, Ordering::SeqCst);
                    if let Err(error) = acp
                        .cancel_client_forms(&session.session_id().await, None)
                        .await
                    {
                        crate::logging::debug(&format!(
                            "acp: form disconnect cleanup failed: {error}"
                        ));
                    }
                    let actions = router.lock().await.transport_lost();
                    sink.apply(actions).await;
                    let recovery = tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            async {
                                let reconnect = session.reconnect_transport();
                                tokio::pin!(reconnect);
                                loop {
                                    tokio::select! {
                                        result = &mut reconnect => break result,
                                        _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                                            anyhow::ensure!(Arc::strong_count(&session) > 1 && router.lock().await.can_reconnect(), "Attachment released during reconnect");
                                        }
                                    }
                                }
                            },
                        )
                        .await;
                    match recovery {
                        Ok(Ok((replacement, events))) => {
                            reader = replacement;
                            for event in events {
                                if acp.form_event(&reader_session, &event).await {
                                    continue;
                                }
                                let actions = router.lock().await.route(event);
                                sink.apply(actions).await;
                            }
                            session.disconnected.store(false, Ordering::SeqCst);
                            continue;
                        }
                        Ok(Err(err)) => {
                            crate::logging::debug(&format!("acp: reconnect failed: {err:#}"));
                            break;
                        }
                        Err(err) => {
                            crate::logging::debug(&format!(
                                "acp: reconnect deadline expired: {err}"
                            ));
                            break;
                        }
                    }
                }
                let Some(event) = decode_daemon_line(&line) else {
                    continue;
                };
                if acp.form_event(&reader_session, &event).await {
                    continue;
                }
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
        if let Ok(mut slot) = self.catalog_task.lock()
            && let Some(handle) = slot.take()
        {
            handle.abort();
        }
        if let Ok(mut slot) = self.reader_task.lock()
            && let Some(handle) = slot.take()
        {
            handle.abort();
        }
    }
}

impl AcpRuntime {
    pub(super) async fn ensure_daemon(&self) -> Result<()> {
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

    pub(super) async fn connect_daemon(&self) -> Result<(ReadHalf, WriteHalf)> {
        self.ensure_daemon().await?;
        let stream = crate::server::connect_socket(&crate::server::socket_path()).await?;
        Ok(stream.into_split())
    }

    /// Wrap a daemon connection: start its reader, then learn whether the
    /// daemon attributes turns (`turn_origin` in `Pong::capabilities`) before
    /// subscribing, so the subscription can ask for turn identity.
    pub(super) async fn start_daemon_session(
        &self,
        session_id: &str,
        working_dir: Option<PathBuf>,
        reader: ReadHalf,
        writer: WriteHalf,
    ) -> Result<Arc<DaemonSession>> {
        let connection = self.connection_seq.fetch_add(1, Ordering::Relaxed);
        #[cfg(unix)]
        let socket_path = match reader.peer_addr() {
            Ok(addr) => addr.as_pathname().map(Path::to_path_buf),
            Err(err) => {
                crate::logging::debug(&format!("acp: cannot capture reconnect address: {err}"));
                None
            }
        };
        #[cfg(not(unix))]
        let socket_path = None;
        let mut session = DaemonSession::new(
            session_id,
            working_dir,
            writer,
            connection_instance_id(&self.instance_id, connection),
            self.request_id_base,
        );
        session.socket_path = socket_path;
        session.form_supported = self.form_supported();
        session.applet_elicitation = self.applet_opt_in();
        let session = Arc::new(session);
        session.spawn_reader(reader, self.clone());
        let capabilities = probe_capabilities(&session).await?;
        let mut router = session.router.lock().await;
        router.preserve_work = capabilities.preserve_work;
        router.set_turn_origin(capabilities.turn_origin);
        router.set_soft_interrupt_ids(capabilities.soft_interrupt_ids);
        drop(router);
        Ok(session)
    }

    pub(super) async fn create_new_session(&self, cwd: PathBuf) -> Result<Arc<DaemonSession>> {
        let (reader, writer) = self.connect_daemon().await?;
        self.create_new_session_connected(cwd, reader, writer).await
    }

    pub(super) async fn create_new_session_connected(
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
                supports_elicitation_form: session.form_supported,
                applet_elicitation: session.applet_elicitation,
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

    pub(super) async fn attach_existing_session(
        &self,
        target_session_id: String,
        cwd: PathBuf,
        replay_history: bool,
    ) -> Result<Arc<DaemonSession>> {
        let existing = self.sessions.lock().await.get(&target_session_id).cloned();
        if let Some(session) = existing.filter(|session| {
            // A completed reader has already rendered its closing bracket;
            // preserve session/load's ability to reconnect a dead transport.
            session
                .reader_task
                .lock()
                .is_ok_and(|task| task.as_ref().is_some_and(|task| !task.is_finished()))
        }) {
            // Loading an attached view must retain its router and open peer
            // bracket. A second socket would announce the same active turn
            // again before dropping the first attachment.
            if replay_history {
                session.peer_gate.lock().await.ready = false;
                let replay = async {
                    if let ServerEvent::History {
                        session_id,
                        messages,
                        ..
                    } = request_history(&session).await?
                    {
                        self.replay_history(&session_id, messages).await?;
                    }
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                // Release even when requesting/replaying history failed.
                let released = self.release_peer_actions(&session).await;
                replay?;
                released?;
            }
            return Ok(session);
        }
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

    pub(super) async fn attach_existing_session_connected(
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
                supports_elicitation_form: session.form_supported,
                applet_elicitation: session.applet_elicitation,
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
            let event = next_routed(&mut resume_rx).await?;
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

    pub(super) async fn replay_history(
        &self,
        session_id: &str,
        messages: Vec<crate::protocol::HistoryMessage>,
    ) -> Result<()> {
        for update in history_replay_updates(session_id, &messages) {
            self.write_notification("session/update", update).await?;
        }
        Ok(())
    }
}

/// How long to wait for the daemon's `Pong` before treating it as a daemon
/// without turn attribution.
pub(super) const CAPABILITY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The request that sends prompt `prompt_id`, tracked by the router until a
/// turn takes it.
///
/// A steering daemon gets a soft interrupt, like the TUI and SDK clients send:
/// an idle session runs it as a new turn, a busy one injects it into the
/// running turn (ours or a peer's) instead of rejecting it. A soft interrupt
/// cannot carry the sticky skill selection, so such a prompt goes as a
/// `Message` first and steers only if the session turns out to be busy. A
/// legacy daemon always gets a `Message`.
pub(super) fn prompt_request(
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
pub(super) struct DaemonCapabilities {
    pub(super) preserve_work: bool,
    /// Turns are attributed (`turn_origin`).
    pub(super) turn_origin: bool,
    /// Soft interrupts carry client ids (`soft_interrupt_ids`); with
    /// `turn_origin`, prompts are steered.
    pub(super) soft_interrupt_ids: bool,
}

impl DaemonCapabilities {
    pub(super) fn from_pong(capabilities: &[String]) -> Self {
        let has = |name: &str| capabilities.iter().any(|capability| capability == name);
        Self {
            preserve_work: has("detach_preserve_work"),
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
pub(super) async fn probe_capabilities(session: &Arc<DaemonSession>) -> Result<DaemonCapabilities> {
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
pub(super) async fn next_routed(rx: &mut RoutedReceiver) -> Result<ServerEvent> {
    match rx.recv().await {
        Some(RoutedEvent::Event(event)) => Ok(*event),
        Some(RoutedEvent::LocalTurnEnd { .. }) => {
            anyhow::bail!("unexpected local turn end on a helper reply")
        }
        None => anyhow::bail!("Jcode daemon disconnected"),
    }
}

pub(super) async fn wait_for_done(rx: &mut RoutedReceiver, request_id: u64) -> Result<()> {
    loop {
        match next_routed(rx).await? {
            ServerEvent::Ack { .. } => {}
            ServerEvent::Done { id } if id == request_id => return Ok(()),
            ServerEvent::Error { id, message, .. } if id == request_id => anyhow::bail!(message),
            _ => {}
        }
    }
}

pub(super) async fn request_history(session: &Arc<DaemonSession>) -> Result<ServerEvent> {
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

pub(super) async fn request_model_catalog(session: &Arc<DaemonSession>) -> Result<ServerEvent> {
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

pub(super) async fn wait_for_model_changed(
    session: &DaemonSession,
    rx: &mut RoutedReceiver,
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
pub(super) async fn wait_for_compact_result(
    rx: &mut RoutedReceiver,
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

pub(super) async fn wait_for_effort_changed(
    session: &DaemonSession,
    rx: &mut RoutedReceiver,
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
