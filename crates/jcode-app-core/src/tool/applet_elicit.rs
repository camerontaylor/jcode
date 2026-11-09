//! Compile native applet controls into the ACP flat form subset.
use super::{Tool, ToolContext, ToolOutput, applet::AppletTool};
use crate::protocol::elicitation::FormSchema;
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use jcode_applet_types::{Action, Document, Node, NodeKind};
use serde_json::{Map, Value, json};

pub struct AppletElicitationTool;
#[async_trait]
impl Tool for AppletElicitationTool {
    fn name(&self) -> &str {
        "applet"
    }
    fn description(&self) -> &str {
        "Ask the user a structured form. Use mount/update with wait=true and bound input, toggle, select or tabs controls plus custom submit buttons. No credentials, HTML, images, or Desktop placement."
    }
    fn parameters_schema(&self) -> Value {
        AppletTool::new().parameters_schema()
    }
    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        if input.get("wait").and_then(Value::as_bool) != Some(true) {
            bail!("Form applets require wait=true; retry with wait=true");
        }
        if !matches!(
            input
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("mount"),
            "mount" | "update"
        ) {
            bail!("Form applets support only mount/update with wait=true");
        }
        if input.get("placement").is_some_and(|v| !v.is_null())
            || input.get("assets").is_some_and(|v| !v.is_null())
        {
            bail!("Form applets do not support Desktop placement or assets");
        }
        AppletTool::new().execute(input, ctx).await
    }
}

pub struct CompiledForm {
    pub message: String,
    pub schema: FormSchema,
    actions: Vec<Action>,
    action_field: Option<String>,
}
impl CompiledForm {
    pub fn answer(&self, content: &mut Map<String, Value>) -> Result<Action> {
        if let Some(field) = &self.action_field {
            let selected = content
                .remove(field)
                .and_then(|v| v.as_str().map(str::to_owned))
                .ok_or_else(|| anyhow!("missing form action"))?;
            return self
                .actions
                .get(
                    selected
                        .strip_prefix("button-")
                        .ok_or_else(|| anyhow!("invalid form action"))?
                        .parse::<usize>()?,
                )
                .cloned()
                .ok_or_else(|| anyhow!("unknown form action"));
        }
        Ok(self.actions.first().cloned().unwrap_or_else(|| Action {
            action: "submit".into(),
            args: Value::Null,
        }))
    }
}

