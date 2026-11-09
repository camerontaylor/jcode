//! Agent-to-client JSON-RPC correlation. The stdin reader never awaits a reply.
use super::*;
use tokio::sync::oneshot;

#[derive(Default)]
pub(super) struct ClientRequests {
    next: AtomicU64,
    active: std::sync::Mutex<HashMap<(String, String), Arc<AtomicBool>>>,
    pending: std::sync::Mutex<HashMap<String, Pending>>,
}

struct Pending {
    session: String,
    elicitation: String,
    reply: oneshot::Sender<std::result::Result<Value, Value>>,
}

type ClientReply = oneshot::Receiver<std::result::Result<Value, Value>>;

impl ClientRequests {
    pub(super) fn contains(&self, session: &str, elicitation: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&(session.into(), elicitation.into()))
    }
    fn token(&self, session: &str, elicitation: &str) -> Arc<AtomicBool> {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry((session.into(), elicitation.into()))
            .or_insert_with(|| Arc::new(AtomicBool::new(true)))
            .clone()
    }
    pub(super) fn begin(&self, session: &str, elicitation: &str) -> (String, ClientReply) {
        let id = format!("jcode-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                id.clone(),
                Pending {
                    session: session.into(),
                    elicitation: elicitation.into(),
                    reply: tx,
                },
            );
        (id, rx)
    }

    pub(super) fn route(&self, message: JsonRpcMessage) {
        let Some(id) = message.id.as_ref().and_then(Value::as_str) else {
            return;
        };
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
        if let Some(pending) = pending {
            let reply = match message.error {
                Some(error) => Err(error),
                None => Ok(message.result.unwrap_or(Value::Null)),
            };
            if pending.reply.send(reply).is_err() {
                crate::logging::debug("acp: client reply receiver was dropped");
            }
        } else {
            crate::logging::debug(&format!("acp: ignoring unknown client response id {id}"));
        }
    }

    pub(super) fn withdraw(&self, session: &str, elicitation: Option<&str>) -> Vec<String> {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(s, e), token| {
                let remove = s == session && elicitation.is_none_or(|id| id == e);
                if remove {
                    token.store(false, Ordering::SeqCst);
                }
                !remove
            });
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let ids: Vec<_> = pending
            .iter()
            .filter(|(_, p)| p.session == session && elicitation.is_none_or(|e| p.elicitation == e))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            pending.remove(id);
        }
        ids
    }

    pub(super) fn clear(&self) {
        for token in self
            .active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .map(|(_, v)| v)
        {
            token.store(false, Ordering::SeqCst);
        }
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }
}

/// ACP is extensible even though the daemon's internal response is strict.
pub(super) fn parse_form_reply(value: Value) -> crate::protocol::elicitation::ElicitationResponse {
    use crate::protocol::elicitation::ElicitationResponse;
    match value.get("action").and_then(Value::as_str) {
        Some("accept") => {
            let content = match value.get("content") {
                None | Some(Value::Null) => None,
                Some(Value::Object(content)) => Some(content.clone()),
                _ => return ElicitationResponse::Cancel,
            };
            ElicitationResponse::Accept { content }
        }
        Some("decline") => ElicitationResponse::Decline,
        _ => ElicitationResponse::Cancel,
    }
}

impl AcpRuntime {
    pub(super) async fn cancel_client_forms(
        &self,
        session: &str,
        elicitation: Option<&str>,
    ) -> Result<()> {
        for id in self.client_requests.withdraw(session, elicitation) {
            self.write_notification("$/cancel_request", json!({"requestId":id}))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn routes_responses_and_ignores_unknown_and_late_replies() {
        let requests = ClientRequests::default();
        let (id, rx) = requests.begin("s", "e");
        requests.route(
            JsonRpcMessage::parse(
                &json!({"jsonrpc":"2.0","id":id,"result":{"action":"decline"}}).to_string(),
            )
            .unwrap(),
        );
        assert_eq!(rx.await.unwrap().unwrap()["action"], "decline");
        requests.route(
            JsonRpcMessage::parse(&json!({"jsonrpc":"2.0","id":id,"result":{}}).to_string())
                .unwrap(),
        );
        requests.route(
            JsonRpcMessage::parse(r#"{"jsonrpc":"2.0","id":"unknown","result":{}}"#).unwrap(),
        );
        let (id, rx) = requests.begin("s", "e");
        assert_eq!(requests.withdraw("s", Some("e")), vec![id.clone()]);
        assert!(rx.await.is_err());
        requests.route(
            JsonRpcMessage::parse(
                &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601}}).to_string(),
            )
            .unwrap(),
        );
    }
    #[tokio::test]
    async fn in_memory_transport_routes_jsonrpc_errors_without_waiting_inline() {
        let requests = ClientRequests::default();
        let (id, reply) = requests.begin("session", "form");
        let (mut client, agent) = tokio::io::duplex(1024);
        let frame =
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unsupported"}});
        client
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(agent).read_line(&mut line).await.unwrap();
        requests.route(JsonRpcMessage::parse(&line).unwrap());
        assert_eq!(reply.await.unwrap().unwrap_err()["code"], -32601);
    }
    #[test]
    fn form_reply_ignores_metadata_and_future_actions_cancel() {
        use crate::protocol::elicitation::ElicitationResponse;
        assert_eq!(
            parse_form_reply(
                json!({"action":"accept","content":null,"_meta":{"future":true},"extra":7})
            ),
            ElicitationResponse::Accept { content: None }
        );
        assert_eq!(
            parse_form_reply(json!({"action":"future","content":{},"_meta":{}})),
            ElicitationResponse::Cancel
        );
        assert_eq!(
            parse_form_reply(json!({"action":"accept","content":{"x":1},"_meta":{}})),
            ElicitationResponse::Accept {
                content: Some(json!({"x":1}).as_object().unwrap().clone())
            }
        );
    }
}

