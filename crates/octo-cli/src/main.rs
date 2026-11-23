use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use octo_core::{Blackboard, MeshConfig, MeshRouter, RouteRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{error, info};

mod backup;
mod connector;
mod doctor;
mod workspace;
use workspace::Workspace;

const DEFAULT_CONFIG_TOML: &str = include_str!("../../../octomesh.toml.example");
const DEFAULT_PROTOCOL_MD: &str = include_str!("../../../templates/PROTOCOL.md");

#[derive(Parser)]
#[command(
    name = "octo",
    version,
    about = "Hybrid P2P & failover mesh bridge for native terminal AI agents"
)]
struct Cli {
    /// Path to octomesh.toml configuration file (defaults to ./octomesh.toml, $OCTOMESH_CONFIG, or ~/.config/octomesh/octomesh.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Path to SQLite blackboard database (defaults to ./.octomesh/state.db, $OCTOMESH_DB, or ~/.config/octomesh/state.db)
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scaffold octomesh.toml, .octomesh/PROTOCOL.md, and optionally sync AGENTS.md / CLAUDE.md
    Init {
        /// Append mesh protocol pointer to AGENTS.md and CLAUDE.md if present (or create AGENTS.md)
        #[arg(long)]
        sync_agents: bool,

        /// Overwrite existing octomesh.toml and PROTOCOL.md
        #[arg(long)]
        force: bool,
    },

    /// Display current peer health, cooldown timers, roles, presence, and recent handoffs
    Status,

    /// Install octo + adapters under a prefix and print the shell env snippet
    Install {
        /// Install prefix (default ~/.local)
        #[arg(long)]
        prefix: Option<PathBuf>,
    },

    /// Structured diagnostics: binary, config, blackboard, runner, peers, D-Bus windows
    Doctor,

    /// List registered peers (presence table); --all includes stale entries
    Presence {
        #[arg(long)]
        all: bool,
    },

    /// List projects with unread mailbox counts for a peer
    Projects {
        #[arg(long)]
        peer: Option<String>,
    },

    /// List tracked tasks (queued/delivered/accepted/done/failed/claimed)
    Tasks {
        #[arg(long)]
        peer: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },

    /// Acknowledge a delivered/queued task (receiver side of AF-04)
    Ack {
        task_id: String,
        #[arg(long)]
        note: Option<String>,
    },

    /// Atomically claim the oldest queued task for a peer (AF-05 lease)
    Next {
        #[arg(long)]
        peer: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 300)]
        lease_secs: i64,
    },

    /// Mark a task done
    Done { id: String },

    /// Mark a task failed with an optional reason
    Fail {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },

    /// Show the context-budget usage for a peer (limit, used, remaining)
    Budget {
        #[arg(long)]
        peer: Option<String>,
    },

    /// List connectors from the global registry with per-project permission status
    Connectors {
        /// Show the last 20 audited connector calls instead
        #[arg(long)]
        audit: bool,
    },

    /// Connector gateway subcommands
    Connector {
        #[command(subcommand)]
        command: ConnectorCommand,
    },

    /// Share a workspace file link through a connector into the mesh mailbox
    Share {
        /// File path relative to the current directory (must stay inside it)
        file: PathBuf,
        /// Connector name from the global registry (e.g. drive, github)
        #[arg(long)]
        via: String,
        /// Recipient peer (or '*')
        #[arg(long)]
        to: String,
        #[arg(long)]
        message: Option<String>,
        /// Print the connector payload and mailbox preview without calling anything
        #[arg(long)]
        dry_run: bool,
    },

    /// Clear cooldown and reset peer health back to ready
    Reset {
        /// Specific peer name to reset (omit to reset all peers)
        peer: Option<String>,
    },

    /// Route a prompt to a peer or role chain with automatic failover
    Ask {
        /// Target peer name (e.g. claude, codex, vibe, agy) or role (e.g. coder, reviewer, fast, demo)
        target: String,

        /// Prompt or task instructions
        prompt: String,

        /// Identity of calling agent (defaults to $OCTOMESH_DEFAULT_CALLER or 'human-cli')
        #[arg(long)]
        caller: Option<String>,

        /// Current delegation depth
        #[arg(long, default_value_t = 0)]
        depth: u32,

        /// Attach current git status and truncated git diff to prompt context
        #[arg(long)]
        include_diff: bool,

        /// Instruct target agent to operate in strict read-only / audit mode
        #[arg(long)]
        readonly: bool,
    },

    /// Send an async message to a peer, role members, or '*' (broadcast)
    Send {
        /// Recipient: peer name, role (delivered to each member), or '*' (broadcast)
        target: String,

        /// Message body (keep it short and structured)
        message: String,

        /// Author identity (defaults to $OCTOMESH_DEFAULT_CALLER or 'human-cli')
        #[arg(long)]
        author: Option<String>,

        /// Optional reference to a task_id from `ask` / mesh_ask
        #[arg(long)]
        ref_task: Option<String>,

        /// Project scope override (defaults to the resolved project id)
        #[arg(long)]
        project: Option<String>,
    },

    /// List direct messages and broadcasts for a peer (project-scoped by default)
    Inbox {
        /// Peer identity to read as (defaults to $OCTOMESH_DEFAULT_CALLER or 'human-cli')
        #[arg(long)]
        peer: Option<String>,

        /// Include already-read messages
        #[arg(long)]
        all: bool,

        /// Mark listed messages as read
        #[arg(long)]
        mark_read: bool,

        /// Project scope override (defaults to the resolved project id)
        #[arg(long)]
        project: Option<String>,

        /// Opt into messages from all projects
        #[arg(long)]
        all_projects: bool,

        /// Acknowledge these message ids (sets ack_at on own rows)
        #[arg(long)]
        ack: Vec<i64>,
    },

    /// Run a local Model Context Protocol (MCP) JSON-RPC 2.0 server over stdio
    McpServe,

    /// Manage the project's local shared files and cloud export folder
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
}