pub fn compile(document: &Document) -> Result<CompiledForm> {
    let mut compiler = Compiler {
        properties: Map::new(),
        required: Vec::new(),
        text: vec![document.title.clone()],
        actions: Vec::new(),
        labels: Vec::new(),
        errors: Vec::new(),
        state: &document.state,
    };
    compiler.visit(&document.view);
    if compiler.text.iter().any(|text| credential_prompt(text)) {
        compiler
            .errors
            .push("form text requests credentials".into());
    }
    let action_field = if compiler.actions.len() > 1 {
        let mut field = "action".to_string();
        while compiler.properties.contains_key(&field) {
            field.insert(0, '_');
        }
        let options: Vec<_> = compiler
            .labels
            .iter()
            .enumerate()
            .map(|(i, label)| json!({"const":format!("button-{i}"),"title":label}))
            .collect();
        compiler.properties.insert(
            field.clone(),
            json!({"type":"string","title":"Action","oneOf":options}),
        );
        compiler.required.push(field.clone());
        Some(field)
    } else {
        None
    };
    if !compiler.errors.is_empty() {
        bail!("Cannot compile applet form: {}", compiler.errors.join("; "));
    }
    let schema = FormSchema(
        json!({"type":"object","title":document.title,"properties":compiler.properties,"required":compiler.required}),
    );
    schema.validate().map_err(|e| anyhow!(e))?;
    Ok(CompiledForm {
        message: compiler.text.join("\n"),
        schema,
        actions: compiler.actions,
        action_field,
    })
}
struct Compiler<'a> {
    properties: Map<String, Value>,
    required: Vec<String>,
    text: Vec<String>,
    actions: Vec<Action>,
    labels: Vec<String>,
    errors: Vec<String>,
    state: &'a Value,
}
fn credential_prompt(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "password",
        "passphrase",
        "secret",
        "api_key",
        "api key",
        "access_token",
        "access token",
        "credential",
        "private key",
        "private_key",
    ]
    .iter()
    .any(|word| text.contains(word))
}
impl Compiler<'_> {
    fn field(&mut self, bind: &str, mut schema: Value) {
        if self.properties.contains_key(bind) {
            self.errors.push(format!("duplicate bind `{bind}`"));
            return;
        }
        if let Some(default) = self.state.get(bind).filter(|v| !v.is_null()) {
            schema["default"] = default.clone();
        }
        self.properties.insert(bind.into(), schema);
        self.required.push(bind.into());
    }
    fn visit(&mut self, node: &Node) {
        use NodeKind::*;
        for action in node.actions() {
            if action.action.starts_with("host.") {
                self.errors.push(format!("host action `{}`", action.action));
            }
        }
        match &node.kind {
            Input {bind,placeholder,..} => {
                let name=format!("{bind} {placeholder}").to_ascii_lowercase();
                if credential_prompt(&name) {
                    self.errors.push(format!("input `{bind}` requests credentials"));
                }
                self.field(bind,json!({"type":"string","title":if placeholder.is_empty(){bind}else{placeholder},"maxLength":8192}));
            }
            Toggle {bind,label,..} => self.field(bind,json!({"type":"boolean","title":label})),
            Select {bind,options,..} => self.field(bind,json!({"type":"string","title":bind,"oneOf":options.iter().map(|o|json!({"const":o.value,"title":o.label})).collect::<Vec<_>>()})),
            Tabs {bind,tabs,..} => self.field(bind,json!({"type":"string","title":bind,"oneOf":tabs.iter().map(|t|json!({"const":t.id,"title":t.label})).collect::<Vec<_>>()})),
            Button {label,on_press,disabled,..} => {
                if !disabled {
                    self.actions.push(on_press.clone());
                    self.labels.push(label.clone());
                    self.text.push(label.clone());
                }
            }
            Text {text,..}|Markdown {text}|Code {text,..} => self.text.push(text.clone()),
            KeyValue {rows} => self.text.extend(rows.iter().map(|r|format!("{}: {}",r.key,r.value))),
            Card {title:Some(title),..} => self.text.push(title.clone()),
            Html {..}|Chip {..}|Image {on_press:Some(_),..}|ListItem {on_press:Some(_),..}|Unknown {..} => {
                let kind = match &node.kind { Html {..}=>"html", Chip {..}=>"chip", Image {..}=>"image", ListItem {..}=>"list_item", Unknown {type_name,..}=>type_name, _=>"unknown" };
                self.errors.push(format!("unsupported node `{kind}`"));
            }
            Image {alt,..}=>self.text.push(alt.clone()),
            _=>{}
        }
        for child in node.children() {
            self.visit(child);
        }
    }
}

