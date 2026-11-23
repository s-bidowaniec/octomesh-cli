use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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

pub const DEFAULT_PRESENCE_TTL_SECS: u64 = 120;
pub const DEFAULT_BUSY_TTL_SECS: u64 = 300;
pub const DEFAULT_MAX_PROMPT_CHARS: usize = 20_000;

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
    /// Per-peer context budget override (chars); falls back to [budgets].
    #[serde(default)]
    pub max_prompt_chars: Option<usize>,
    /// D-Bus window peer: well-known bus name (e.g. org.kde.konsole).
    #[serde(default)]
    pub window_bus: Option<String>,
    /// D-Bus object path of the target window/session.
    #[serde(default)]
    pub window_path: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MeshSection {
    /// Explicit project id; overrides the git-remote slug.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default = "default_presence_ttl")]
    pub presence_ttl_secs: u64,
    #[serde(default = "default_busy_ttl")]
    pub busy_ttl_secs: u64,
}

fn default_presence_ttl() -> u64 {
    DEFAULT_PRESENCE_TTL_SECS
}

fn default_busy_ttl() -> u64 {
    DEFAULT_BUSY_TTL_SECS
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectSection {
    #[serde(default)]
    pub name: Option<String>,
    /// Connector names this project may call via the gateway; "*" allows all.
    #[serde(default)]
    pub allowed_connectors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetsSection {
    #[serde(default = "default_max_prompt_chars")]
    pub default_max_prompt_chars: usize,
}

fn default_max_prompt_chars() -> usize {
    DEFAULT_MAX_PROMPT_CHARS
}

impl Default for BudgetsSection {
    fn default() -> Self {
        Self {
            default_max_prompt_chars: DEFAULT_MAX_PROMPT_CHARS,
        }
    }
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
    #[serde(default)]
    pub mesh: MeshSection,
    #[serde(default)]
    pub project: ProjectSection,
    #[serde(default)]
    pub budgets: BudgetsSection,
}

impl Default for MeshConfig {
    fn default() -> Self {
        Self {
            max_depth: default_max_depth(),
            default_cooldown_secs: default_cooldown_secs(),
            adapter_runner: default_adapter_runner(),
            roles: HashMap::new(),
            peers: HashMap::new(),
            mesh: MeshSection::default(),
            project: ProjectSection::default(),
            budgets: BudgetsSection::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectorProfile {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Env var NAMES the connector needs (values are never stored or printed).
    #[serde(default)]
    pub env_keys: Vec<String>,
    #[serde(default)]
    pub experimental: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub minimal_scopes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectorsConfig {
    #[serde(default)]
    pub connectors: BTreeMap<String, ConnectorProfile>,
}

impl ConnectorsConfig {
    /// Load the global connector registry (`~/.config/octomesh/connectors.toml`).
    /// A missing file yields an empty registry (no connectors configured).
    pub fn load_global() -> Result<Self, ConfigError> {
        let path = global_connectors_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        let cfg: ConnectorsConfig = toml::from_str(&raw)?;
        Ok(cfg)
    }
}

pub fn global_config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("octomesh");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config/octomesh")
}

pub fn global_config_path() -> PathBuf {
    global_config_dir().join("octomesh.toml")
}

pub fn global_connectors_path() -> PathBuf {
    global_config_dir().join("connectors.toml")
}

pub fn global_db_path() -> PathBuf {
    if let Ok(db_env) = std::env::var("OCTOMESH_DB") {
        if !db_env.trim().is_empty() {
            return PathBuf::from(db_env);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".octomesh/state.db")
}

/// Config resolution order: `./octomesh.toml`, then `$OCTOMESH_CONFIG`, then the global path.
pub fn resolve_config_path(working_dir: &Path) -> (PathBuf, &'static str) {
    let local = working_dir.join("octomesh.toml");
    if local.exists() {
        return (local, "project-local");
    }
    if let Ok(env_cfg) = std::env::var("OCTOMESH_CONFIG") {
        if !env_cfg.trim().is_empty() {
            return (PathBuf::from(env_cfg), "OCTOMESH_CONFIG");
        }
    }
    (global_config_path(), "global")
}

/// Sanitize a project name into a plain ASCII slug (no whitespace).
pub fn sanitize_project_slug(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `git@github.com:org/repo.git` / `https://github.com/org/repo` -> `github.com.org.repo`
pub fn git_url_to_project_slug(raw_url: &str) -> String {
    let mut s = raw_url.trim().to_string();
    if let Some(stripped) = s.strip_suffix(".git") {
        s = stripped.to_string();
    }
    if let Some(at_pos) = s.find('@') {
        s = s[at_pos + 1..].to_string();
    }
    for scheme in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            s = rest.to_string();
        }
    }
    s = s.replace(':', "/");
    let parts: Vec<String> = s
        .split('/')
        .filter(|part| !part.is_empty())
        .map(sanitize_project_slug)
        .filter(|part| !part.is_empty())
        .collect();
    parts.join(".")
}

fn git_remote_slug(working_dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(working_dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() {
        return None;
    }
    let slug = git_url_to_project_slug(&url);
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
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

    /// Load config using the standard resolution order from a working directory.
    pub fn load_from_dir(working_dir: &Path) -> Result<(Self, PathBuf, &'static str), ConfigError> {
        let (path, source) = resolve_config_path(working_dir);
        let cfg = Self::load_from_path(&path)?;
        Ok((cfg, path, source))
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
        if self.peers.contains_key("*") || self.roles.contains_key("*") {
            return Err(ConfigError::Validation(
                "name '*' is reserved for mailbox broadcasts".to_string(),
            ));
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

    /// Project identity: `[project].name`, then `[mesh].project`, then the slug of the
    /// git `origin` remote of `working_dir`, else `"default"`.
    pub fn project_id(&self, working_dir: &Path) -> String {
        if let Some(ref p) = self.project.name {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return sanitize_project_slug(trimmed);
            }
        }
        if let Some(ref p) = self.mesh.project {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return sanitize_project_slug(trimmed);
            }
        }
        if let Some(slug) = git_remote_slug(working_dir) {
            return slug;
        }
        "default".to_string()
    }

    /// Context budget (chars) for a peer: per-peer override, else `[budgets]` default.
    pub fn max_prompt_chars_for_peer(&self, peer_name: &str) -> usize {
        if let Some(peer) = self.peers.get(peer_name) {
            if let Some(lim) = peer.max_prompt_chars {
                return lim;
            }
        }
        if self.budgets.default_max_prompt_chars > 0 {
            self.budgets.default_max_prompt_chars
        } else {
            DEFAULT_MAX_PROMPT_CHARS
        }
    }

    /// Whether the project may call this connector through the gateway.
    pub fn is_connector_allowed(&self, connector_name: &str) -> bool {
        self.project
            .allowed_connectors
            .iter()
            .any(|c| c.trim() == "*" || c.trim() == connector_name.trim())
    }
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
                max_prompt_chars: None,
                window_bus: None,
                window_path: None,
            },
        );
        let mut roles = HashMap::new();
        roles.insert(role_name.to_string(), vec![peer_name.to_string()]);
        MeshConfig {
            roles,
            peers,
            ..MeshConfig::default()
        }
    }

    #[test]
    fn reserves_only_the_broadcast_target() {
        assert!(cfg_with("*", "reviewer").validate().is_err());
        assert!(cfg_with("agy", "*").validate().is_err());
        assert!(cfg_with("all", "reviewer").validate().is_ok());
    }

    #[test]
    fn project_slug_from_git_urls() {
        assert_eq!(
            git_url_to_project_slug("git@github.com:s-bidowaniec/octomesh-cli.git"),
            "github.com.s-bidowaniec.octomesh-cli"
        );
        assert_eq!(
            git_url_to_project_slug("https://github.com/s-bidowaniec/octomesh-cli"),
            "github.com.s-bidowaniec.octomesh-cli"
        );
        assert_eq!(sanitize_project_slug("my proj/öü"), "my_proj___");
    }

    #[test]
    fn project_id_prefers_explicit_config_then_git_then_default() {
        let mut cfg = cfg_with("agy", "reviewer");
        let dir = std::env::temp_dir();
        assert_eq!(cfg.project_id(&dir), "default"); // temp dir is not the repo
        cfg.project.name = Some("My Proj".to_string());
        assert_eq!(cfg.project_id(&dir), "My_Proj");
        cfg.project.name = None;
        cfg.mesh.project = Some("meshproj".to_string());
        assert_eq!(cfg.project_id(&dir), "meshproj");
    }

    #[test]
    fn per_peer_budget_overrides_default() {
        let mut cfg = cfg_with("codex", "coder");
        assert_eq!(cfg.max_prompt_chars_for_peer("codex"), 20_000);
        cfg.peers.get_mut("codex").unwrap().max_prompt_chars = Some(100);
        assert_eq!(cfg.max_prompt_chars_for_peer("codex"), 100);
        cfg.budgets.default_max_prompt_chars = 500;
        assert_eq!(cfg.max_prompt_chars_for_peer("other"), 500);
    }

    #[test]
    fn connector_allowlist_supports_star() {
        let mut cfg = cfg_with("agy", "reviewer");
        assert!(!cfg.is_connector_allowed("github"));
        cfg.project.allowed_connectors = vec!["github".to_string(), "drive".to_string()];
        assert!(cfg.is_connector_allowed("github"));
        assert!(!cfg.is_connector_allowed("gitlab"));
        cfg.project.allowed_connectors = vec!["*".to_string()];
        assert!(cfg.is_connector_allowed("anything"));
    }
}
