pub mod config;
pub mod db;
pub mod protocol;
pub mod router;

pub use config::{
    git_url_to_project_slug, global_config_dir, global_config_path, global_connectors_path,
    global_db_path, resolve_config_path, sanitize_project_slug, BudgetsSection, ConfigError,
    ConnectorProfile, ConnectorsConfig, MeshConfig, MeshSection, PeerConfig, ProjectSection,
    DEFAULT_BUSY_TTL_SECS, DEFAULT_MAX_PROMPT_CHARS, DEFAULT_PRESENCE_TTL_SECS,
};
pub use db::{
    Blackboard, ConnectorCallRow, HandoffRecord, InboxPage, MessageRow, PeerHealth, PeerStateRow,
    PresenceRow, ProjectUnread, TaskHistoryRow, TaskRow,
};
pub use protocol::{AdapterErrorCode, AdapterRequest, AdapterResponse};
pub use router::{MeshError, MeshRouter, RouteRequest, RouteResult};
