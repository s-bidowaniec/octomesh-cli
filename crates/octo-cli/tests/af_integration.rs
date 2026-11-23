use octo_core::{Blackboard, MeshConfig, MeshRouter, PeerConfig, RouteRequest};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use uuid::Uuid;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn test_config(max_prompt_chars: Option<usize>) -> MeshConfig {
    let mut peers = HashMap::new();
    let mut env = HashMap::new();
    env.insert(
        "PYTHONPATH".to_string(),
        repo_root().join("adapters").display().to_string(),
    );
    peers.insert(
        "mock_primary".to_string(),
        PeerConfig {
            adapter: "mock".to_string(),
            command: None,
            args: vec![],
            timeout_secs: 10,
            cooldown_secs: Some(60),
            env: env.clone(),
            max_prompt_chars,
            window_bus: None,
            window_path: None,
        },
    );
    peers.insert(
        "mock_backup".to_string(),
        PeerConfig {
            adapter: "mock".to_string(),
            command: None,
            args: vec![],
            timeout_secs: 10,
            cooldown_secs: Some(60),
            env,
            max_prompt_chars: None,
            window_bus: None,
            window_path: None,
        },
    );
    let mut roles = HashMap::new();
    roles.insert(
        "demo".to_string(),
        vec!["mock_primary".to_string(), "mock_backup".to_string()],
    );
    MeshConfig {
        adapter_runner: vec![
            "python3".to_string(),
            "-m".to_string(),
            "octomesh_adapters.runner".to_string(),
        ],
        roles,
        peers,
        ..MeshConfig::default()
    }
}

#[tokio::test]
async fn ask_creates_tracked_task_and_records_budget_usage() {
    let board = Blackboard::open_in_memory().unwrap();
    let router = MeshRouter::new(test_config(None), board.clone(), "/tmp");
    let res = router
        .route(RouteRequest {
            target: "demo".into(),
            prompt: "short prompt".into(),
            caller: "vibe".into(),
            depth: 1,
            include_diff: false,
            readonly: false,
        })
        .await
        .unwrap();
    assert_eq!(res.resolved_peer, "mock_primary");

    let tasks = board.list_tasks(Some("vibe"), None, 10).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, "done"); // mock adapter is synchronous -> done
    assert_eq!(tasks[0].kind, "ask");
    assert_eq!(tasks[0].project, "default");
    let (chars, calls) = board.total_used_chars_for_peer("mock_primary").unwrap();
    assert_eq!(chars, "short prompt".chars().count() as i64);
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn oversized_prompt_fails_fast_with_budget_exceeded() {
    let board = Blackboard::open_in_memory().unwrap();
    let router = MeshRouter::new(test_config(Some(100)), board.clone(), "/tmp");
    let big = "x".repeat(500);
    let err = router
        .route(RouteRequest {
            target: "mock_primary".into(),
            prompt: big.clone(),
            caller: "vibe".into(),
            depth: 1,
            include_diff: false,
            readonly: false,
        })
        .await
        .unwrap_err();
    match err {
        octo_core::MeshError::BudgetExceeded { peer, chars, limit } => {
            assert_eq!(peer, "mock_primary");
            assert_eq!(chars, 500);
            assert_eq!(limit, 100);
        }
        other => panic!("expected BudgetExceeded, got {other:?}"),
    }
    // task marked failed with detail, no adapter spawn -> no usage, no cooldown
    let tasks = board.list_tasks(None, Some("failed"), 10).unwrap();
    assert_eq!(tasks.len(), 1);
    assert!(tasks[0]
        .detail
        .as_ref()
        .unwrap()
        .contains("budget_exceeded"));
    let (_, calls) = board.total_used_chars_for_peer("mock_primary").unwrap();
    assert_eq!(calls, 0);
    assert!(board.is_peer_available("mock_primary").unwrap());
}

struct CliProject(PathBuf);