#[derive(Subcommand)]
enum ConnectorCommand {
    /// Proxy a JSON-RPC call to a connector's stdio MCP server (permission-checked, audited)
    Call {
        /// Connector name from the global registry
        name: String,
        /// JSON-RPC request as a JSON string
        request: Option<String>,
        /// Read the JSON-RPC request from a file instead
        #[arg(long)]
        payload_file: Option<PathBuf>,
        #[arg(long)]
        timeout: Option<u64>,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Create the shared files and exports directories
    Init,
    /// Print the project root and shared directories as JSON
    Info,
    /// Copy a completed file from the shared files directory to exports
    Publish {
        /// Relative path within the shared files directory
        file: PathBuf,
    },
    /// Exchange immutable workspace revisions through an encrypted rclone remote
    Backup(backup::BackupArgs),
}

fn global_config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(&home).join(".octomesh"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        dirs.push(PathBuf::from(xdg).join("octomesh"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config").join("octomesh"));
    }
    dirs
}

fn resolve_paths(config_arg: Option<PathBuf>, db_arg: Option<PathBuf>) -> (PathBuf, PathBuf) {
    let local_cfg = PathBuf::from("octomesh.toml");
    let global_dirs = global_config_dirs();

    let (config_path, global_dir_used) = if let Some(explicit) = config_arg {
        (explicit, None)
    } else if local_cfg.exists() {
        (local_cfg, None)
    } else if let Ok(env_cfg) = std::env::var("OCTOMESH_CONFIG") {
        let p = PathBuf::from(env_cfg);
        let parent = p.parent().map(PathBuf::from);
        (p, parent)
    } else if let Some(found) = global_dirs
        .iter()
        .find(|d| d.join("octomesh.toml").exists())
    {
        (found.join("octomesh.toml"), Some(found.clone()))
    } else {
        (PathBuf::from("octomesh.toml"), None)
    };

    let db_path = if let Some(explicit) = db_arg {
        explicit
    } else if let Ok(env_db) = std::env::var("OCTOMESH_DB") {
        PathBuf::from(env_db)
    } else if let Some(gdir) = global_dir_used {
        gdir.join("state.db")
    } else {
        PathBuf::from(".octomesh/state.db")
    };

    (config_path, db_path)
}

fn env_router_context() -> (u32, Vec<String>) {
    let min_depth = std::env::var("OCTOMESH_DEPTH")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .map(|d| d.saturating_add(1))
        .unwrap_or(0);

    let ancestors = std::env::var("OCTOMESH_CALLER_CHAIN")
        .ok()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    (min_depth, ancestors)
}

/// Resolve the project id for the current directory from the loaded config.
fn current_project(config: &MeshConfig) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    config.project_id(&cwd)
}

fn detect_tty() -> Option<String> {
    if let Ok(tty) = std::env::var("TTY") {
        let t = tty.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Ok(link) = fs::read_link("/proc/self/fd/0") {
        let s = link.to_string_lossy().to_string();
        if !s.is_empty() && !s.starts_with("pipe:") && !s.starts_with("socket:") {
            return Some(s);
        }
    }
    None
}

fn detect_host() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

/// AF-03: register the calling peer in the presence table on every client start
/// (best effort — never fails the actual command).
fn register_presence_best_effort(config_arg: Option<&Path>, db_arg: Option<&Path>) {
    let (cfg_path, db_path) = resolve_paths(
        config_arg.map(|p| p.to_path_buf()),
        db_arg.map(|p| p.to_path_buf()),
    );
    let Ok(config) = MeshConfig::load_from_path(&cfg_path) else {
        return;
    };
    let Ok(board) = Blackboard::open(&db_path) else {
        return;
    };
    let peer = default_identity(None);
    let now = chrono::Utc::now();
    let row = octo_core::PresenceRow {
        peer,
        pid: std::process::id() as i64,
        host: detect_host(),
        tty: detect_tty(),
        started_at: now,
        last_seen: now,
        meta: Some(json!({"cwd": std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()}).to_string()),
    };
    let _ = board.upsert_presence(&row);
    let ttl = config.mesh.presence_ttl_secs.max(1) as i64;
    let cutoff = now - chrono::Duration::seconds(ttl);
    let _ = board.prune_presence_before(cutoff);
}

#[tokio::main]
async fn main() -> Result<()> {
    // Strictly direct all logs to stderr so stdout remains clean for CLI / MCP JSON-RPC output
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // AF-03: every normal client start registers itself in the presence table.
    if !matches!(cli.command, Commands::Init { .. }) {
        register_presence_best_effort(cli.config.as_deref(), cli.db.as_deref());
    }

    match cli.command {
        Commands::Init { sync_agents, force } => {
            let cfg_path = cli.config.unwrap_or_else(|| PathBuf::from("octomesh.toml"));
            cmd_init(&cfg_path, sync_agents, force)
        }
        Commands::Status => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_status(&cfg_path, &db_path)
        }
        Commands::Install { prefix } => {
            let prefix = prefix.unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(".local")
            });
            let exe = std::env::current_exe().ok();
            let snippet = doctor::install_octomesh(&prefix, exe.as_deref())
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("Installed octo under {}", prefix.display());
            println!("Add to your shell rc:\n{snippet}");
            Ok(())
        }
        Commands::Doctor => {
            let (cfg_path, db_path) = resolve_paths(cli.config.clone(), cli.db.clone());
            cmd_doctor(&cfg_path, &db_path)
        }
        Commands::Presence { all } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_presence(&cfg_path, &db_path, all)
        }
        Commands::Projects { peer } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_projects(&cfg_path, &db_path, peer)
        }
        Commands::Tasks {
            peer,
            status,
            limit,
        } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_tasks(
                &cfg_path,
                &db_path,
                peer.as_deref(),
                status.as_deref(),
                limit,
            )
        }
        Commands::Ack { task_id, note } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_ack(&cfg_path, &db_path, &task_id, note.as_deref())
        }
        Commands::Next {
            peer,
            project,
            lease_secs,
        } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_next(&cfg_path, &db_path, peer, project, lease_secs)
        }
        Commands::Done { id } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_task_status(&cfg_path, &db_path, &id, "done", None)
        }
        Commands::Fail { id, reason } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_task_status(&cfg_path, &db_path, &id, "failed", reason.as_deref())
        }
        Commands::Budget { peer } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_budget(&cfg_path, &db_path, peer)
        }
        Commands::Connectors { audit } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_connectors(&cfg_path, &db_path, audit)
        }
        Commands::Connector { command } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_connector(&cfg_path, &db_path, command)
        }
        Commands::Share {
            file,
            via,
            to,
            message,
            dry_run,
        } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_share(
                &cfg_path,
                &db_path,
                &file,
                &via,
                &to,
                message.as_deref(),
                dry_run,
            )
        }
        Commands::Reset { peer } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_reset(&cfg_path, &db_path, peer.as_deref())
        }
        Commands::Ask {
            target,
            prompt,
            caller,
            depth,
            include_diff,
            readonly,
        } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            let resolved_caller = caller
                .or_else(|| std::env::var("OCTOMESH_DEFAULT_CALLER").ok())
                .unwrap_or_else(|| "human-cli".to_string());
            cmd_ask(
                &cfg_path,
                &db_path,
                RouteRequest {
                    target,
                    prompt,
                    caller: resolved_caller,
                    depth,
                    include_diff,
                    readonly,
                },
            )
            .await
        }
        Commands::Send {
            target,
            message,
            author,
            ref_task,
            project,
        } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_send(
                &cfg_path, &db_path, &target, &message, author, ref_task, project,
            )
        }
        Commands::Inbox {
            peer,
            all,
            mark_read,
            project,
            all_projects,
            ack,
        } => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_inbox(
                &cfg_path,
                &db_path,
                peer,
                all,
                mark_read,
                project,
                all_projects,
                &ack,
            )
        }
        Commands::McpServe => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_mcp_serve(&cfg_path, &db_path).await
        }
        Commands::Workspace { command } => cmd_workspace(command),
    }
}

