use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::time::timeout;

use super::jsonrpc::{
    JsonRpcErrorObject, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
};

#[derive(Debug, Clone)]
pub struct StdioTransportOptions {
    pub binary_path: String,
    pub current_dir: Option<PathBuf>,
    pub extra_env: Vec<(String, String)>,
}

impl Default for StdioTransportOptions {
    fn default() -> Self {
        Self {
            binary_path: std::env::var("CLAUDE_CODEX_PROXY_CODEX_BIN")
                .unwrap_or_else(|_| "codex".to_string()),
            current_dir: None,
            extra_env: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StdioTransport {
    inner: Arc<StdioTransportInner>,
}

#[derive(Debug)]
struct StdioTransportInner {
    stdin: Mutex<ChildStdin>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value, JsonRpcErrorObject>>>>,
    notifications: broadcast::Sender<JsonRpcNotification>,
    server_requests: broadcast::Sender<JsonRpcRequest>,
    next_id: AtomicU64,
    child: Mutex<Child>,
}

impl StdioTransport {
    pub async fn spawn(options: StdioTransportOptions) -> Result<Self> {
        let mut command = Command::new(&options.binary_path);
        command.arg("app-server");
        if let Some(current_dir) = &options.current_dir {
            command.current_dir(current_dir);
        }
        for (key, value) in &options.extra_env {
            command.env(key, value);
        }
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .context("failed to spawn codex app-server")?;
        let stdin = child
            .stdin
            .take()
            .context("codex app-server stdin missing")?;
        let stdout = child
            .stdout
            .take()
            .context("codex app-server stdout missing")?;
        let stderr = child
            .stderr
            .take()
            .context("codex app-server stderr missing")?;

        let (notifications_tx, _) = broadcast::channel(512);
        let (server_requests_tx, _) = broadcast::channel(128);

        let transport = Self {
            inner: Arc::new(StdioTransportInner {
                stdin: Mutex::new(stdin),
                pending: Mutex::new(HashMap::new()),
                notifications: notifications_tx,
                server_requests: server_requests_tx,
                next_id: AtomicU64::new(1),
                child: Mutex::new(child),
            }),
        };

        transport.spawn_stdout_task(stdout);
        transport.spawn_stderr_task(stderr);

        Ok(transport)
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<JsonRpcNotification> {
        self.inner.notifications.subscribe()
    }

    pub fn subscribe_server_requests(&self) -> broadcast::Receiver<JsonRpcRequest> {
        self.inner.server_requests.subscribe()
    }

    pub async fn request<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let request_id = RequestId::from(self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        let key = request_id.key();
        let request = JsonRpcRequest::new(request_id, method.to_string(), params);
        let payload =
            serde_json::to_vec(&request).context("failed to serialize JSON-RPC request")?;

        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(key.clone(), tx);

        {
            let mut stdin = self.inner.stdin.lock().await;
            stdin
                .write_all(&payload)
                .await
                .context("failed to write JSON-RPC request")?;
            stdin
                .write_all(b"\n")
                .await
                .context("failed to write JSON-RPC newline")?;
            stdin
                .flush()
                .await
                .context("failed to flush JSON-RPC request")?;
        }

        let response = match timeout(request_timeout(), rx).await {
            Ok(response) => response.context("request channel closed before response")?,
            Err(_) => {
                self.inner.pending.lock().await.remove(&key);
                anyhow::bail!("timed out waiting for JSON-RPC response to {method}");
            }
        };
        let response = response.map_err(anyhow::Error::new)?;
        serde_json::from_value(response).context("failed to deserialize JSON-RPC response")
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let notification = JsonRpcNotification::new(method.to_string(), params);
        let payload = serde_json::to_vec(&notification)
            .context("failed to serialize JSON-RPC notification")?;
        let mut stdin = self.inner.stdin.lock().await;
        stdin
            .write_all(&payload)
            .await
            .context("failed to write JSON-RPC notification")?;
        stdin
            .write_all(b"\n")
            .await
            .context("failed to write JSON-RPC newline")?;
        stdin
            .flush()
            .await
            .context("failed to flush JSON-RPC notification")?;
        Ok(())
    }

    pub async fn respond_success(&self, id: RequestId, result: Value) -> Result<()> {
        let payload = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .context("failed to serialize JSON-RPC success response")?;
        let mut stdin = self.inner.stdin.lock().await;
        stdin
            .write_all(&payload)
            .await
            .context("failed to write response")?;
        stdin
            .write_all(b"\n")
            .await
            .context("failed to write response newline")?;
        stdin.flush().await.context("failed to flush response")?;
        Ok(())
    }

    pub async fn kill(&self) -> Result<()> {
        self.inner
            .child
            .lock()
            .await
            .kill()
            .await
            .context("failed to kill app-server child")
    }

    fn spawn_stdout_task(&self, stdout: tokio::process::ChildStdout) {
        let inner = self.inner.clone();
        let notifications = inner.notifications.clone();
        let server_requests = inner.server_requests.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() || !trimmed.starts_with('{') {
                    continue;
                }

                let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                    log::debug!("ignoring non-JSON app-server stdout line: {trimmed}");
                    continue;
                };

                if let Some(method) = value.get("method").and_then(Value::as_str) {
                    if let Some(id) = value.get("id") {
                        let request = JsonRpcRequest {
                            jsonrpc: value
                                .get("jsonrpc")
                                .and_then(Value::as_str)
                                .unwrap_or("2.0")
                                .to_string(),
                            id: parse_id(id),
                            method: method.to_string(),
                            params: value.get("params").cloned().unwrap_or(Value::Null),
                        };
                        let _ = server_requests.send(request);
                    } else {
                        let notification = JsonRpcNotification {
                            jsonrpc: value
                                .get("jsonrpc")
                                .and_then(Value::as_str)
                                .unwrap_or("2.0")
                                .to_string(),
                            method: method.to_string(),
                            params: value.get("params").cloned().unwrap_or(Value::Null),
                        };
                        let _ = notifications.send(notification);
                    }
                    continue;
                }

                if let Some(id) = value.get("id") {
                    let response = JsonRpcResponse {
                        jsonrpc: value
                            .get("jsonrpc")
                            .and_then(Value::as_str)
                            .unwrap_or("2.0")
                            .to_string(),
                        id: parse_id(id),
                        result: value.get("result").cloned(),
                        error: value
                            .get("error")
                            .cloned()
                            .and_then(|error| serde_json::from_value(error).ok()),
                    };
                    let key = response.id.key();
                    if let Some(sender) = inner.pending.lock().await.remove(&key) {
                        let result = match (response.result, response.error) {
                            (Some(result), None) => Ok(result),
                            (_, Some(error)) => Err(error),
                            _ => Err(JsonRpcErrorObject {
                                code: -32603,
                                message: "malformed JSON-RPC response".to_string(),
                                data: None,
                            }),
                        };
                        let _ = sender.send(result);
                    }
                }
            }

            let mut pending = inner.pending.lock().await;
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err(JsonRpcErrorObject {
                    code: -32000,
                    message: "app-server transport closed before response".to_string(),
                    data: None,
                }));
            }
        });
    }

    fn spawn_stderr_task(&self, stderr: tokio::process::ChildStderr) {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!("[codex app-server] {line}");
            }
        });
    }
}