impl AcpRuntime {
    pub(super) fn form_supported(&self) -> bool {
        self.client_form.load(Ordering::SeqCst)
    }
    pub(super) fn applet_opt_in(&self) -> bool {
        explicit_applet(&crate::config::config().tools, self.form_supported())
    }
    pub(super) fn insert_elicitation_meta(&self, result: &mut Value) {
        if self.form_supported() {
            result["_meta"]["jcode"]["appletElicitation"] = json!(self.applet_opt_in());
        }
    }
    pub(super) async fn form_event(
        &self,
        session: &std::sync::Weak<DaemonSession>,
        event: &ServerEvent,
    ) -> bool {
        use crate::protocol::elicitation::ElicitationResponse;
        match event {
            ServerEvent::ElicitationRequest(request) => {
                if !self.form_supported()
                    || self
                        .client_requests
                        .contains(&request.session_id, &request.elicitation_id)
                {
                    return true;
                }
                let request = request.clone();
                let session = session.clone();
                let acp = self.clone();
                // Register synchronously before spawning, so an immediately following
                // resolution can withdraw the request before its task first runs.
                let token = self
                    .client_requests
                    .token(&request.session_id, &request.elicitation_id);
                let (id, rx) = self
                    .client_requests
                    .begin(&request.session_id, &request.elicitation_id);
                tokio::spawn(async move {
                    let mut pending = Some((id, rx));
                    loop {
                        let Some((id, reply)) = pending.take() else {
                            break;
                        };
                        if !token.load(Ordering::SeqCst) {
                            break;
                        }
                        let mut params = json!({"sessionId":request.session_id,"mode":"form","message":request.message,"requestedSchema":request.requested_schema});
                        if let Some(tool) = &request.tool_call_id {
                            params["toolCallId"] = json!(tool);
                        }
                        if let Err(error)=acp.write_value(json!({"jsonrpc":"2.0","id":id,"method":"elicitation/create","params":params})).await {
                            crate::logging::debug(&format!("acp: form write failed: {error}"));
                            acp.client_requests.withdraw(&request.session_id,Some(&request.elicitation_id));break;
                        }
                        let response = match reply.await {
                            Ok(Ok(value)) => parse_form_reply(value),
                            Ok(Err(_)) => ElicitationResponse::Cancel,
                            Err(_) => break,
                        };
                        if !token.load(Ordering::SeqCst) {
                            break;
                        }
                        if let ElicitationResponse::Accept { content } = &response {
                            let empty = serde_json::Map::new();
                            if request
                                .requested_schema
                                .check_content(content.as_ref().unwrap_or(&empty))
                                .is_err()
                            {
                                pending = Some(
                                    acp.client_requests
                                        .begin(&request.session_id, &request.elicitation_id),
                                );
                                continue;
                            }
                        }
                        let Some(session) = session.upgrade() else {
                            break;
                        };
                        if let Err(error) = session
                            .send(&Request::ElicitationResponse {
                                id: session.next_id(),
                                session_id: request.session_id.clone(),
                                elicitation_id: request.elicitation_id.clone(),
                                response,
                            })
                            .await
                        {
                            crate::logging::debug(&format!("acp: form reply failed: {error}"));
                        }
                        break;
                    }
                    if token.load(Ordering::SeqCst) {
                        acp.client_requests
                            .withdraw(&request.session_id, Some(&request.elicitation_id));
                    }
                });
                true
            }
            ServerEvent::ElicitationResolved {
                session_id,
                elicitation_id,
                ..
            } => {
                if let Err(error) = self
                    .cancel_client_forms(session_id, Some(elicitation_id))
                    .await
                {
                    crate::logging::debug(&format!("acp: form cancellation failed: {error}"));
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    #[test]
    fn form_capability_requires_an_object_mode() {
        for value in [
            Value::Null,
            json!({}),
            json!({"form":null}),
            json!({"form":false}),
        ] {
            assert!(!advertises_form(
                &json!({"clientCapabilities":{"elicitation":value}})
            ));
        }
        assert!(advertises_form(
            &json!({"clientCapabilities":{"elicitation":{"form":{}}}})
        ));
    }
}
pub(super) fn advertises_form(params: &Value) -> bool {
    params
        .pointer("/clientCapabilities/elicitation/form")
        .is_some_and(Value::is_object)
}

fn explicit_applet(tools: &crate::config::ToolConfig, form: bool) -> bool {
    form && tools
        .enabled
        .iter()
        .any(|n| n.trim().eq_ignore_ascii_case("applet"))
        && !tools
            .disabled
            .iter()
            .any(|n| n.trim().eq_ignore_ascii_case("applet"))
}
#[cfg(test)]
mod opt_in_tests {
    use super::*;
    #[test]
    fn applet_requires_explicit_name_and_disabled_wins() {
        let mut tools = crate::config::ToolConfig::default();
        for profile in ["full", "acp", "minimal"] {
            tools.profile = profile.into();
            for enabled in [vec![], vec!["*".into()], vec!["all".into()]] {
                tools.enabled = enabled;
                assert!(!explicit_applet(&tools, true));
            }
        }
        tools.enabled = vec!["applet".into()];
        assert!(explicit_applet(&tools, true));
        assert!(!explicit_applet(&tools, false));
        tools.disabled = vec!["applet".into()];
        assert!(!explicit_applet(&tools, true));
    }
}