fn active_workspace(cwd: &Path) -> Option<Workspace> {
    Workspace::discover(cwd).filter(|workspace| workspace.ensure_dirs(false).is_ok())
}

fn cmd_workspace(command: WorkspaceCommand) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = Workspace::discover(&cwd)
        .context("No OctoMesh project found. Run octo init at the project root first")?;
    match command {
        WorkspaceCommand::Init | WorkspaceCommand::Info => {
            workspace.ensure_dirs(matches!(command, WorkspaceCommand::Init))?;
            println!("{}", serde_json::to_string_pretty(&workspace)?);
        }
        WorkspaceCommand::Publish { file } => {
            println!("{}", workspace.publish(&file)?.display());
        }
        WorkspaceCommand::Backup(args) => backup::run(&workspace, args)?,
    }
    Ok(())
}

fn cmd_init(config_path: &Path, sync_agents: bool, force: bool) -> Result<()> {
    if !config_path.exists() || force {
        if let Some(parent) = config_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(config_path, DEFAULT_CONFIG_TOML)
            .with_context(|| format!("Failed to write {}", config_path.display()))?;
        println!("Created {}", config_path.display());
    } else {
        println!(
            "Skipped {} (already exists; pass --force to overwrite)",
            config_path.display()
        );
    }

    let octo_dir = Path::new(".octomesh");
    fs::create_dir_all(octo_dir)?;
    let protocol_path = octo_dir.join("PROTOCOL.md");
    if !protocol_path.exists() || force {
        fs::write(&protocol_path, DEFAULT_PROTOCOL_MD)?;
        println!("Created {}", protocol_path.display());
    }

    if sync_agents {
        let marker = "## Octomesh P2P Agent Mesh";
        let snippet = "\n## Octomesh P2P Agent Mesh\n\nThis repository uses `octomesh-cli` (`octo`) for peer-to-peer agent delegation and failover across `claude`, `codex`, `vibe`, and `agy`.\nRead `.octomesh/PROTOCOL.md` before invoking `mesh_ask` or `mesh_handoff`.\n";

        for filename in ["AGENTS.md", "CLAUDE.md"] {
            let p = Path::new(filename);
            if p.exists() {
                let existing = fs::read_to_string(p)?;
                if !existing.contains(marker) {
                    let mut updated = existing;
                    updated.push_str(snippet);
                    fs::write(p, updated)?;
                    println!("Appended mesh instructions to {filename}");
                }
            } else if filename == "AGENTS.md" {
                fs::write(p, snippet.trim_start())?;
                println!("Created {filename} with mesh instructions");
            }
        }
    }

    Ok(())
}

fn cmd_reset(config_path: &Path, db_path: &Path, peer: Option<&str>) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let peer_names: Vec<String> = config.peers.keys().cloned().collect();
    board.ensure_peers(&peer_names)?;

    let affected = board.reset_peer_health(peer)?;
    match peer {
        Some(name) => println!("Reset peer '{name}' ({affected} row updated)"),
        None => println!("Reset all peers ({affected} rows updated)"),
    }
    Ok(())
}

fn cmd_doctor(config_path: &Path, db_path: &Path) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path).ok();
    let (checks, all_ok) = doctor::run_doctor_checks(Some(config_path), db_path, config.as_ref());
    println!("=== Octomesh Doctor ===");
    for check in &checks {
        println!("{}", check.format_line());
    }
    if !all_ok {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_presence(config_path: &Path, db_path: &Path, all: bool) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let ttl = config.mesh.presence_ttl_secs.max(1) as i64;
    let now = chrono::Utc::now();
    let rows = board.list_presence()?;
    println!("=== Presence (ttl={}s) ===", ttl);
    for row in rows {
        let age = now.signed_duration_since(row.last_seen).num_seconds();
        let state = if age <= ttl { "LIVE" } else { "STALE" };
        if !all && state == "STALE" {
            continue;
        }
        println!(
            "  {:<16} {} pid={} host={} tty={} last_seen={}",
            row.peer,
            state,
            row.pid,
            row.host,
            row.tty.as_deref().unwrap_or("-"),
            row.last_seen.to_rfc3339()
        );
    }
    Ok(())
}

fn cmd_projects(config_path: &Path, db_path: &Path, peer: Option<String>) -> Result<()> {
    MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let peer = default_identity(peer);
    let rows = board.list_projects_with_unread(&peer)?;
    println!("=== Projects with unread for '{peer}' ===");
    if rows.is_empty() {
        println!("  (none)");
    }
    for row in rows {
        println!("  {:<40} unread={}", row.project, row.unread);
    }
    Ok(())
}

fn cmd_tasks(
    config_path: &Path,
    db_path: &Path,
    peer: Option<&str>,
    status: Option<&str>,
    limit: usize,
) -> Result<()> {
    MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let rows = board.list_tasks(peer, status, limit)?;
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}

