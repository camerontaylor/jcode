use super::*;

pub(super) const JSONRPC_PARSE_ERROR: i64 = -32700;
pub(super) const JSONRPC_INVALID_REQUEST: i64 = -32600;
pub(super) const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
pub(super) const JSONRPC_INVALID_PARAMS: i64 = -32602;
pub(super) const JSONRPC_INTERNAL_ERROR: i64 = -32603;
pub(super) const JSONRPC_SERVER_ERROR: i64 = -32000;

#[derive(Debug)]
pub(super) struct JsonRpcMessage {
    pub(super) id: Option<Value>,
    pub(super) method: Option<String>,
    pub(super) params: Value,
}

impl JsonRpcMessage {
    pub(super) fn parse(line: &str) -> std::result::Result<Self, (i64, String)> {
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

impl AcpRuntime {
    pub(super) async fn write_result(&self, id: Value, result: Value) -> Result<()> {
        self.write_value(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .await
    }

    pub(super) async fn write_error_value(
        &self,
        id: Value,
        code: i64,
        message: String,
    ) -> Result<()> {
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

    pub(super) async fn write_notification(&self, method: &str, params: Value) -> Result<()> {
        self.write_value(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }
}

impl AcpRuntime {
    pub(super) async fn write_value(&self, value: Value) -> Result<()> {
        let mut stdout = self.stdout.lock().await;
        let mut line = serde_json::to_string(&value)?;
        line.push('\n');
        stdout.write_all(line.as_bytes()).await?;
        stdout.flush().await?;
        Ok(())
    }
}
