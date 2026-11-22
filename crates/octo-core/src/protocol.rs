use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AdapterErrorCode {
    Ok,
    RateLimited,
    Timeout,
    AuthError,
    CliNotFound,
    ExecError,
}

impl AdapterErrorCode {
    pub fn should_failover(&self) -> bool {
        matches!(
            self,
            AdapterErrorCode::RateLimited
                | AdapterErrorCode::Timeout
                | AdapterErrorCode::AuthError
                | AdapterErrorCode::CliNotFound
                | AdapterErrorCode::ExecError
        )
    }

    pub fn triggers_cooldown(&self) -> bool {
        matches!(
            self,
            AdapterErrorCode::RateLimited
                | AdapterErrorCode::Timeout
                | AdapterErrorCode::AuthError
                | AdapterErrorCode::CliNotFound
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterRequest {
    pub peer: String,
    pub adapter: String,
    pub prompt: String,
    pub caller: String,
    pub depth: u32,
    pub include_diff: bool,
    pub readonly: bool,
    pub timeout_secs: u64,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub working_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterResponse {
    pub status: AdapterErrorCode,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub peer: String,
    #[serde(default)]
    pub adapter: String,
    #[serde(default)]
    pub duration_ms: u64,
}