fn cmd_ack(config_path: &Path, db_path: &Path, task_id: &str, note: Option<&str>) -> Result<()> {
    MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let updated = board.ack_task(task_id, note)?;
    if updated {
        let task = board
            .get_task(task_id)?
            .context("task vanished after ack")?;
        println!("{}", serde_json::to_string_pretty(&task)?);
    } else {
        anyhow::bail!("task '{task_id}' is not in a delivered/queued/claimed state to ack");
    }
    Ok(())
}

fn cmd_next(
    config_path: &Path,
    db_path: &Path,
    peer: Option<String>,
    project: Option<String>,
    lease_secs: i64,
) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let peer = default_identity(peer);
    let project = project.unwrap_or_else(|| current_project(&config));
    match board.claim_next_task(&peer, &project, lease_secs)? {
        Some(task) => println!("{}", serde_json::to_string_pretty(&task)?),
        None => println!("{{\"claimed\": null, \"peer\": \"{peer}\", \"project\": \"{project}\"}}"),
    }
    Ok(())
}

fn cmd_task_status(
    config_path: &Path,
    db_path: &Path,
    id: &str,
    status: &str,
    reason: Option<&str>,
) -> Result<()> {
    MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let updated = board.set_task_status(id, status, reason)?;
    if !updated {
        anyhow::bail!("task '{id}' not found");
    }
    let task = board.get_task(id)?.context("task vanished")?;
    println!("{}", serde_json::to_string_pretty(&task)?);
    Ok(())
}

fn cmd_budget(config_path: &Path, db_path: &Path, peer: Option<String>) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let peer = default_identity(peer);
    let limit = config.max_prompt_chars_for_peer(&peer);
    let (used_chars, calls) = board.total_used_chars_for_peer(&peer)?;
    let used_chars = used_chars.max(0) as usize;
    let remaining = limit.saturating_sub(used_chars);
    let summary = json!({
        "peer": peer,
        "limit_chars": limit,
        "est_limit_tokens": limit / 4,
        "used_chars": used_chars,
        "used_est_tokens": used_chars / 4,
        "remaining_chars": remaining,
        "remaining_est_tokens": remaining / 4,
        "calls_recorded": calls,
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn cmd_connectors(config_path: &Path, db_path: &Path, audit: bool) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let project = current_project(&config);
    if audit {
        let rows = board.list_connector_calls(20)?;
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let registry = octo_core::ConnectorsConfig::load_global()?;
    let rows = connector::status_rows(&registry, &config, &project, &board);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"project": project, "connectors": rows}))?
    );
    Ok(())
}

fn cmd_connector(config_path: &Path, db_path: &Path, command: ConnectorCommand) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let project = current_project(&config);
    match command {
        ConnectorCommand::Call {
            name,
            request,
            payload_file,
            timeout,
        } => {
            let request_json = match (request, payload_file) {
                (Some(r), _) => r,
                (None, Some(f)) => fs::read_to_string(&f)
                    .with_context(|| format!("failed to read {}", f.display()))?,
                (None, None) => anyhow::bail!("provide the JSON-RPC request or --payload-file"),
            };
            let registry = octo_core::ConnectorsConfig::load_global()?;
            let method = serde_json::from_str::<Value>(&request_json)
                .ok()
                .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string());
            let profile = match connector::check_connector_permission(
                &registry, &config, &project, &name,
            ) {
                Ok(p) => p,
                Err(e) => {
                    let _ = board.record_connector_call(
                        &name,
                        &project,
                        &method,
                        false,
                        Some(&e.message()),
                    );
                    println!(
                        "{}",
                        json!({"ok": false, "error": {"code": e.code(), "message": e.message()}})
                    );
                    std::process::exit(2);
                }
            };
            let timeout_secs = timeout.unwrap_or(connector::DEFAULT_CONNECTOR_TIMEOUT_SECS);
            match connector::call_connector(profile, &request_json, timeout_secs) {
                Ok(resp) => {
                    let ok = serde_json::from_str::<Value>(&resp)
                        .map(|v| v.get("error").is_none())
                        .unwrap_or(true);
                    let _ = board.record_connector_call(&name, &project, &method, ok, None);
                    println!("{resp}");
                }
                Err(e) => {
                    let _ = board.record_connector_call(&name, &project, &method, false, Some(&e));
                    println!(
                        "{}",
                        json!({"ok": false, "error": {"code": "connector_error", "message": e}})
                    );
                    std::process::exit(3);
                }
            }
            Ok(())
        }
    }
}

fn cmd_share(
    config_path: &Path,
    db_path: &Path,
    file: &Path,
    via: &str,
    to: &str,
    message: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let cwd = std::env::current_dir()?;
    let project = current_project(&config);
    let author = default_identity(None);

    // Sandbox: the file must resolve strictly inside the current directory.
    let rel = file.to_string_lossy().to_string();
    if file.is_absolute() || rel.split(['/', '\\']).any(|p| p == "..") {
        anyhow::bail!("path_outside_workspace: file must stay inside the current directory");
    }
    let resolved = cwd
        .join(file)
        .canonicalize()
        .unwrap_or_else(|_| cwd.join(file));
    if !resolved.starts_with(&cwd) || !resolved.is_file() {
        anyhow::bail!("file_not_found: {}", file.display());
    }
    let data = fs::read(&resolved)?;
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(&data);
    let sha256 = format!("{:x}", hasher.finalize());
    let meta = json!({
        "name": resolved.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "size_bytes": data.len(),
        "sha256": sha256,
    });
    let tool_name = if via == "github" {
        "create_file"
    } else {
        "upload_file"
    };
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": tool_name, "arguments": {
            "file_name": meta["name"], "local_path": resolved.display().to_string(),
            "size_bytes": meta["size_bytes"], "sha256": meta["sha256"],
        }},
    });
    let preview_body = |link: &str| {
        let prefix = message
            .map(|m| format!("{}\n", m.trim()))
            .unwrap_or_default();
        format!(
            "{prefix}Shared via {via}: {link}\nfile={} size={}B sha256={}",
            meta["name"], meta["size_bytes"], meta["sha256"]
        )
    };

    if dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dry_run": true, "project": project, "to": to, "via": via,
                "connector_payload": payload, "mailbox_preview": preview_body(&format!("<pending:{via}:{}>", meta["name"])),
            }))?
        );
        return Ok(());
    }

    let registry = octo_core::ConnectorsConfig::load_global()?;
    let profile = connector::check_connector_permission(&registry, &config, &project, via)
        .map_err(|e| anyhow::anyhow!("{}: {}", e.code(), e.message()))?;
    let resp = connector::call_connector(
        profile,
        &payload.to_string(),
        connector::DEFAULT_CONNECTOR_TIMEOUT_SECS,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    let link = serde_json::from_str::<Value>(&resp)
        .ok()
        .and_then(|v| {
            v.pointer("/result/content/0/text")
                .and_then(|t| t.as_str())
                .map(str::to_string)
                .or_else(|| {
                    ["url", "link", "id"]
                        .iter()
                        .find_map(|k| v.get(k).and_then(|x| x.as_str()).map(str::to_string))
                })
                .or_else(|| Some(resp.clone()))
        })
        .unwrap_or(resp);
    let body = preview_body(&link);
    let recipients = expand_recipients(&config, to)?;
    let ids = board.send_messages_project(&author, &recipients, &body, None, &project)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true, "project": project, "to": to, "via": via, "link": link,
            "message_ids": ids, "file": meta,
        }))?
    );
    Ok(())
}