impl CliProject {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("octo-af-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let cfg = format!(
            r#"adapter_runner = ["python3", "-m", "octomesh_adapters.runner"]
max_depth = 2
default_cooldown_secs = 60

[mesh]
project = "proj_a"

[project]
name = "proj_a"
allowed_connectors = ["github"]

[peers.mock_primary]
adapter = "mock"
timeout_secs = 10
env = {{ PYTHONPATH = "{}" }}

[peers.codex]
adapter = "mock"
timeout_secs = 10

[roles]
demo = ["mock_primary"]
"#,
            repo_root().join("adapters").display()
        );
        fs::write(dir.join("octomesh.toml"), cfg).unwrap();
        let xdg = dir.join("xdg/octomesh");
        fs::create_dir_all(&xdg).unwrap();
        fs::write(
            xdg.join("connectors.toml"),
            "[connectors.github]\ncommand = \"npx\"\n[connectors.drive]\ncommand = \"npx\"\n",
        )
        .unwrap();
        Self(dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_octo"))
            .current_dir(&self.0)
            .env_remove("OCTOMESH_CONFIG")
            .env_remove("OCTOMESH_DB")
            .env("OCTOMESH_DEFAULT_CALLER", "vibe")
            .env("XDG_CONFIG_HOME", self.0.join("xdg"))
            .args(args)
            .output()
            .unwrap()
    }

    fn run_ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "args {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

impl Drop for CliProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_mailbox_project_isolation_ack_and_tasks() {
    let proj = CliProject::new();
    proj.run_ok(&[
        "send",
        "codex",
        "hello proj_a",
        "--author",
        "vibe",
        "--project",
        "proj_a",
    ]);
    proj.run_ok(&[
        "send",
        "codex",
        "hello proj_b",
        "--author",
        "vibe",
        "--project",
        "proj_b",
    ]);

    let inbox_a = proj.run_ok(&["inbox", "--peer", "codex", "--project", "proj_a"]);
    assert!(inbox_a.contains("hello proj_a"));
    assert!(!inbox_a.contains("hello proj_b"));

    // ack the proj_a message
    let msg_id = inbox_a
        .lines()
        .find_map(|l| {
            l.strip_prefix('#')
                .and_then(|r| r.split_whitespace().next())
        })
        .unwrap()
        .to_string();
    let ack_out = proj.run_ok(&[
        "inbox",
        "--peer",
        "codex",
        "--project",
        "proj_a",
        "--ack",
        &msg_id,
    ]);
    assert!(ack_out.contains("acked 1"));
    let inbox_acked = proj.run_ok(&["inbox", "--peer", "codex", "--project", "proj_a", "--all"]);
    assert!(inbox_acked.contains("(ack)"));

    // ask creates a tracked task (mock peer -> done)
    proj.run_ok(&["ask", "demo", "hi", "--caller", "vibe"]);
    let tasks = proj.run_ok(&["tasks"]);
    assert!(tasks.contains("\"done\""));

    // presence auto-registered the caller
    let presence = proj.run_ok(&["presence"]);
    assert!(presence.contains("vibe"));

    // budget report works
    let budget = proj.run_ok(&["budget", "--peer", "vibe"]);
    assert!(budget.contains("limit_chars"));
}

#[test]
fn cli_connector_gateway_denies_not_allowed() {
    let proj = CliProject::new();
    // drive is not in allowed_connectors=[github] -> exit 2 + connector_not_allowed
    let out = proj.run(&[
        "connector",
        "call",
        "drive",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}",
    ]);
    assert_eq!(out.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("connector_not_allowed"), "stdout: {stdout}");

    // unknown connector also denied
    let out = proj.run(&[
        "connector",
        "call",
        "nosuch",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("unknown_connector"));

    // connectors list shows registry-less empty but works; audit works
    let audit = proj.run_ok(&["connectors", "--audit"]);
    assert!(audit.contains("drive")); // the denied call was audited
}

#[test]
fn cli_share_dry_run_touches_nothing_and_rejects_traversal() {
    let proj = CliProject::new();
    fs::write(proj.0.join("report.txt"), "hello").unwrap();
    let dry = proj.run_ok(&[
        "share",
        "report.txt",
        "--via",
        "drive",
        "--to",
        "codex",
        "--dry-run",
    ]);
    assert!(dry.contains("dry_run"));
    assert!(dry.contains("upload_file"));

    let out = proj.run(&[
        "share",
        "../../etc/passwd",
        "--via",
        "drive",
        "--to",
        "codex",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("path_outside_workspace"));
}

#[test]
fn cli_next_done_fail_lifecycle() {
    let proj = CliProject::new();
    // no queued tasks -> null claim, exit 0
    let out = proj.run_ok(&["next", "--peer", "codex"]);
    assert!(out.contains("\"claimed\": null"));
    // done/fail on unknown id -> error
    let out = proj.run(&["done", "no-such-task"]);
    assert!(!out.status.success());
}

fn _unused(_: &Path) {}