/// The tool and Desktop share a winner claim before either mutates applet state.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Source {
    Desktop,
    Acp,
}
type Claims = std::collections::HashMap<(String, String), Option<Source>>;
fn claims() -> &'static std::sync::Mutex<Claims> {
    static CLAIMS: std::sync::OnceLock<std::sync::Mutex<Claims>> = std::sync::OnceLock::new();
    CLAIMS.get_or_init(Default::default)
}
pub(crate) fn claim(session: &str, instance: &str, source: Source) -> bool {
    let mut claims = claims().lock().unwrap_or_else(|p| p.into_inner());
    let Some(winner) = claims.get_mut(&(session.into(), instance.into())) else {
        return true;
    };
    if winner.is_some() {
        return false;
    }
    *winner = Some(source);
    true
}
pub(crate) fn release_claim(session: &str, instance: &str) {
    if let Some(winner) = claims()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_mut(&(session.into(), instance.into()))
    {
        *winner = None;
    }
}
struct FormGuard {
    session: String,
    instance: String,
}
impl Drop for FormGuard {
    fn drop(&mut self) {
        claims()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&(self.session.clone(), self.instance.clone()));
        super::applet::cancel_waiter(&self.session, &self.instance);
    }
}
async fn interrupted(ctx: &ToolContext) {
    match &ctx.graceful_shutdown_signal {
        Some(signal) => {
            if !signal.is_set() {
                signal.notified().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}
pub struct PreparedForm {
    form: CompiledForm,
    _guard: FormGuard,
}
pub fn prepare(ctx: &ToolContext, instance: &str, form: CompiledForm) -> PreparedForm {
    claims()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert((ctx.session_id.clone(), instance.into()), None);
    PreparedForm {
        form,
        _guard: FormGuard {
            session: ctx.session_id.clone(),
            instance: instance.into(),
        },
    }
}
pub async fn wait(
    ctx: &ToolContext,
    instance: &str,
    prepared: PreparedForm,
    mut reply: tokio::sync::oneshot::Receiver<String>,
    timeout: std::time::Duration,
) -> Result<Option<String>> {
    use crate::elicitation::ElicitOutcome;
    let PreparedForm { form, _guard } = prepared;
    let ticket = crate::elicitation::global()
        .start_applet(
            &ctx.session_id,
            Some(ctx.tool_call_id.clone()),
            form.message.clone(),
            form.schema.clone(),
            instance,
        )
        .map_err(|error| anyhow!(error))?;
    let Some(ticket) = ticket else {
        return Ok(None);
    };
    let outcome = tokio::select! {
        biased;
        result=&mut reply=>return match result {Ok(message)=>Ok(Some(message)),Err(error)=>Err(anyhow!(error))},
        outcome=ticket.wait(interrupted(ctx),timeout)=>outcome,
    };
    let (action, state) = match outcome {
        ElicitOutcome::Accepted(mut content) => {
            let action = form.answer(&mut content)?;
            let snapshot = crate::applets::snapshot_for_session(&ctx.session_id)?;
            let mut state = snapshot
                .get(instance)
                .ok_or_else(|| anyhow!("applet disappeared"))?
                .document
                .state
                .clone();
            let object = state
                .as_object_mut()
                .ok_or_else(|| anyhow!("applet state must be an object"))?;
            object.extend(content);
            (action, Some(state))
        }
        ElicitOutcome::Declined => (Action::new("host.declined"), None),
        ElicitOutcome::Cancelled => (Action::new("host.cancelled"), None),
        ElicitOutcome::Expired | ElicitOutcome::Unsupported => return Ok(None),
    };
    let snapshot = crate::applets::snapshot_for_session(&ctx.session_id)?;
    let inst = snapshot
        .get(instance)
        .ok_or_else(|| anyhow!("applet disappeared"))?;
    let state = state.unwrap_or_else(|| inst.document.state.clone());
    let (snapshot, inst) = crate::applets::set_state(&ctx.session_id, instance, state)?;
    super::applet::publish(&ctx.session_id, snapshot);
    let message = super::applet::format_action_message(
        instance,
        &inst.document.title,
        &action,
        &inst.document.state,
        None,
    );
    if !super::applet::deliver_to_waiter(&ctx.session_id, instance, message) {
        return Ok(None);
    }
    match reply.await {
        Ok(message) => Ok(Some(message)),
        Err(error) => Err(anyhow!(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document(children: Value) -> Document {
        Document {
            revision: 1,
            title: "Choose".into(),
            view: serde_json::from_value(json!({"type":"stack","children":children})).unwrap(),
            state: json!({"q":"initial","ok":true}),
            assets: vec![],
        }
    }
    #[test]
    fn compiler_table_preserves_controls_defaults_labels_and_actions() {
        let form = compile(&document(json!([
            {"type":"text","text":"Read this"},
            {"type":"input","bind":"q","placeholder":"Question"},
            {"type":"toggle","bind":"ok","label":"Enabled"},
            {"type":"select","bind":"pick","options":[{"value":"x","label":"Ex"}]},
            {"type":"tabs","bind":"tab","tabs":[{"id":"one","label":"First","children":[]}]},
            {"type":"button","label":"Save","on_press":{"action":"save","args":{"n":1}}},
            {"type":"button","label":"Skip","on_press":{"action":"skip"}}
        ])))
        .unwrap();
        let p = &form.schema.0["properties"];
        assert_eq!(p["q"]["default"], "initial");
        assert_eq!(p["q"]["title"], "Question");
        assert_eq!(p["ok"]["type"], "boolean");
        assert_eq!(p["pick"]["oneOf"][0]["title"], "Ex");
        assert_eq!(p["tab"]["oneOf"][0]["const"], "one");
        assert_eq!(p["action"]["oneOf"][0]["title"], "Save");
        let mut content = json!({"q":"answer","action":"button-0"})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(form.answer(&mut content).unwrap().args, json!({"n":1}));
        assert!(!content.contains_key("action"));
        assert!(form.message.contains("Read this"));
        let single = compile(&document(
            json!([{"type":"button","label":"Go","on_press":{"action":"go"}}]),
        ))
        .unwrap();
        assert_eq!(single.answer(&mut Map::new()).unwrap().action, "go");
        assert!(single.schema.0["properties"].get("action").is_none());
    }
    #[test]
    fn submit_selector_preserves_bound_action_and_duplicate_button_names() {
        let form = compile(&document(json!([
            {"type":"input","bind":"action"},
            {"type":"button","label":"First","on_press":{"action":"save","args":1}},
            {"type":"button","label":"Second","on_press":{"action":"save","args":2}}
        ])))
        .unwrap();
        let mut content = json!({"action":"bound value","_action":"button-1"})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(form.answer(&mut content).unwrap().args, json!(2));
        assert_eq!(content["action"], "bound value");
        assert!(!content.contains_key("_action"));
    }
    #[test]
    fn compiler_rejects_duplicates_credentials_and_unsupported_nodes() {
        for nodes in [
            json!([{"type":"input","bind":"q"},{"type":"input","bind":"q"}]),
            json!([{"type":"input","bind":"password"}]),
            json!([{"type":"button","label":"Run","on_press":{"action":"host.run"}}]),
            json!([{"type":"html","source":"hello","height":100},{"type":"chip","label":"chip"}]),
        ] {
            assert!(compile(&document(nodes)).is_err());
        }
        let error = compile(&document(
            json!([{"type":"html","source":"hello","height":100},{"type":"chip","label":"chip"}]),
        ))
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("html") && error.contains("chip"));
    }
    #[test]
    fn desktop_and_acp_claim_exactly_one_winner() {
        let ctx = ToolContext {
            session_id: "claim-session".into(),
            message_id: "message".into(),
            tool_call_id: "call".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: super::super::ToolExecutionMode::AgentTurn,
        };
        let prepared = prepare(&ctx, "claim-test", compile(&document(json!([]))).unwrap());
        assert!(claim(&ctx.session_id, "claim-test", Source::Desktop));
        assert!(!claim(&ctx.session_id, "claim-test", Source::Acp));
        drop(prepared);
    }
}

impl super::Registry {
    /// Desktop opts in through its inline response renderer capability.
    /// Keep Desktop-only UI tools out of the base map so terminal/headless
    /// registries never advertise or execute them, including through batch
    /// and codemode.
    pub(crate) async fn set_desktop_ui_enabled(&self, enabled: bool) {
        if enabled {
            self.register(
                "user_response".into(),
                std::sync::Arc::new(super::user_response::UserResponseTool),
            )
            .await;
            self.register(
                "applet".into(),
                std::sync::Arc::new(super::applet::AppletTool::new()),
            )
            .await;
        } else {
            self.unregister("user_response").await;
            self.unregister("applet").await;
        }
    }
}