fn cmd_status(config_path: &Path, db_path: &Path) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let peer_names: Vec<String> = config.peers.keys().cloned().collect();
    board.ensure_peers(&peer_names)?;

    let project = current_project(&config);
    let caller = default_identity(None);
    println!("=== Octomesh Status ===");
    println!("Config: {}", config_path.display());
    println!("State DB: {}", db_path.display());
    println!("Project: {project}");
    println!("Max Delegation Depth: {}", config.max_depth);
    println!(
        "Unread for '{caller}' in '{project}': {}",
        board.unread_count_project(&caller, &project)?
    );
    println!("Open tasks: {}", board.count_open_tasks()?);
    println!("\n[Roles]");
    let mut roles: Vec<_> = config.roles.iter().collect();
    roles.sort_by_key(|(k, _)| *k);
    for (role, chain) in roles {
        println!("  {:<14} -> {}", role, chain.join(" -> "));
    }

    println!("\n[Peers]");
    for state in board.list_peer_states()? {
        let adapter = config
            .peers
            .get(&state.peer_name)
            .map(|p| p.adapter.as_str())
            .unwrap_or("unknown");
        let cd_display = match state.cooldown_until {
            Some(dt) => format!("until {}", dt.to_rfc3339()),
            None => "-".to_string(),
        };
        println!(
            "  {:<16} adapter={:<10} health={:<12} cooldown={}",
            state.peer_name,
            adapter,
            state.health.as_str(),
            cd_display
        );
    }

    println!("\n[Presence]");
    let ttl = config.mesh.presence_ttl_secs.max(1) as i64;
    let now = chrono::Utc::now();
    let mut any = false;
    for row in board.list_presence()? {
        let age = now.signed_duration_since(row.last_seen).num_seconds();
        if age > ttl {
            continue;
        }
        any = true;
        println!(
            "  {:<16} LIVE pid={} host={} tty={}",
            row.peer,
            row.pid,
            row.host,
            row.tty.as_deref().unwrap_or("-")
        );
    }
    if !any {
        println!("  (no live peers)");
    }

    let handoffs = board.recent_handoffs(5)?;
    if !handoffs.is_empty() {
        println!("\n[Recent Handoffs]");
        for h in handoffs {
            println!("  #{} [{}] {}: {}", h.id, h.author, h.task_title, h.summary);
        }
    }

    Ok(())
}

async fn cmd_ask(config_path: &Path, db_path: &Path, req: RouteRequest) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let cwd = std::env::current_dir()?;
    let (min_depth, ancestors) = env_router_context();
    let router = MeshRouter::new(config, board, cwd).with_env_context(min_depth, ancestors);

    let res = router.route(req).await?;
    println!(
        "[octo] resolved_peer={} adapter={} task_id={}\n",
        res.resolved_peer, res.adapter, res.task_id
    );
    println!("{}", res.output);
    Ok(())
}

fn default_identity(arg: Option<String>) -> String {
    arg.or_else(|| std::env::var("OCTOMESH_DEFAULT_CALLER").ok())
        .unwrap_or_else(|| "human-cli".to_string())
}

fn expand_recipients(config: &MeshConfig, target: &str) -> Result<Vec<String>> {
    if target == "*" {
        return Ok(vec!["*".to_string()]);
    }
    config
        .resolve_candidates(target)
        .ok_or_else(|| anyhow::anyhow!("Unknown target '{target}' (not a peer, role, or '*')"))
}

fn cmd_send(
    config_path: &Path,
    db_path: &Path,
    target: &str,
    message: &str,
    author: Option<String>,
    ref_task: Option<String>,
    project_override: Option<String>,
) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let author = default_identity(author);
    let project = project_override.unwrap_or_else(|| current_project(&config));
    let recipients = expand_recipients(&config, target)?;
    let ids = board.send_messages_project(
        &author,
        &recipients,
        message,
        ref_task.as_deref(),
        &project,
    )?;
    let ids_str = ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "[octo] sent message from '{author}' to '{target}' in project '{project}' (ids: {ids_str})"
    );
    Ok(())
}

