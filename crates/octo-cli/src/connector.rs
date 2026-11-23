use octo_core::{Blackboard, ConnectorProfile, ConnectorsConfig, MeshConfig};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

pub const DEFAULT_CONNECTOR_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorStatusRow {
    pub name: String,
    pub allowed_for_project: bool,
    pub binary_present: bool,
    pub env_keys_set: BTreeMap<String, bool>,
    pub last_call_ok: Option<bool>,
    pub last_call_ts: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorPermissionError {
    UnknownConnector(String),
    ConnectorNotAllowed { connector: String, project: String },
}

impl ConnectorPermissionError {
    pub fn code(&self) -> &'static str {
        match self {
            ConnectorPermissionError::UnknownConnector(_) => "unknown_connector",
            ConnectorPermissionError::ConnectorNotAllowed { .. } => "connector_not_allowed",
        }
    }

    pub fn message(&self) -> String {
        match self {
            ConnectorPermissionError::UnknownConnector(name) => {
                format!("Connector '{name}' is not defined in the global connectors registry")
            }
            ConnectorPermissionError::ConnectorNotAllowed { connector, project } => {
                format!("Connector '{connector}' is not allowed for project '{project}'")
            }
        }
    }
}

pub fn check_connector_permission<'a>(
    registry: &'a ConnectorsConfig,
    config: &MeshConfig,
    project_id: &str,
    connector_name: &str,
) -> Result<&'a ConnectorProfile, ConnectorPermissionError> {
    let profile = registry
        .connectors
        .get(connector_name)
        .ok_or_else(|| ConnectorPermissionError::UnknownConnector(connector_name.to_string()))?;
    if !config.is_connector_allowed(connector_name) {
        return Err(ConnectorPermissionError::ConnectorNotAllowed {
            connector: connector_name.to_string(),
            project: project_id.to_string(),
        });
    }
    Ok(profile)
}

fn binary_present(command: &str) -> bool {
    Path::new(command).is_file()
        || std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|dir| dir.join(command).is_file()))
            .unwrap_or(false)
}

pub fn status_rows(
    registry: &ConnectorsConfig,
    config: &MeshConfig,
    project_id: &str,
    board: &Blackboard,
) -> Vec<ConnectorStatusRow> {
    let audit = board.list_connector_calls(500).unwrap_or_default();
    registry
        .connectors
        .iter()
        .map(|(name, profile)| {
            let allowed = config.is_connector_allowed(name);
            let last_call = audit.iter().rev().find(|a| a.connector == *name);
            ConnectorStatusRow {
                name: name.clone(),
                allowed_for_project: allowed,
                binary_present: binary_present(&profile.command),
                env_keys_set: profile
                    .env_keys
                    .iter()
                    .map(|k| {
                        let set = std::env::var(k)
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false);
                        (k.clone(), set)
                    })
                    .collect(),
                last_call_ok: last_call.map(|a| a.ok),
                last_call_ts: last_call.map(|a| a.created_at.to_rfc3339()),
            }
        })
        .collect()
}

/// Redact secret env values (KEY/TOKEN/SECRET/PASSWORD) from connector error text.
fn redact_secrets(text: &str, env_keys: &[String]) -> String {
    let mut out = text.to_string();
    for key in env_keys {
        if let Ok(val) = std::env::var(key) {
            if val.len() >= 8 {
                out = out.replace(&val, "***");
            }
        }
    }
    out
}

/// Spawn the connector's stdio MCP server, send `initialize` then `request`,
/// and return the last JSON-RPC response line (with a hard timeout).
pub fn call_connector(
    profile: &ConnectorProfile,
    request_json: &str,
    timeout_secs: u64,
) -> Result<String, String> {
    let mut child = Command::new(&profile.command)
        .args(&profile.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn connector '{}': {e}", profile.command))?;

    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "octomesh-cli", "version": env!("CARGO_PKG_VERSION")}
        }
    });
    let req: serde_json::Value =
        serde_json::from_str(request_json).map_err(|e| format!("invalid JSON-RPC request: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let payload = format!("{}\n{}\n", init, req);
        stdin
            .write_all(payload.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("failed writing to connector stdin: {e}"))?;
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "connector stdout unavailable".to_string())?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    if tx.send(Ok(l)).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    break;
                }
            }
        }
    });

    let deadline = Duration::from_secs(timeout_secs.max(1));
    let mut last_response: Option<String> = None;
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        match rx.recv_timeout(deadline.saturating_sub(start.elapsed())) {
            Ok(Ok(line)) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    if v.get("id").and_then(|i| i.as_i64()) == Some(1) || last_response.is_none() {
                        last_response = Some(line);
                    }
                    if v.get("id").and_then(|i| i.as_i64()) == Some(1) {
                        break;
                    }
                } else {
                    last_response = Some(line);
                }
            }
            Ok(Err(e)) => return Err(redact_secrets(&e, &profile.env_keys)),
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    last_response.ok_or_else(|| {
        redact_secrets(
            &format!(
                "connector '{}' produced no JSON-RPC response within {timeout_secs}s",
                profile.command
            ),
            &profile.env_keys,
        )
    })
}
