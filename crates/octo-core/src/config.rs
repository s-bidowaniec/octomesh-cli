use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to parse TOML config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("Invalid configuration: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    pub adapter: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub cooldown_secs: Option<u64>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

fn default_timeout_secs() -> u64 {
    120
}

fn default_max_depth() -> u32 {
    2
}

fn default_cooldown_secs() -> u64 {
    300
}

fn default_adapter_runner() -> Vec<String> {
    vec![
        "uv".to_string(),
        "run".to_string(),
        "--project".to_string(),
        "adapters".to_string(),
        "python".to_string(),
        "-m".to_string(),
        "octomesh_adapters.runner".to_string(),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshConfig {
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    #[serde(default = "default_cooldown_secs")]
    pub default_cooldown_secs: u64,
    #[serde(default = "default_adapter_runner")]
    pub adapter_runner: Vec<String>,
    #[serde(default)]
    pub roles: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub peers: HashMap<String, PeerConfig>,
}

impl MeshConfig {
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path_ref = path.as_ref();
        let raw = fs::read_to_string(path_ref).map_err(|source| ConfigError::Io {
            path: path_ref.to_path_buf(),
            source,
        })?;
        let cfg: MeshConfig = toml::from_str(&raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.adapter_runner.is_empty() {
            return Err(ConfigError::Validation(
                "adapter_runner must contain at least one executable token".to_string(),
            ));
        }
        if self.peers.is_empty() {
            return Err(ConfigError::Validation(
                "at least one peer must be defined under [peers]".to_string(),
            ));
        }
        for name in self.peers.keys().chain(self.roles.keys()) {
            if !is_valid_mesh_name(name) {
                return Err(ConfigError::Validation(format!(
                    "invalid name '{name}': must be non-empty ASCII without whitespace, \
                     commas, or '*' (reserved for broadcast)"
                )));
            }
        }
        for role in self.roles.keys() {
            if self.peers.contains_key(role) {
                return Err(ConfigError::Validation(format!(
                    "name '{role}' is used both as a role and as a peer"
                )));
            }
        }
        for (role, chain) in &self.roles {
            if chain.is_empty() {
                return Err(ConfigError::Validation(format!(
                    "role '{role}' has an empty failover chain"
                )));
            }
            for peer_name in chain {
                if !self.peers.contains_key(peer_name) {
                    return Err(ConfigError::Validation(format!(
                        "role '{role}' references undefined peer '{peer_name}'"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Resolve a target string into an ordered list of candidate peer names.
    /// If `target` matches a configured role, returns its failover chain.
    /// If `target` matches a configured peer name, returns a single-element list.
    pub fn resolve_candidates(&self, target: &str) -> Option<Vec<String>> {
        if let Some(chain) = self.roles.get(target) {
            return Some(chain.clone());
        }
        if self.peers.contains_key(target) {
            return Some(vec![target.to_string()]);
        }
        None
    }

    pub fn cooldown_for_peer(&self, peer_name: &str) -> u64 {
        self.peers
            .get(peer_name)
            .and_then(|p| p.cooldown_secs)
            .unwrap_or(self.default_cooldown_secs)
    }
}

/// Mesh names (peers, roles) must be plain ASCII without whitespace or commas:
/// they are joined with ',' into OCTOMESH_CALLER_CHAIN and used as SQLite
/// message recipients, so spaces/unicode/commas would break routing.
fn is_valid_mesh_name(name: &str) -> bool {
    !name.is_empty()
        && name != "*"
        && name.is_ascii()
        && !name.chars().any(|c| c.is_whitespace() || c == ',')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(peer_name: &str, role_name: &str) -> MeshConfig {
        let mut peers = HashMap::new();
        peers.insert(
            peer_name.to_string(),
            PeerConfig {
                adapter: "mock".to_string(),
                command: None,
                args: vec![],
                timeout_secs: 5,
                cooldown_secs: None,
                env: HashMap::new(),
            },
        );
        let mut roles = HashMap::new();
        roles.insert(role_name.to_string(), vec![peer_name.to_string()]);
        MeshConfig {
            max_depth: 2,
            default_cooldown_secs: 60,
            adapter_runner: vec!["true".to_string()],
            roles,
            peers,
        }
    }

    #[test]
    fn accepts_plain_ascii_names() {
        assert!(cfg_with("agy_window", "windows").validate().is_ok());
    }

    #[test]
    fn rejects_names_with_spaces_unicode_commas_or_star() {
        for bad in ["my peer", "agy_ö", "a,b", "*", ""] {
            assert!(
                cfg_with(bad, "role_x").validate().is_err(),
                "peer name '{bad}' must be rejected"
            );
            assert!(
                cfg_with("peer_x", bad).validate().is_err(),
                "role name '{bad}' must be rejected"
            );
        }
    }

    #[test]
    fn rejects_role_peer_name_collision() {
        assert!(cfg_with("same", "same").validate().is_err());
    }
}