fn cmd_inbox(
    config_path: &Path,
    db_path: &Path,
    peer: Option<String>,
    all: bool,
    mark_read: bool,
    project_override: Option<String>,
    all_projects: bool,
    ack: &[i64],
) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let peer = default_identity(peer);
    let project = project_override.unwrap_or_else(|| current_project(&config));
    if !ack.is_empty() {
        let acked = board.ack_messages(&peer, ack)?;
        println!("[octo] acked {acked} message(s) for '{peer}'");
    }
    let page = board.read_inbox_project(&peer, &project, all_projects, !all, 50, mark_read)?;
    if mark_read {
        let marked = page.marked_read;
        println!("[octo] marked {marked} message(s) as read for '{peer}'");
    }
    let scope = if all_projects {
        "all projects".to_string()
    } else {
        format!("project '{project}'")
    };
    println!(
        "=== Inbox for '{peer}' ({scope}, unread: {}) ===",
        page.unread
    );
    if page.messages.is_empty() {
        println!("  (empty)");
    }
    for m in page.messages {
        let ref_part = m
            .ref_task
            .as_deref()
            .map(|t| format!(" [ref_task={t}]"))
            .unwrap_or_default();
        let read_part = if m.read_at.is_some() { " (read)" } else { "" };
        let ack_part = if m.ack_at.is_some() { " (ack)" } else { "" };
        println!(
            "#{} from={} to={} project={}{}{}{} at={}\n  {}",
            m.id,
            m.sender,
            m.recipient,
            m.project,
            ref_part,
            read_part,
            ack_part,
            m.created_at.to_rfc3339(),
            m.body
        );
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

async fn cmd_mcp_serve(config_path: &Path, db_path: &Path) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let cwd = std::env::current_dir()?;
    let workspace = active_workspace(&cwd);
    let (min_depth, ancestors) = env_router_context();
    let router = MeshRouter::new(config, board, cwd).with_env_context(min_depth, ancestors);

    info!(
        config = %config_path.display(),
        db = %db_path.display(),
        "Starting octo MCP server on stdio"
    );

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = io::stdout();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to parse JSON-RPC frame: {e}");
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {e}") }
                });
                write_json_line(&mut stdout, &err_resp).await?;
                continue;
            }
        };

        // Notifications (no `id`) do not receive a response in JSON-RPC 2.0
        let Some(req_id) = req.id.clone() else {
            continue;
        };

        let response = handle_mcp_request(
            &router,
            workspace.as_ref(),
            req_id,
            &req.method,
            &req.params,
        )
        .await;
        write_json_line(&mut stdout, &response).await?;
    }

    Ok(())
}

async fn write_json_line(stdout: &mut io::Stdout, payload: &Value) -> Result<()> {
    let serialized = serde_json::to_string(payload)?;
    stdout.write_all(serialized.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

async fn handle_mcp_request(
    router: &MeshRouter,
    workspace: Option<&Workspace>,
    id: Value,
    method: &str,
    params: &Value,
) -> Value {
    match method {
        "initialize" => {
            let negotiated = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("2024-11-05");
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": negotiated,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "octomesh-cli",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            })
        }
        "ping" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        }),
        "resources/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "resources": [] }
        }),
        "prompts/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "prompts": [] }
        }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [
                    {
                        "name": "mesh_workspace",
                        "description": "Locate the current project's local shared files directory.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {},
                            "additionalProperties": false
                        }
                    },
                    {
                        "name": "mesh_status",
                        "description": "List available peers (claude, codex, vibe, agy), failover role chains, cooldown states, and recent handoffs from the SQLite blackboard.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {},
                            "additionalProperties": false
                        }
                    },
                    {
                        "name": "mesh_ask",
                        "description": "Delegate a coding task, question, or read-only review to another peer (claude, codex, vibe, agy) or role chain (coder, reviewer, fast, demo) with automatic rate-limit/timeout failover.",
                        "inputSchema": {
                            "type": "object",
                            "required": ["target", "prompt", "caller"],
                            "properties": {
                                "target": { "type": "string", "description": "Configured peer name (including D-Bus window peers) or role ('coder', 'reviewer', 'fast', 'demo')" },
                                "prompt": { "type": "string", "description": "Task instructions or review request" },
                                "caller": { "type": "string", "description": "Your own peer/agent identifier to prevent self-invocation loops" },
                                "depth": { "type": "integer", "default": 1, "description": "Current delegation hop depth (increment by 1 when forwarding)" },
                                "include_diff": { "type": "boolean", "default": false, "description": "Include git status and truncated git diff in context" },
                                "readonly": { "type": "boolean", "default": false, "description": "Enforce read-only review mode without file edits" }
                            }
                        }
                    },
                    {
                        "name": "mesh_handoff",
                        "description": "Record a structured task checkpoint/summary to the SQLite blackboard so another terminal or agent can resume seamlessly.",
                        "inputSchema": {
                            "type": "object",
                            "required": ["author", "task_title", "summary", "next_steps"],
                            "properties": {
                                "author": { "type": "string", "description": "Peer/agent recording the handoff" },
                                "task_title": { "type": "string", "description": "Short title of the active workstream" },
                                "summary": { "type": "string", "description": "Completed work and current state" },
                                "next_steps": { "type": "string", "description": "Concrete remaining actions for the next peer" }
                            }
                        }
                    },
                    {
                        "name": "mesh_send",
                        "description": "Send an async structured message to a peer, role members, or '*' (broadcast) on the SQLite blackboard mailbox.",
                        "inputSchema": {
                            "type": "object",
                            "required": ["target", "message"],
                            "properties": {
                                "target": { "type": "string", "description": "Peer name, role (delivered to each member), or '*' (broadcast)" },
                                "message": { "type": "string", "description": "Short structured message body" },
                                "author": { "type": "string", "description": "Your peer/agent identifier" },
                                "ref_task": { "type": "string", "description": "Optional task_id from mesh_ask this message refers to" }
                            }
                        }
                    },
                    {
                        "name": "mesh_inbox",
                        "description": "List your direct messages and broadcasts (scoped to the current project). Optionally mark this page as read and/or acknowledge message ids.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "peer": { "type": "string", "description": "Your peer/agent identifier (defaults to $OCTOMESH_DEFAULT_CALLER or human-cli)" },
                                "unread_only": { "type": "boolean", "default": true, "description": "Only unread messages" },
                                "mark_read": { "type": "boolean", "default": false, "description": "Mark listed messages as read" },
                                "ack": { "type": "array", "items": { "type": "integer" }, "description": "Message ids to acknowledge (sets ack_at)" },
                                "all_projects": { "type": "boolean", "default": false, "description": "Include messages from all projects" },
                                "limit": { "type": "integer", "minimum": 0, "maximum": 100, "default": 20, "description": "Max messages to return" }
                            }
                        }
                    },
                    {
                        "name": "mesh_tasks",
                        "description": "List tracked tasks (queued/delivered/accepted/claimed/done/failed), optionally filtered by peer and status.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "peer": { "type": "string", "description": "Filter to tasks from/to this peer" },
                                "status": { "type": "string", "description": "Filter by status (queued, delivered, accepted, claimed, done, failed)" },
                                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
                            }
                        }
                    },
                    {
                        "name": "mesh_ack",
                        "description": "Acknowledge a delivered/queued task: status becomes accepted (receiver side of a window delegation).",
                        "inputSchema": {
                            "type": "object",
                            "required": ["task_id"],
                            "properties": {
                                "task_id": { "type": "string" },
                                "note": { "type": "string" }
                            }
                        }
                    },
                    {
                        "name": "mesh_next",
                        "description": "Atomically claim the oldest queued task for a peer in the current project (lease-based; stale claims requeue).",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "peer": { "type": "string", "description": "Claiming peer (defaults to $OCTOMESH_DEFAULT_CALLER)" },
                                "project": { "type": "string", "description": "Project scope (defaults to the resolved project id)" },
                                "lease_secs": { "type": "integer", "default": 300 }
                            }
                        }
                    },
                    {
                        "name": "mesh_task_done",
                        "description": "Mark a task done.",
                        "inputSchema": {
                            "type": "object",
                            "required": ["task_id"],
                            "properties": { "task_id": { "type": "string" } }
                        }
                    },
                    {
                        "name": "mesh_task_fail",
                        "description": "Mark a task failed with an optional reason.",
                        "inputSchema": {
                            "type": "object",
                            "required": ["task_id"],
                            "properties": {
                                "task_id": { "type": "string" },
                                "reason": { "type": "string" }
                            }
                        }
                    },
                    {
                        "name": "mesh_connectors",
                        "description": "List connectors from the global registry with per-project permission, binary and env-key presence (names only, never values).",
                        "inputSchema": {
                            "type": "object",
                            "properties": {},
                            "additionalProperties": false
                        }
                    },
                    {
                        "name": "mesh_connector_call",
                        "description": "Proxy a JSON-RPC request to an allowed connector's stdio MCP server; every call is audited.",
                        "inputSchema": {
                            "type": "object",
                            "required": ["connector", "request"],
                            "properties": {
                                "connector": { "type": "string" },
                                "request": { "type": "object", "description": "JSON-RPC 2.0 request object" },
                                "timeout_secs": { "type": "integer", "default": 30 }
                            }
                        }
                    }
                ]
            }
        }),
        "tools/call" => handle_tool_call(router, workspace, id, params).await,
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {method}")
            }
        }),
    }
}

