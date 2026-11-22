use crate::config::MeshConfig;
use crate::db::{Blackboard, DbError, PeerHealth};
use crate::protocol::{AdapterErrorCode, AdapterRequest, AdapterResponse};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum MeshError {
    #[error("Anti-loop violation: delegation depth {depth} exceeds max_depth {max_depth}")]
    MaxDepthExceeded { depth: u32, max_depth: u32 },

    #[error("Anti-loop violation: peer '{caller}' cannot invoke itself")]
    SelfInvocation { caller: String },

    #[error("Unknown peer or role target '{0}'")]
    UnknownTarget(String),

    #[error("All candidate peers for target '{target}' are unavailable or failed: {attempts:?}")]
    AllPeersExhausted {
        target: String,
        attempts: Vec<PeerAttempt>,
    },

    #[error("Database error: {0}")]
    Db(#[from] DbError),

    #[error("Adapter subprocess error: {0}")]
    Subprocess(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerAttempt {
    pub peer: String,
    pub skipped_cooldown: bool,
    pub status: Option<AdapterErrorCode>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRequest {
    pub target: String,
    pub prompt: String,
    pub caller: String,
    pub depth: u32,
    pub include_diff: bool,
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResult {
    pub task_id: String,
    pub target: String,
    pub resolved_peer: String,
    pub adapter: String,
    pub output: String,
    pub attempts: Vec<PeerAttempt>,
}

pub struct MeshRouter {
    config: MeshConfig,
    board: Blackboard,
    working_dir: PathBuf,
    min_depth: u32,
    ancestors: Vec<String>,
}

impl MeshRouter {
    pub fn new(config: MeshConfig, board: Blackboard, working_dir: impl Into<PathBuf>) -> Self {
        let peer_names: Vec<String> = config.peers.keys().cloned().collect();
        let _ = board.ensure_peers(&peer_names);
        Self {
            config,
            board,
            working_dir: working_dir.into(),
            min_depth: 0,
            ancestors: Vec::new(),
        }
    }

    pub fn with_env_context(mut self, min_depth: u32, ancestors: Vec<String>) -> Self {
        self.min_depth = min_depth;
        self.ancestors = ancestors;
        self
    }

    pub fn config(&self) -> &MeshConfig {
        &self.config
    }

    pub fn blackboard(&self) -> &Blackboard {
        &self.board
    }

    pub async fn route(&self, req: RouteRequest) -> Result<RouteResult, MeshError> {
        // 1. Anti-loop guard: check depth (honoring inherited process depth)
        let effective_depth = req.depth.max(self.min_depth);
        if effective_depth > self.config.max_depth {
            return Err(MeshError::MaxDepthExceeded {
                depth: effective_depth,
                max_depth: self.config.max_depth,
            });
        }

        // 2. Anti-loop guard: reject direct self-invocation or ancestor cycle
        if !req.caller.is_empty() && req.caller == req.target {
            return Err(MeshError::SelfInvocation {
                caller: req.caller.clone(),
            });
        }
        if self.ancestors.iter().any(|a| a == &req.target) {
            return Err(MeshError::SelfInvocation {
                caller: req.target.clone(),
            });
        }

        // 3. Resolve target to peer list (either single peer or role chain)
        let candidates = self
            .config
            .resolve_candidates(&req.target)
            .ok_or_else(|| MeshError::UnknownTarget(req.target.clone()))?;

        let mut attempts = Vec::new();
        let task_id = Uuid::new_v4().to_string();

        for peer_name in candidates {
            // Also prevent self-delegation or ancestor loop when target was a role
            if (!req.caller.is_empty() && peer_name == req.caller)
                || self.ancestors.iter().any(|a| a == &peer_name)
            {
                attempts.push(PeerAttempt {
                    peer: peer_name,
                    skipped_cooldown: false,
                    status: None,
                    detail: "Skipped caller/ancestor peer to prevent loop".to_string(),
                });
                continue;
            }

            if !self.board.is_peer_available(&peer_name)? {
                info!(peer = %peer_name, "Peer is currently on cooldown; skipping");
                attempts.push(PeerAttempt {
                    peer: peer_name,
                    skipped_cooldown: true,
                    status: None,
                    detail: "Peer on active cooldown".to_string(),
                });
                continue;
            }

            let peer_cfg = match self.config.peers.get(&peer_name) {
                Some(cfg) => cfg,
                None => continue,
            };

            self.board
                .set_peer_health(&peer_name, PeerHealth::Busy, None, None)?;

            let mut env = peer_cfg.env.clone();
            env.insert("OCTOMESH_DEPTH".to_string(), effective_depth.to_string());
            let mut chain = self.ancestors.clone();
            if !req.caller.is_empty() && !chain.iter().any(|c| c == &req.caller) {
                chain.push(req.caller.clone());
            }
            if !chain.iter().any(|c| c == &peer_name) {
                chain.push(peer_name.clone());
            }
            env.insert("OCTOMESH_CALLER_CHAIN".to_string(), chain.join(","));

            let adapter_req = AdapterRequest {
                peer: peer_name.clone(),
                adapter: peer_cfg.adapter.clone(),
                prompt: req.prompt.clone(),
                caller: req.caller.clone(),
                depth: effective_depth,
                include_diff: req.include_diff,
                readonly: req.readonly,
                timeout_secs: peer_cfg.timeout_secs,
                command: peer_cfg.command.clone(),
                args: peer_cfg.args.clone(),
                env,
                working_dir: self.working_dir.to_string_lossy().to_string(),
            };

            let resp = self.invoke_adapter(&adapter_req).await;

            match resp {
                Ok(adapter_resp) if adapter_resp.status == AdapterErrorCode::Ok => {
                    self.board
                        .set_peer_health(&peer_name, PeerHealth::Ready, None, None)?;
                    self.board.record_task(
                        &task_id,
                        &req.caller,
                        &req.target,
                        &peer_name,
                        req.depth,
                        "ok",
                        &req.prompt,
                        None,
                    )?;
                    attempts.push(PeerAttempt {
                        peer: peer_name.clone(),
                        skipped_cooldown: false,
                        status: Some(AdapterErrorCode::Ok),
                        detail: "Succeeded".to_string(),
                    });
                    return Ok(RouteResult {
                        task_id,
                        target: req.target,
                        resolved_peer: peer_name,
                        adapter: peer_cfg.adapter.clone(),
                        output: adapter_resp.output,
                        attempts,
                    });
                }
                Ok(adapter_resp) => {
                    let err_msg = adapter_resp
                        .error_message
                        .clone()
                        .unwrap_or_else(|| format!("Adapter returned {:?}", adapter_resp.status));
                    warn!(
                        peer = %peer_name,
                        status = ?adapter_resp.status,
                        error = %err_msg,
                        "Peer failed; evaluating cooldown & failover"
                    );

                    if adapter_resp.status.triggers_cooldown() {
                        let cd = self.config.cooldown_for_peer(&peer_name);
                        let health = match adapter_resp.status {
                            AdapterErrorCode::RateLimited => PeerHealth::RateLimited,
                            AdapterErrorCode::CliNotFound | AdapterErrorCode::AuthError => {
                                PeerHealth::Dead
                            }
                            _ => PeerHealth::RateLimited,
                        };
                        self.board
                            .set_peer_health(&peer_name, health, Some(cd), Some(&err_msg))?;
                    } else {
                        self.board.set_peer_health(
                            &peer_name,
                            PeerHealth::Ready,
                            None,
                            Some(&err_msg),
                        )?;
                    }

                    self.board.record_task(
                        &task_id,
                        &req.caller,
                        &req.target,
                        &peer_name,
                        req.depth,
                        "failed",
                        &req.prompt,
                        Some(&err_msg),
                    )?;

                    attempts.push(PeerAttempt {
                        peer: peer_name,
                        skipped_cooldown: false,
                        status: Some(adapter_resp.status),
                        detail: err_msg,
                    });
                }
                Err(sub_err) => {
                    let err_msg = sub_err.to_string();
                    let cd = self.config.cooldown_for_peer(&peer_name);
                    self.board.set_peer_health(
                        &peer_name,
                        PeerHealth::RateLimited,
                        Some(cd),
                        Some(&err_msg),
                    )?;
                    self.board.record_task(
                        &task_id,
                        &req.caller,
                        &req.target,
                        &peer_name,
                        req.depth,
                        "subprocess_error",
                        &req.prompt,
                        Some(&err_msg),
                    )?;
                    attempts.push(PeerAttempt {
                        peer: peer_name,
                        skipped_cooldown: false,
                        status: Some(AdapterErrorCode::Timeout),
                        detail: err_msg,
                    });
                }
            }
        }

        Err(MeshError::AllPeersExhausted {
            target: req.target,
            attempts,
        })
    }

    async fn invoke_adapter(&self, req: &AdapterRequest) -> Result<AdapterResponse, MeshError> {
        let mut cmd = Command::new(&self.config.adapter_runner[0]);
        if self.config.adapter_runner.len() > 1 {
            cmd.args(&self.config.adapter_runner[1..]);
        }
        cmd.current_dir(&self.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (k, v) in &req.env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| MeshError::Subprocess(format!("Failed to spawn adapter runner: {e}")))?;

        let payload = serde_json::to_vec(req)
            .map_err(|e| MeshError::Subprocess(format!("Failed to serialize request: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&payload).await.map_err(|e| {
                MeshError::Subprocess(format!("Failed writing to adapter stdin: {e}"))
            })?;
            stdin
                .shutdown()
                .await
                .map_err(|e| MeshError::Subprocess(format!("Failed closing adapter stdin: {e}")))?;
        }

        let grace_timeout = Duration::from_secs(req.timeout_secs.saturating_add(5));
        let output = timeout(grace_timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                MeshError::Subprocess(format!(
                    "Adapter subprocess timed out after {}s",
                    req.timeout_secs
                ))
            })?
            .map_err(|e| MeshError::Subprocess(format!("Adapter wait failed: {e}")))?;

        let stdout_str = String::from_utf8_lossy(&output.stdout);
        let stderr_str = String::from_utf8_lossy(&output.stderr);

        serde_json::from_str::<AdapterResponse>(stdout_str.trim()).map_err(|e| {
            MeshError::Subprocess(format!(
                "Invalid JSON from adapter (exit={:?}, parse_err={}): stdout='{}' stderr='{}'",
                output.status.code(),
                e,
                stdout_str.trim(),
                stderr_str.trim()
            ))
        })
    }
}