fn parse_id(value: &Value) -> RequestId {
    if let Some(number) = value.as_u64() {
        return RequestId::Number(number);
    }

    RequestId::String(value.as_str().unwrap_or_default().to_string())
}

fn request_timeout() -> Duration {
    let seconds = std::env::var("CLAUDE_CODEX_PROXY_JSONRPC_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(45);
    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[tokio::test]
    async fn transport_round_trips_initialize() {
        let dir = std::env::temp_dir().join(format!("codex-mock-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let script_path = dir.join("mock-codex.sh");
        fs::write(
            &script_path,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      echo '{"jsonrpc":"2.0","id":1,"result":{"userAgent":"mock"}}'
      ;;
    *'"method":"initialized"'*)
      ;;
    *'"method":"configRequirements/read"'*)
      echo '{"jsonrpc":"2.0","id":2,"result":{"requirements":{"allowedApprovalPolicies":["untrusted","on-request"],"allowedSandboxModes":["read-only","workspace-write"]}}}'
      ;;
  esac
done
"#,
        )
        .expect("script");
        let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");

        let transport = StdioTransport::spawn(StdioTransportOptions {
            binary_path: script_path.to_string_lossy().to_string(),
            current_dir: None,
            extra_env: Vec::new(),
        })
        .await
        .expect("transport");

        let initialize: Value = transport
            .request(
                "initialize",
                json!({
                    "clientInfo": { "name": "test", "version": "0.1.0" }
                }),
            )
            .await
            .expect("initialize");
        assert_eq!(initialize["userAgent"], "mock");
    }
}
