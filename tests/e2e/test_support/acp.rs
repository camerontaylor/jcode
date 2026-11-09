use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
/// Minimal ACP client over the real binary's stdin/stdout.
pub(crate) struct AcpClient {
    child: Child,
    pub(crate) stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    pub(crate) tape: Vec<serde_json::Value>,
    /// Notifications received while waiting for a response.
    pub(crate) pending: Vec<serde_json::Value>,
}

impl AcpClient {
    pub(crate) fn spawn() -> Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_jcode"))
            .arg("acp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawn jcode acp")?;
        let stdin = child.stdin.take().context("acp stdin")?;
        let stdout = child.stdout.take().context("acp stdout")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
            tape: Vec::new(),
            pending: Vec::new(),
        })
    }

    pub(crate) async fn send(&mut self, value: serde_json::Value) -> Result<()> {
        self.tape
            .push(serde_json::json!({"seq":self.tape.len()+1,"dir":"c2a","msg":value}));
        let mut line = serde_json::to_string(&value)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    pub(crate) async fn read_message(&mut self) -> Result<serde_json::Value> {
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(30), self.stdout.read_line(&mut line))
            .await
            .context("timed out waiting for an ACP message")??;
        if read == 0 {
            anyhow::bail!("jcode acp exited before answering");
        }
        let value: serde_json::Value = serde_json::from_str(line.trim())?;
        assert_eq!(value["jsonrpc"], "2.0");
        if value.get("method").is_some() {
            if value.get("id").is_some() {
                assert_eq!(value["method"], "elicitation/create");
                assert!(
                    value["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("jcode-"))
                );
            }
        } else {
            assert!(value.get("id").is_some());
            assert_ne!(value.get("result").is_some(), value.get("error").is_some());
        }
        self.tape
            .push(serde_json::json!({"seq":self.tape.len()+1,"dir":"a2c","msg":value}));
        Ok(value)
    }

    /// Send a request and return its result, buffering notifications that
    /// arrive while waiting.
    pub(crate) async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json_request(id, method, params)).await?;
        loop {
            let message = self
                .read_message()
                .await
                .with_context(|| format!("waiting for the ACP {method} response"))?;
            if message.get("method").is_none()
                && message.get("id").and_then(|id| id.as_u64()) == Some(id)
            {
                if let Some(error) = message.get("error") {
                    anyhow::bail!("ACP {method} failed: {error}");
                }
                return Ok(message.get("result").cloned().unwrap_or_default());
            }
            self.pending.push(message);
        }
    }

    /// Send a request without waiting for its response, so the test can act
    /// while it is in flight.
    pub(crate) async fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json_request(id, method, params)).await?;
        Ok(id)
    }

    /// Wait for the response to a request sent with [`Self::send_request`],
    /// buffering notifications that arrive first.
    pub(crate) async fn wait_for_response(&mut self, id: u64) -> Result<serde_json::Value> {
        let is_response = |message: &serde_json::Value| {
            message.get("method").is_none()
                && message.get("id").and_then(|id| id.as_u64()) == Some(id)
        };
        if let Some(index) = self.pending.iter().position(is_response) {
            return Ok(self.pending.remove(index));
        }
        loop {
            let message = self.read_message().await?;
            if message.get("method").is_none()
                && message.get("id").and_then(|id| id.as_u64()) == Some(id)
            {
                return Ok(message);
            }
            self.pending.push(message);
        }
    }

    /// Wait for a notification matching `predicate`, reading more messages as
    /// needed. Non-matching notifications are buffered for later checks.
    pub(crate) async fn wait_for_notification(
        &mut self,
        predicate: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<serde_json::Value> {
        if let Some(index) = self.pending.iter().position(&predicate) {
            return Ok(self.pending.remove(index));
        }
        for _ in 0..200 {
            let message = self.read_message().await?;
            if message.get("method").is_some() && predicate(&message) {
                return Ok(message);
            }
            self.pending.push(message);
        }
        anyhow::bail!("no matching ACP notification arrived")
    }

    /// Assert nothing arrives from the adapter for `window`.
    pub(crate) async fn expect_silence(&mut self, window: Duration) -> Result<()> {
        match tokio::time::timeout(window, self.read_message()).await {
            Err(_) => Ok(()),
            Ok(Ok(message)) => anyhow::bail!("adapter kept writing after session/close: {message}"),
            Ok(Err(err)) => Err(err),
        }
    }

    pub(crate) fn take_notifications(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.pending)
    }

    /// Read notifications in arrival order until `predicate` matches, returning
    /// everything read, so wire order can be asserted.
    pub(crate) async fn read_notifications_until(
        &mut self,
        predicate: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<Vec<serde_json::Value>> {
        let mut read = Vec::new();
        for _ in 0..200 {
            let message = self.read_message().await?;
            if message.get("method").is_none() {
                anyhow::bail!("expected a notification, got {message}");
            }
            let done = predicate(&message);
            read.push(message);
            if done {
                return Ok(read);
            }
        }
        anyhow::bail!("no matching ACP notification arrived")
    }

    pub(crate) async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
    }
}

fn json_request(id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}
