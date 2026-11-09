//! Agent-to-client JSON-RPC correlation. The stdin reader never awaits a reply.
#![expect(
    dead_code,
    reason = "Request issuance and withdrawal are wired in the following adapter commit"
)]
use super::*;
use tokio::sync::oneshot;

#[derive(Default)]
pub(super) struct ClientRequests {
    next: AtomicU64,
    pending: std::sync::Mutex<HashMap<String, Pending>>,
}

struct Pending {
    session: String,
    elicitation: String,
    reply: oneshot::Sender<std::result::Result<Value, Value>>,
}

type ClientReply = oneshot::Receiver<std::result::Result<Value, Value>>;

impl ClientRequests {
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
