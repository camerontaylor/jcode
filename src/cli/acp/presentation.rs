use super::*;

/// `session/update` payload for a `usage_update` notification.
pub(super) fn usage_update_payload(
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

/// Wire name of a turn origin kind.
pub(super) fn turn_origin_kind(origin: &TurnOrigin) -> &'static str {
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

/// `session/update` for the prompt that started a mirrored peer turn.
pub(super) fn peer_user_message_chunk(session_id: &str, turn_id: &str, content: &str) -> Value {
    user_message_chunk(session_id, format!("{session_id}:{turn_id}:user"), content)
}

/// `session/update` showing another client's message.
pub(super) fn user_message_chunk(session_id: &str, message_id: String, content: &str) -> Value {
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
pub(super) fn turn_end_state(status: TurnEndStatus) -> &'static str {
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
pub(super) fn history_replay_updates(
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

pub(super) struct EventMapper {
    pub(super) session_id: String,
    pub(super) profile: AcpProfile,
    pub(super) current_tool_id: Option<String>,
    pub(super) tool_inputs: HashMap<String, String>,
    /// Namespace for the `messageId` of streamed assistant messages, e.g.
    /// `{session}:own:{request id}`. Chunks of one message share an id so
    /// clients group them, and the next message in the same turn gets the next
    /// index.
    pub(super) message_scope: String,
    pub(super) message_index: u64,
    /// Index of the reasoning block currently being streamed, in its own id
    /// space so a thought block and an assistant message never share an id.
    pub(super) thought_index: u64,
    /// Workspace root the daemon resolves tool paths against. Tool inputs name
    /// files relative to it, and ACP locations require absolute paths.
    pub(super) working_dir: Option<PathBuf>,
}

impl EventMapper {
    /// A mapper whose streamed messages are scoped to `{session}:stream`.
    #[cfg(test)]
    pub(super) fn new(session_id: String, profile: AcpProfile) -> Self {
        let scope = format!("{session_id}:stream");
        Self::with_scope(session_id, profile, scope)
    }

    pub(super) fn with_scope(
        session_id: String,
        profile: AcpProfile,
        message_scope: String,
    ) -> Self {
        Self {
            session_id,
            profile,
            current_tool_id: None,
            tool_inputs: HashMap::new(),
            message_scope,
            message_index: 0,
            thought_index: 0,
            working_dir: None,
        }
    }

    /// Resolve tool input paths the way the daemon does.
    pub(super) fn with_working_dir(mut self, working_dir: Option<PathBuf>) -> Self {
        self.working_dir = working_dir;
        self
    }

    /// Stable id for the assistant message currently being streamed.
    pub(super) fn message_id(&self) -> String {
        format!("{}:{}", self.message_scope, self.message_index)
    }

    /// Stable id for the reasoning block currently being streamed.
    pub(super) fn thought_message_id(&self) -> String {
        format!("{}:thought:{}", self.message_scope, self.thought_index)
    }

    pub(super) fn agent_chunk(&self, text: String) -> Value {
        agent_message_chunk_with_id(text, self.message_id())
    }

    pub(super) fn thought_chunk(&self, text: String) -> Value {
        json!({
            "sessionUpdate": "agent_thought_chunk",
            "messageId": self.thought_message_id(),
            "content": {
                "type": "text",
                "text": text,
            }
        })
    }

    pub(super) fn map_event(&mut self, event: ServerEvent) -> Vec<Value> {
        // Routing already classified the event by its turn scope; anything
        // still wrapped is unwrapped here only for mapping.
        match super::commands::unwrap_turn_event(event) {
            // One assistant message ended; the next chunk starts a new one.
            ServerEvent::MessageEnd { .. } => {
                self.message_index = self.message_index.saturating_add(1);
                self.thought_index = self.thought_index.saturating_add(1);
                Vec::new()
            }
            // Raw model reasoning. v1 has no non-chunk shape for it, and the
            // daemon's `ReasoningDone` only closes the block, so the block's
            // chunks share one `messageId` and the next block gets the next.
            ServerEvent::ReasoningDelta { text } => vec![self.thought_chunk(text)],
            ServerEvent::ReasoningDone { .. } => {
                self.thought_index = self.thought_index.saturating_add(1);
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
                    "name": name,
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
                {
                    // The streamed input is complete once the call executes,
                    // so this is where the affected files are known. No
                    // location key at all means the tool names no file.
                    let locations = tool_locations(&name, &raw_input, self.working_dir.as_deref());
                    if let Some(object) = update.as_object_mut() {
                        if !locations.is_empty() {
                            object.insert("locations".to_string(), json!(locations));
                        }
                        object.insert("rawInput".to_string(), raw_input);
                    }
                }
                vec![update]
            }
            ServerEvent::ToolDone {
                id,
                name,
                output,
                error,
            } => {
                let mut content = Vec::new();
                // Diffs come from the structured edit input the model sent, and
                // only for a call that actually applied. Reading the file to
                // reconstruct old text would be a guess, so `write` reports no
                // `oldText` at all.
                if error.is_none()
                    && let Some(input) = self.tool_inputs.get(&id)
                    && let Some(raw_input) = parse_json_object(input)
                {
                    content.extend(tool_diffs(&name, &raw_input, self.working_dir.as_deref()));
                }
                content.push(json!({
                    "type": "content",
                    "content": {
                        "type": "text",
                        "text": output.clone(),
                    }
                }));
                vec![json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": id,
                    "title": tool_title(&name),
                    "kind": tool_kind(&name),
                    "status": if error.is_some() { "failed" } else { "completed" },
                    "content": content,
                    "rawOutput": {
                        "output": output,
                        "error": error,
                    }
                })]
            }
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

pub(super) fn parse_json_object(input: &str) -> Option<Value> {
    let value: Value = match serde_json::from_str(input) {
        Ok(value) => value,
        Err(error) => {
            crate::logging::debug(&format!("acp: invalid tool JSON: {error}"));
            return None;
        }
    };
    value.as_object()?;
    Some(value)
}

/// The tools whose writes change the session plan. Aliases match the daemon's
/// own name resolution (`todo_read`/`todo_write`/`todoread`/`todowrite`).
pub(super) fn is_todo_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "todo" | "todo_read" | "todo_write" | "todoread" | "todowrite"
    )
}

/// Structured input fields that name the file a tool call works on, with the
/// field that carries its line when the schema documents one.
///
/// Only tools whose schema declares these fields appear here, and only those
/// fields are read: a tool that names no file has no location, which is
/// legitimate and reported as absence rather than guessed from its arguments
/// or its display title. `apply_patch` is absent on purpose; its input is patch
/// text, and deriving paths from it would mean parsing the patch.
pub(super) fn tool_location_fields(name: &str) -> &'static [(&'static str, Option<&'static str>)] {
    match name {
        "read" => &[("file_path", Some("start_line"))],
        "edit" | "write" => &[("file_path", None)],
        _ => &[],
    }
}

/// The absolute form of a tool input path, resolved exactly as the daemon's
/// `ToolContext::resolve_path` resolves it. ACP locations and diffs require an
/// absolute path, so a path that cannot be made absolute yields nothing.
pub(super) fn absolute_tool_path(path: &str, working_dir: Option<&Path>) -> Option<String> {
    let path = Path::new(path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_dir?.join(path)
    };
    resolved
        .is_absolute()
        .then(|| resolved.to_string_lossy().into_owned())
}

/// ACP `locations` for a tool call, from its structured input.
pub(super) fn tool_locations(name: &str, input: &Value, working_dir: Option<&Path>) -> Vec<Value> {
    let mut locations = Vec::new();
    for (path_field, line_field) in tool_location_fields(name) {
        let Some(path) = input.get(*path_field).and_then(Value::as_str) else {
            continue;
        };
        let Some(path) = absolute_tool_path(path, working_dir) else {
            continue;
        };
        let line = line_field
            .and_then(|field| input.get(field))
            .and_then(Value::as_u64)
            .filter(|line| *line > 0)
            .and_then(|line| match u32::try_from(line) {
                Ok(line) => Some(line),
                Err(error) => {
                    crate::logging::debug(&format!("acp: invalid tool line: {error}"));
                    None
                }
            });
        locations.push(match line {
            Some(line) => json!({ "path": path, "line": line }),
            None => json!({ "path": path }),
        });
    }
    locations
}

/// ACP `diff` content for a tool call, from its structured edit input.
///
/// `edit` reports one diff per replacement the model asked for. `write` reports
/// only `newText`: the previous content is not part of the request, and reading
/// the file to reconstruct it would report a state the daemon never supplied.
pub(super) fn tool_diffs(name: &str, input: &Value, working_dir: Option<&Path>) -> Vec<Value> {
    let Some(path) = input
        .get("file_path")
        .and_then(Value::as_str)
        .and_then(|path| absolute_tool_path(path, working_dir))
    else {
        return Vec::new();
    };
    let diff = |old_text: Option<&str>, new_text: &str| match old_text {
        Some(old_text) => json!({
            "type": "diff",
            "path": path,
            "oldText": old_text,
            "newText": new_text,
        }),
        None => json!({
            "type": "diff",
            "path": path,
            "newText": new_text,
        }),
    };
    match name {
        "edit" => {
            let mut diffs = Vec::new();
            if let Some(edits) = input.get("edits").and_then(Value::as_array) {
                for edit in edits {
                    let (Some(old_text), Some(new_text)) = (
                        edit.get("old_string").and_then(Value::as_str),
                        edit.get("new_string").and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    diffs.push(diff(Some(old_text), new_text));
                }
            }
            // The legacy single-replacement form of the same tool.
            if let (Some(old_text), Some(new_text)) = (
                input.get("old_string").and_then(Value::as_str),
                input.get("new_string").and_then(Value::as_str),
            ) {
                diffs.push(diff(Some(old_text), new_text));
            }
            diffs
        }
        "write" => match input.get("content").and_then(Value::as_str) {
            Some(content) => vec![diff(None, content)],
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The `plan` update for a todo snapshot, or `None` when it repeats the plan
/// this connection already sent.
///
/// A plan is session state, not turn output, so it is rendered whenever the
/// daemon's snapshot arrives — including after the turn that wrote it ended.
/// Each update carries the complete list, as v1 requires; an emptied list is a
/// real replacement and is still sent.
pub(super) fn plan_update(last: &mut Option<Vec<Value>>, payload: &str) -> Option<Value> {
    let entries = plan_entries(payload);
    if last.as_ref() == Some(&entries) {
        return None;
    }
    *last = Some(entries.clone());
    Some(json!({
        "sessionUpdate": "plan",
        "entries": entries,
    }))
}

/// ACP `plan` entries from the daemon's todo snapshot payload.
///
/// The payload is the structured todo card (`todos`, `plan`, `goals`); only the
/// todo list is a plan. ACP requires every entry to carry a `high`/`medium`/
/// `low` priority and a `pending`/`in_progress`/`completed` status, so jcode's
/// free-form priority defaults to `medium` and an unrecognized status to
/// `pending`. Cancelled todos are not plan entries: ACP has no cancelled state
/// and cancelled work is not part of the plan.
pub(super) fn plan_entries(payload: &str) -> Vec<Value> {
    let Ok(snapshot) = serde_json::from_str::<Value>(payload) else {
        return Vec::new();
    };
    snapshot
        .get("todos")
        .and_then(Value::as_array)
        .map(|todos| {
            todos
                .iter()
                .filter_map(|todo| {
                    let content = todo.get("content").and_then(Value::as_str)?;
                    let status = match todo.get("status").and_then(Value::as_str) {
                        Some("in_progress") => "in_progress",
                        Some("completed") => "completed",
                        Some("cancelled") => return None,
                        _ => "pending",
                    };
                    let priority = match todo.get("priority").and_then(Value::as_str) {
                        Some("high") => "high",
                        Some("low") => "low",
                        _ => "medium",
                    };
                    Some(json!({
                        "content": content,
                        "priority": priority,
                        "status": status,
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn jcode_capability_meta(profile: AcpProfile) -> Value {
    let mut extensions = vec![
        "shared_control",
        "peer_turns",
        "turn_correlation",
        "skill_selection",
    ];
    if profile.is_extended() {
        extensions.push("raw_server_event");
    }
    json!({
        "interfaceVersion": 1,
        "profile": profile.as_str(),
        "extensions": extensions,
        "sessionMcpServers": false,
    })
}

/// Effective daemon support behind `sharedControl` on one attachment.
///
/// Derived from the daemon capabilities learned before subscribing, never from
/// this adapter's version. A legacy daemon is read flat, so peer turns are
/// neither observable nor stoppable there; prompts join a running turn only as
/// a client-id-carrying soft interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SharedControlSupport {
    pub(super) detach: bool,
    pub(super) observe: bool,
    pub(super) stop_observed_turn: bool,
    pub(super) implicit_steering: bool,
}

impl SharedControlSupport {
    /// `observe` and `stopObservedTurn` both need `turn_origin`: peer turns are
    /// only visible as attributed turns, and stopping one sends the
    /// turn-scoped `Cancel` that same capability enables. `implicitSteering`
    /// needs `turn_origin` *and* `soft_interrupt_ids`
    /// ([`RouterState::steering`]), because a prompt only joins a running turn
    /// as a soft interrupt carrying this connection's client id.
    pub(super) fn from_router(router: &RouterState) -> Self {
        let turn_origin = router.supports_turn_origin();
        Self {
            detach: router.preserve_work && router.steering(),
            observe: turn_origin,
            stop_observed_turn: turn_origin,
            implicit_steering: router.steering(),
        }
    }
}

/// `_meta.jcode` of a successful new/load/resume result.
///
/// Detach requires negotiated daemon work preservation and input withdrawal.
pub(super) fn jcode_session_meta(instance_id: &str, support: SharedControlSupport) -> Value {
    json!({
        "clientInstanceId": instance_id,
        "sharedControl": {
            "observe": support.observe,
            "stopObservedTurn": support.stop_observed_turn,
            "detach": support.detach,
            "implicitSteering": support.implicit_steering,
        },
    })
}

/// Merge `_meta.jcode` into a result, keeping every other `_meta` key.
pub(super) fn insert_jcode_session_meta(
    result: &mut Value,
    instance_id: &str,
    support: SharedControlSupport,
) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    let meta = object.entry("_meta").or_insert_with(|| json!({}));
    if !meta.is_object() {
        *meta = json!({});
    }
    if let Some(meta) = meta.as_object_mut() {
        meta.insert(
            "jcode".to_string(),
            jcode_session_meta(instance_id, support),
        );
    }
}

pub(super) fn initialize_result(_params: &Value, profile: AcpProfile) -> Value {
    // We only speak exactly ACP_PROTOCOL_VERSION; the response pins to our
    // version regardless of the `protocolVersion` the client requested.
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

    // Every profile advertises the jcode capability metadata: a consumer gates
    // every jcode behavior on it, so an older or narrower profile that omitted
    // it would be indistinguishable from a build that predates it.
    if let Some(object) = agent_capabilities.as_object_mut() {
        object.insert(
            "_meta".to_string(),
            json!({ "jcode": jcode_capability_meta(profile) }),
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

/// `agent_message_chunk` with a stable `messageId`.
///
/// Clients group streamed chunks into one message per id and fall back to
/// positional grouping when the id is missing, which merges consecutive
/// same-role messages (two assistant messages separated only by tool calls, for
/// example). Every streamed message therefore carries one.
pub(super) fn agent_message_chunk_with_id(text: String, message_id: String) -> Value {
    json!({
        "sessionUpdate": "agent_message_chunk",
        "messageId": message_id,
        "content": {
            "type": "text",
            "text": text,
        }
    })
}

pub(super) fn tool_title(name: &str) -> String {
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
        // Planning, not file or command work.
        "todo" => "think",
        "webfetch" | "websearch" | "codesearch" => "fetch",
        // No jcode tool is named for deleting or moving: `apply_patch` does
        // both and reports `edit`. Unknown names stay `other` rather than being
        // inferred from their spelling.
        _ => "other",
    }
}
