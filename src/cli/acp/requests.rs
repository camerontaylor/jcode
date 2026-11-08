use super::*;

pub(super) fn compatibility_option_value(
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

/// `session/list` page size.
pub(super) const SESSION_LIST_PAGE: usize = 50;

/// Position of the last entry of a returned page, so the next page resumes
/// exactly after it. Encoded as an opaque string for the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionListCursor {
    pub(super) updated_at_ms: Option<i64>,
    pub(super) session_id: String,
}

impl SessionListCursor {
    pub(super) fn encode(&self) -> String {
        json!({
            "updatedAt": self.updated_at_ms,
            "sessionId": self.session_id,
        })
        .to_string()
    }

    pub(super) fn decode(raw: &str) -> std::result::Result<Self, String> {
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

pub(super) fn session_list_cursor(
    params: &Value,
) -> std::result::Result<Option<SessionListCursor>, String> {
    match params.get("cursor").and_then(Value::as_str) {
        Some(raw) if !raw.trim().is_empty() => SessionListCursor::decode(raw).map(Some),
        _ => Ok(None),
    }
}

/// `cwd` is optional for `session/list`; when present it must be absolute, like
/// every other ACP path.
pub(super) fn optional_cwd_from_params(
    params: &Value,
) -> std::result::Result<Option<PathBuf>, String> {
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
pub(super) fn canonical_dir(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// [`canonical_dir`] memoised for one `session/list` call.
pub(super) fn canonical_dir_cached(cache: &mut HashMap<PathBuf, PathBuf>, path: &Path) -> PathBuf {
    if let Some(canonical) = cache.get(path) {
        return canonical.clone();
    }
    let canonical = canonical_dir(path);
    cache.insert(path.to_path_buf(), canonical.clone());
    canonical
}

/// Build the `session/list` response: filter by `cwd`, newest first, one page
/// at a time, with an opaque cursor for the rest.
pub(super) fn session_list_result(
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
pub(super) fn entry_sorts_after_cursor(
    entry: &jcode_harness_api_server::session_catalog::SessionCatalogEntry,
    cursor: &SessionListCursor,
) -> bool {
    match entry.updated_at_ms.cmp(&cursor.updated_at_ms) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => entry.session_id > cursor.session_id,
    }
}

pub(super) fn session_list_entry(
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

pub(super) fn cwd_from_params(params: &Value) -> std::result::Result<PathBuf, String> {
    let cwd = match params.get("cwd").and_then(Value::as_str) {
        Some(cwd) if !cwd.trim().is_empty() => PathBuf::from(cwd),
        _ => std::env::current_dir().map_err(|err| err.to_string())?,
    };
    if !cwd.is_absolute() {
        return Err(format!("ACP cwd must be absolute: {}", cwd.display()));
    }
    Ok(cwd)
}

pub(super) fn required_session_id(params: &Value) -> std::result::Result<String, String> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Missing required sessionId".to_string())
}

/// Whether the client advertised the ACP v1 `session.notices` capability, which
/// is the only case in which an agent may send a `notice` session update. It is
/// an object marker under `clientCapabilities.session`; `null` and a missing key
/// both mean unsupported.
pub(super) fn client_advertises_session_notices(params: &Value) -> bool {
    params
        .get("clientCapabilities")
        .and_then(|capabilities| capabilities.get("session"))
        .and_then(|session| session.get("notices"))
        .is_some_and(Value::is_object)
}

/// `session/update` notice for client-supplied `mcpServers` this adapter accepts
/// but does not connect, or `None` when there is nothing to report (missing,
/// `null` or empty array) or the client did not advertise `session.notices`.
///
/// ACP v1 `notice` is the informational update for the user that is not part of
/// session history, and it may only be sent to a client that advertised it. When
/// it is not advertised nothing is sent: `agent_message_chunk` would invent an
/// assistant turn, and `sessionMcpServers: false` in the capability metadata
/// already tells a jcode-aware client that session servers are not in effect.
pub(super) fn ignored_mcp_servers_notice(
    session_id: &str,
    params: &Value,
    session_notices: bool,
) -> Option<Value> {
    if !session_notices {
        return None;
    }
    let servers = params.get("mcpServers")?.as_array()?;
    if servers.is_empty() {
        return None;
    }
    let names: Vec<&str> = servers
        .iter()
        .filter_map(|server| server.get("name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    let ignored = if names.is_empty() {
        format!("{} client MCP server(s)", servers.len())
    } else {
        names.join(", ")
    };
    Some(json!({
        "sessionId": session_id,
        "update": {
            "sessionUpdate": "notice",
            "severity": "info",
            "title": "Client MCP servers not connected",
            "description": format!(
                "This session ignores the ACP mcpServers it was given ({ignored}): jcode runs the MCP servers configured daemon-side, not per session."
            ),
        }
    }))
}

pub(super) fn validate_acp_mcp_servers(params: &Value) -> std::result::Result<(), String> {
    match params.get("mcpServers") {
        None | Some(Value::Null) => Ok(()),
        // Session-scoped MCP is not supported yet, but rejecting this required
        // ACP field prevents hosts with MCP servers from creating a session.
        Some(Value::Array(_)) => Ok(()),
        Some(_) => Err("ACP mcpServers must be an array".to_string()),
    }
}

/// How an ACP prompt asks the adapter to change this attachment's sticky skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SkillSelection {
    /// No `skill` key was sent (or an empty one): keep the attached skill.
    Unchanged,
    /// `"skill": "name"`: run this prompt with `name` and keep it attached.
    Select(String),
    /// `"skill": null`: clear the attached skill before the text is read.
    Clear,
}

impl SkillSelection {
    pub(super) fn is_explicit(&self) -> bool {
        !matches!(self, SkillSelection::Unchanged)
    }
}

/// Read the prompt's skill selection. The canonical field is
/// `_meta.jcode.skill`, where a string selects and an explicit `null` clears;
/// a missing key leaves the selection alone. The v1 compatibility aliases
/// (`_meta.jcode.activeSkill`, `_meta.skill`, `_meta.activeSkill` and their
/// `active_skill` spellings) are read after it, so the canonical field wins.
///
/// A canonical value that is neither a string nor `null` is a malformed
/// extension parameter rather than a silently ignored field; the aliases stay
/// lenient because they exist only for hosts written against older builds.
pub(super) fn skill_selection_from_params(
    params: &Value,
) -> std::result::Result<SkillSelection, String> {
    let Some(meta) = params.get("_meta") else {
        return Ok(SkillSelection::Unchanged);
    };
    let jcode = meta.get("jcode");
    if let Some(canonical) = jcode.and_then(|jcode| jcode.get("skill")) {
        return match canonical {
            Value::Null => Ok(SkillSelection::Clear),
            Value::String(name) => Ok(named_skill(name)),
            other => Err(format!(
                "ACP skill selection must be a skill name or null, got {other}"
            )),
        };
    }
    for object in [jcode, Some(meta)].into_iter().flatten() {
        for key in ["skill", "activeSkill", "active_skill"] {
            match object.get(key) {
                Some(Value::Null) => return Ok(SkillSelection::Clear),
                Some(Value::String(name)) => {
                    let selection = named_skill(name);
                    if selection.is_explicit() {
                        return Ok(selection);
                    }
                }
                Some(_) | None => {}
            }
        }
    }
    Ok(SkillSelection::Unchanged)
}

/// A selection written as a name. Hosts sometimes serialize the textual form,
/// so a leading `/` or `$` is accepted; blank means no selection.
pub(super) fn named_skill(name: &str) -> SkillSelection {
    let name = name.trim().trim_start_matches(['/', '$']).trim();
    if name.is_empty() {
        SkillSelection::Unchanged
    } else {
        SkillSelection::Select(name.to_string())
    }
}

/// What one ACP prompt does with the attachment's sticky skill, decided before
/// anything is sent to the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PromptSkillPlan {
    /// Skill sent with the prompt, and kept on the attachment when `attach`.
    pub(super) skill: Option<String>,
    /// Whether this prompt rewrites the attachment's sticky selection.
    pub(super) attach: bool,
    /// Text sent to the model: a textual `/skill` prefix is consumed, an
    /// explicit selection leaves the text exactly as written.
    pub(super) text: String,
    /// Selection-only action: answer `end_turn` without starting a model turn.
    pub(super) selection_only: bool,
}

/// Decide the prompt's skill, keeping the contract's precedence: an explicit
/// metadata selection wins over textual `/skill` or `$skill` recognition, a
/// clear submits the text unskilled, and a bare selection starts no turn.
pub(super) fn plan_prompt_skill(
    selection: &SkillSelection,
    sticky: Option<String>,
    skills: &crate::skill::SkillRegistry,
    text: String,
    has_images: bool,
) -> std::result::Result<PromptSkillPlan, String> {
    let selection_only = text.trim().is_empty() && !has_images;
    match selection {
        // The text of a clearing prompt is never interpreted as a skill
        // invocation: it is submitted unskilled as written.
        SkillSelection::Clear => Ok(PromptSkillPlan {
            skill: None,
            attach: true,
            text,
            selection_only,
        }),
        // The canonical selection is the prompt's skill, so the text stays a
        // prompt even when it starts with a slash.
        SkillSelection::Select(name) => {
            if skills.get(name).is_none() {
                return Err(format!("Unknown skill: {name}"));
            }
            Ok(PromptSkillPlan {
                skill: Some(name.clone()),
                attach: true,
                text,
                selection_only,
            })
        }
        SkillSelection::Unchanged => match resolve_acp_skill_invocation(skills, &text) {
            Some((name, prompt)) => {
                let text = prompt.unwrap_or_default();
                Ok(PromptSkillPlan {
                    skill: Some(name),
                    attach: true,
                    selection_only: text.trim().is_empty() && !has_images,
                    text,
                })
            }
            None => Ok(PromptSkillPlan {
                skill: sticky,
                attach: false,
                text,
                selection_only,
            }),
        },
    }
}

/// The error for an explicit skill selection that arrives while a turn or a
/// prompt owns the session, or `None` when the selection can be honored.
///
/// v1 cannot carry a sticky-skill change on a steer: a soft interrupt has no
/// skill field, and a prompt the daemon rejects as busy is resent as a steer.
/// Rejecting here, before anything is inserted, keeps the client from believing
/// a selection that would have been silently dropped.
pub(super) fn busy_skill_selection_error(
    session_id: &str,
    selection: &SkillSelection,
    busy: bool,
) -> Option<String> {
    (selection.is_explicit() && busy).then(|| {
        format!(
            "Session {session_id} is already processing a prompt; a skill selection needs an idle session"
        )
    })
}

/// Resolve `/name [prompt]` or `$name [prompt]` to a registered skill. Returns
/// `None` for unknown names and path-like text so it reaches the model as-is.
pub(super) fn resolve_acp_skill_invocation(
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
pub(super) fn acp_skill_commands(skills: &crate::skill::SkillRegistry) -> Vec<Value> {
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
pub(super) enum AcpSlashCommand {
    Model(Option<String>),
    Models,
    Effort(Option<String>),
    Compact,
    Subagent(AcpSubagentSpec),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AcpSubagentSpec {
    pub(super) prompt: String,
    pub(super) subagent_type: String,
    pub(super) model: Option<String>,
}

/// Parse `/subagent [--model <id>] [--type <kind>] <prompt>`. Flags are only
/// recognized before the prompt, so prompt text may contain `--`.
pub(super) fn parse_acp_subagent_spec(argument: Option<String>) -> Result<AcpSubagentSpec> {
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

pub(super) fn split_flag_value<'a>(flag: &str, after: &'a str) -> Result<(String, &'a str)> {
    let after = after.trim_start();
    let (value, rest) = after.split_once(char::is_whitespace).unwrap_or((after, ""));
    if value.is_empty() {
        anyhow::bail!("Missing value for {flag}");
    }
    Ok((value.to_string(), rest.trim_start()))
}

pub(super) fn selected_slash_command(
    text: &str,
    selection: &SkillSelection,
) -> Option<Result<AcpSlashCommand>> {
    if matches!(selection, SkillSelection::Select(_)) {
        None
    } else {
        parse_acp_slash_command(text)
    }
}

pub(super) fn parse_acp_slash_command(text: &str) -> Option<Result<AcpSlashCommand>> {
    // A leading space is the ACP client convention for escaping slash command
    // interpretation and sending the text to the model literally.
    let trimmed = text.trim_end();
    let body = trimmed.strip_prefix('/')?;
    let mut parts = body.splitn(2, char::is_whitespace);
    let name = parts.next()?;
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

pub(super) fn format_model_catalog(current: Option<&str>, models: &[String]) -> String {
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

pub(super) fn prompt_from_params(
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

pub(super) fn format_resource_block(resource: &Value) -> String {
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
