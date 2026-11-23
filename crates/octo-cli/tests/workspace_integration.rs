use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use uuid::Uuid;

struct TestProject(PathBuf);

impl TestProject {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("octo-workspace-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        let project = Self(path);
        project.run(&project.0, &["init"]);
        project
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_octo"));
        command
            .current_dir(cwd)
            .env_remove("OCTOMESH_CONFIG")
            .env_remove("OCTOMESH_DB")
            .args(args);
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let result = self.command(cwd, args).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result
    }

    fn workspace(&self) -> PathBuf {
        self.0.join(".octomesh/workspace")
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_and_mcp_share_mailbox_and_keep_broadcasts_unread_for_other_peers() {
    let project = TestProject::new();
    project.run(
        &project.0,
        &["send", "*", "build ready", "--author", "vibe"],
    );
    let mut mcp = project
        .command(&project.0, &["mcp-serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        mcp.stdin.take().unwrap(),
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "mesh_inbox",
                "arguments": { "peer": "agy", "mark_read": true, "limit": 1 }
            }
        })
    )
    .unwrap();
    let response = mcp.wait_with_output().unwrap();
    assert!(response.status.success());
    let frame: Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(frame["result"]["isError"], false);
    let page: Value =
        serde_json::from_str(frame["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(page["messages"][0]["body"], "build ready");
    assert_eq!(page["unread"], 0);
    assert_eq!(page["marked_read"], 1);

    let agy = project.run(&project.0, &["inbox", "--peer", "agy"]);
    assert!(!String::from_utf8(agy.stdout)
        .unwrap()
        .contains("build ready"));
    let codex = project.run(&project.0, &["inbox", "--peer", "codex"]);
    assert!(String::from_utf8(codex.stdout)
        .unwrap()
        .contains("build ready"));
}

#[test]
fn workspace_is_shared_by_cli_and_mcp_in_project_subdirectory() {
    let project = TestProject::new();
    let subdir = project.0.join("docs");
    fs::create_dir(&subdir).unwrap();

    let init = project.run(&subdir, &["workspace", "init"]);
    let info = project.run(&project.0, &["workspace", "info"]);
    let configured_paths: Value = serde_json::from_slice(&info.stdout).unwrap();
    assert_eq!(init.stdout, info.stdout);
    assert_eq!(
        configured_paths["files"],
        project.workspace().join("files").to_str().unwrap()
    );

    let config = project.0.join("octomesh.toml");
    let db = project.0.join(".octomesh/state.db");
    let mut mcp = project
        .command(&subdir, &["mcp-serve"])
        .arg("--config")
        .arg(config)
        .arg("--db")
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        mcp.stdin.take().unwrap(),
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"mesh_workspace","arguments":{{}}}}}}"#
    )
    .unwrap();
    let response = mcp.wait_with_output().unwrap();
    assert!(
        response.status.success(),
        "{}",
        String::from_utf8_lossy(&response.stderr)
    );
    let frame: Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(frame["result"]["isError"], false);
    let payload: Value =
        serde_json::from_str(frame["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(payload["files"], configured_paths["files"]);
    assert!(payload.get("exports").is_none());
    assert!(!subdir.join(".octomesh").exists());

    let other_repo = subdir.join("child");
    fs::create_dir_all(other_repo.join(".git")).unwrap();
    let outside = project
        .command(&other_repo, &["workspace", "info"])
        .output()
        .unwrap();
    assert!(!outside.status.success());
}

#[test]
fn export_is_explicit_complete_and_refuses_concurrent_overwrites() {
    let project = TestProject::new();
    project.run(&project.0, &["workspace", "init"]);
    let files = project.workspace().join("files");
    let exports = project.workspace().join("exports");
    let artifact = "raport.bin";
    let data = vec![42; 512 * 1024];
    fs::write(files.join(artifact), &data).unwrap();
    assert!(!exports.join(artifact).exists());

    let publish = || {
        project
            .command(&project.0, &["workspace", "publish", artifact])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut a = publish();
    let mut b = publish();
    assert_ne!(a.wait().unwrap().success(), b.wait().unwrap().success());
    assert_eq!(fs::read(exports.join(artifact)).unwrap(), data);

    fs::write(files.join(artifact), b"changed").unwrap();
    let collision = project
        .command(&project.0, &["workspace", "publish", artifact])
        .output()
        .unwrap();
    assert!(!collision.status.success());
    assert_eq!(fs::read(exports.join(artifact)).unwrap(), data);
    assert_eq!(fs::read_dir(&exports).unwrap().count(), 1);
    assert_eq!(fs::read_dir(project.workspace()).unwrap().count(), 3);
}

#[cfg(unix)]
#[test]
fn publish_rejects_paths_outside_shared_files() {
    use std::os::unix::fs::symlink;

    let project = TestProject::new();
    project.run(&project.0, &["workspace", "init"]);
    let files = project.workspace().join("files");
    fs::write(files.join(".private"), b"synthetic").unwrap();
    symlink(project.0.join("octomesh.toml"), files.join("alias.toml")).unwrap();
    for path in ["../octomesh.toml", ".private", "alias.toml"] {
        let result = project
            .command(&project.0, &["workspace", "publish", path])
            .output()
            .unwrap();
        assert!(!result.status.success(), "published {path}");
    }
    assert_eq!(
        fs::read_dir(project.workspace().join("exports"))
            .unwrap()
            .count(),
        0
    );
}