async fn handle_tool_call(
    router: &MeshRouter,
    workspace: Option<&Workspace>,
    id: Value,
    params: &Value,
) -> Value {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match tool_name {
        "mesh_workspace" => {
            if let Some(workspace) = workspace {
                mcp_text_result(
                    id,
                    json!({
                        "project_root": workspace.project_root,
                        "files": workspace.files,
                        "inbox": workspace.inbox,
                    })
                    .to_string(),
                    false,
                )
            } else {
                mcp_text_result(id, "Run octo workspace init in this project".into(), true)
            }
        }
        "mesh_status" => {
            let board = router.blackboard();
            let peers = board.list_peer_states().unwrap_or_default();
            let handoffs = board.recent_handoffs(5).unwrap_or_default();
            let presence = board.list_presence().unwrap_or_default();
            let open_tasks = board.count_open_tasks().unwrap_or(0);
            let caller = default_identity(None);
            let project = router.project().to_string();
            let projects = board.list_projects_with_unread(&caller).unwrap_or_default();
            let (used_chars, calls) = board.total_used_chars_for_peer(&caller).unwrap_or((0, 0));
            let limit = router.config().max_prompt_chars_for_peer(&caller);
            let payload = json!({
                "max_depth": router.config().max_depth,
                "project": project,
                "roles": router.config().roles,
                "peers": peers,
                "presence": presence,
                "open_tasks": open_tasks,
                "projects": projects,
                "budget": {
                    "peer": caller,
                    "limit_chars": limit,
                    "used_chars": used_chars,
                    "remaining_chars": (limit as i64 - used_chars).max(0),
                    "calls_recorded": calls,
                },
                "recent_handoffs": handoffs,
            });
            mcp_text_result(
                id,
                serde_json::to_string_pretty(&payload).unwrap_or_default(),
                false,
            )
        }
        "mesh_ask" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let prompt = args
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let default_caller =
                std::env::var("OCTOMESH_DEFAULT_CALLER").unwrap_or_else(|_| "unknown".to_string());
            let raw_caller = args
                .get("caller")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            let caller = if raw_caller.is_empty() || raw_caller == "unknown" {
                default_caller
            } else {
                raw_caller.to_string()
            };
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let include_diff = args
                .get("include_diff")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let readonly = args
                .get("readonly")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            match router
                .route(RouteRequest {
                    target,
                    prompt,
                    caller,
                    depth,
                    include_diff,
                    readonly,
                })
                .await
            {
                Ok(res) => {
                    let body = serde_json::to_string_pretty(&res).unwrap_or(res.output);
                    mcp_text_result(id, body, false)
                }
                Err(e) => mcp_text_result(id, format!("mesh_ask failed: {e}"), true),
            }
        }
        "mesh_handoff" => {
            let default_author =
                std::env::var("OCTOMESH_DEFAULT_CALLER").unwrap_or_else(|_| "unknown".to_string());
            let raw_author = args
                .get("author")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            let author = if raw_author.is_empty() || raw_author == "unknown" {
                default_author.as_str()
            } else {
                raw_author
            };
            let task_title = args
                .get("task_title")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled");
            let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            let next_steps = args
                .get("next_steps")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match router
                .blackboard()
                .record_handoff(author, task_title, summary, next_steps)
            {
                Ok(rec) => {
                    let body = serde_json::to_string_pretty(&rec).unwrap_or_default();
                    mcp_text_result(id, body, false)
                }
                Err(e) => mcp_text_result(id, format!("mesh_handoff failed: {e}"), true),
            }
        }
        "mesh_send" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let message = args
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let raw_author = args
                .get("author")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            let author = default_identity(
                (!raw_author.is_empty() && raw_author != "unknown").then(|| raw_author.to_owned()),
            );
            let ref_task = args
                .get("ref_task")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let recipients = match expand_recipients(router.config(), &target) {
                Ok(r) => r,
                Err(e) => return mcp_text_result(id, format!("mesh_send failed: {e}"), true),
            };
            let project = router.project().to_string();
            match router.blackboard().send_messages_project(
                &author,
                &recipients,
                &message,
                ref_task.as_deref(),
                &project,
            ) {
                Ok(ids) => mcp_text_result(
                    id,
                    json!({ "ids": ids, "project": project }).to_string(),
                    false,
                ),
                Err(e) => mcp_text_result(id, format!("mesh_send failed: {e}"), true),
            }
        }
        "mesh_inbox" => {
            let raw_peer = args
                .get("peer")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            let peer = default_identity(
                (!raw_peer.is_empty() && raw_peer != "unknown").then(|| raw_peer.to_owned()),
            );
            let unread_only = args
                .get("unread_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let mark_read = args
                .get("mark_read")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(100) as usize;
            let all_projects = args
                .get("all_projects")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let ack_ids: Vec<i64> = args
                .get("ack")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect::<Vec<_>>())
                .unwrap_or_default();
            let project = router.project().to_string();
            let board = router.blackboard();
            let acked = if ack_ids.is_empty() {
                0
            } else {
                board.ack_messages(&peer, &ack_ids).unwrap_or(0)
            };
            match board.read_inbox_project(
                &peer,
                &project,
                all_projects,
                unread_only,
                limit,
                mark_read,
            ) {
                Ok(page) => mcp_text_result(
                    id,
                    json!({
                        "peer": peer,
                        "project": project,
                        "all_projects": all_projects,
                        "acked": acked,
                        "unread": page.unread,
                        "marked_read": page.marked_read,
                        "messages": page.messages,
                    })
                    .to_string(),
                    false,
                ),
                Err(e) => mcp_text_result(id, format!("mesh_inbox failed: {e}"), true),
            }
        }
        "mesh_tasks" => {
            let peer = args.get("peer").and_then(|v| v.as_str());
            let status = args.get("status").and_then(|v| v.as_str());
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(50)
                .clamp(1, 200) as usize;
            match router.blackboard().list_tasks(peer, status, limit) {
                Ok(rows) => mcp_text_result(
                    id,
                    serde_json::to_string_pretty(&rows).unwrap_or_default(),
                    false,
                ),
                Err(e) => mcp_text_result(id, format!("mesh_tasks failed: {e}"), true),
            }
        }
        "mesh_ack" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
            let note = args.get("note").and_then(|v| v.as_str());
            if task_id.is_empty() {
                return mcp_text_result(id, "mesh_ack failed: task_id required".into(), true);
            }
            match router.blackboard().ack_task(task_id, note) {
                Ok(true) => {
                    let task = router.blackboard().get_task(task_id).ok().flatten();
                    mcp_text_result(
                        id,
                        serde_json::to_string_pretty(&json!({"acked": true, "task": task}))
                            .unwrap_or_default(),
                        false,
                    )
                }
                Ok(false) => mcp_text_result(
                    id,
                    format!(
                        "mesh_ack: task '{task_id}' is not in a delivered/queued/claimed state"
                    ),
                    true,
                ),
                Err(e) => mcp_text_result(id, format!("mesh_ack failed: {e}"), true),
            }
        }
        "mesh_next" => {
            let peer = default_identity(
                args.get("peer")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string),
            );
            let project = args
                .get("project")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| router.project().to_string());
            let lease_secs = args
                .get("lease_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(300);
            match router
                .blackboard()
                .claim_next_task(&peer, &project, lease_secs)
            {
                Ok(task) => mcp_text_result(
                    id,
                    serde_json::to_string_pretty(
                        &json!({"claimed": task, "peer": peer, "project": project}),
                    )
                    .unwrap_or_default(),
                    false,
                ),
                Err(e) => mcp_text_result(id, format!("mesh_next failed: {e}"), true),
            }
        }
        "mesh_task_done" | "mesh_task_fail" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
            let status = if tool_name == "mesh_task_done" {
                "done"
            } else {
                "failed"
            };
            let reason = args.get("reason").and_then(|v| v.as_str());
            match router.blackboard().set_task_status(task_id, status, reason) {
                Ok(true) => mcp_text_result(
                    id,
                    json!({"updated": true, "task_id": task_id, "status": status}).to_string(),
                    false,
                ),
                Ok(false) => mcp_text_result(id, format!("task '{task_id}' not found"), true),
                Err(e) => mcp_text_result(id, format!("{tool_name} failed: {e}"), true),
            }
        }
        "mesh_connectors" => {
            let registry = octo_core::ConnectorsConfig::load_global().unwrap_or_default();
            let rows = connector::status_rows(
                &registry,
                router.config(),
                router.project(),
                router.blackboard(),
            );
            mcp_text_result(
                id,
                serde_json::to_string_pretty(
                    &json!({"project": router.project(), "connectors": rows}),
                )
                .unwrap_or_default(),
                false,
            )
        }
        "mesh_connector_call" => {
            let name = args
                .get("connector")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let request = args.get("request").cloned().unwrap_or_else(|| json!({}));
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(30);
            let project = router.project().to_string();
            let registry = match octo_core::ConnectorsConfig::load_global() {
                Ok(r) => r,
                Err(e) => {
                    return mcp_text_result(id, format!("mesh_connector_call failed: {e}"), true)
                }
            };
            let profile = match connector::check_connector_permission(
                &registry,
                router.config(),
                &project,
                &name,
            ) {
                Ok(p) => p,
                Err(e) => {
                    let _ = router.blackboard().record_connector_call(
                        &name,
                        &project,
                        "tools/call",
                        false,
                        Some(&e.message()),
                    );
                    return mcp_text_result(id, format!("{}: {}", e.code(), e.message()), true);
                }
            };
            match connector::call_connector(profile, &request.to_string(), timeout_secs) {
                Ok(resp) => {
                    let ok = serde_json::from_str::<Value>(&resp)
                        .map(|v| v.get("error").is_none())
                        .unwrap_or(true);
                    let _ = router.blackboard().record_connector_call(
                        &name,
                        &project,
                        "tools/call",
                        ok,
                        None,
                    );
                    mcp_text_result(id, resp, false)
                }
                Err(e) => {
                    let _ = router.blackboard().record_connector_call(
                        &name,
                        &project,
                        "tools/call",
                        false,
                        Some(&e),
                    );
                    mcp_text_result(id, format!("connector_error: {e}"), true)
                }
            }
        }
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32602,
                "message": format!("Unknown tool: {tool_name}")
            }
        }),
    }
}

fn mcp_text_result(id: Value, text: String, is_error: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [
                {
                    "type": "text",
                    "text": text
                }
            ],
            "isError": is_error
        }
    })
}
