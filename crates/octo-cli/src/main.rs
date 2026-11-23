use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use octo_core::{Blackboard, MeshConfig, MeshRouter, RouteRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{error, info};

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

    /// Display current peer health, cooldown timers, roles, and recent handoffs
    Status,

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

    /// Run a local Model Context Protocol (MCP) JSON-RPC 2.0 server over stdio
    McpServe,

    /// Manage the project's local shared files and cloud export folder
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
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

    match cli.command {
        Commands::Init { sync_agents, force } => {
            let cfg_path = cli.config.unwrap_or_else(|| PathBuf::from("octomesh.toml"));
            cmd_init(&cfg_path, sync_agents, force)
        }
        Commands::Status => {
            let (cfg_path, db_path) = resolve_paths(cli.config, cli.db);
            cmd_status(&cfg_path, &db_path)
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

fn cmd_status(config_path: &Path, db_path: &Path) -> Result<()> {
    let config = MeshConfig::load_from_path(config_path)?;
    let board = Blackboard::open(db_path)?;
    let peer_names: Vec<String> = config.peers.keys().cloned().collect();
    board.ensure_peers(&peer_names)?;

    println!("=== Octomesh Status ===");
    println!("Config: {}", config_path.display());
    println!("State DB: {}", db_path.display());
    println!("Max Delegation Depth: {}", config.max_depth);
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
                                "target": { "type": "string", "description": "Peer name ('claude', 'codex', 'vibe', 'agy') or role ('coder', 'reviewer', 'fast', 'demo')" },
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
                    })
                    .to_string(),
                    false,
                )
            } else {
                mcp_text_result(id, "Run octo workspace init in this project".into(), true)
            }
        }
        "mesh_status" => {
            let peers = router.blackboard().list_peer_states().unwrap_or_default();
            let handoffs = router.blackboard().recent_handoffs(5).unwrap_or_default();
            let payload = json!({
                "max_depth": router.config().max_depth,
                "roles": router.config().roles,
                "peers": peers,
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
