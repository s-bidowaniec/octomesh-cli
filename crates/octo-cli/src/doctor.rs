use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticCheck {
    pub name: String,
    pub ok: bool,
    pub warn: bool,
    pub detail: String,
}

impl DiagnosticCheck {
    pub fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: true,
            warn: false,
            detail: detail.into(),
        }
    }

    pub fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: true,
            warn: true,
            detail: detail.into(),
        }
    }

    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: false,
            warn: false,
            detail: detail.into(),
        }
    }

    pub fn format_line(&self) -> String {
        let tag = if !self.ok {
            "FAIL"
        } else if self.warn {
            "WARN"
        } else {
            "OK"
        };
        format!("{tag} {}: {}", self.name, self.detail)
    }
}

pub fn check_binary_on_path(bin_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(bin_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
    for entry in fs::read_dir(src).map_err(|e| format!("readdir {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("dir entry error: {e}"))?;
        let ty = entry
            .file_type()
            .map_err(|e| format!("file_type error: {e}"))?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).map_err(|e| {
                format!(
                    "copy {} -> {}: {e}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

fn repo_root_from_exe(exe: &Path) -> Option<PathBuf> {
    // <repo>/target/<profile>/octo -> <repo>
    let root = exe.parent()?.parent()?.parent()?;
    if root.join("adapters/run_adapter.py").is_file() {
        Some(root.to_path_buf())
    } else {
        None
    }
}

/// `octo install [--prefix DIR]`: binary to <prefix>/bin, adapters to
/// <prefix>/share/octomesh/adapters, global config with an ABSOLUTE runner.
/// Returns the shell snippet to print.
pub fn install_octomesh(prefix_dir: &Path, current_exe: Option<&Path>) -> Result<String, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let bin_dir = prefix_dir.join("bin");
    let share_adapters_dir = prefix_dir.join("share/octomesh/adapters");
    let global_cfg_dir = home.join(".config/octomesh");
    let global_cfg_path = global_cfg_dir.join("octomesh.toml");
    let global_db = home.join(".octomesh/state.db");

    fs::create_dir_all(&bin_dir).map_err(|e| format!("create bin dir failed: {e}"))?;
    fs::create_dir_all(&share_adapters_dir)
        .map_err(|e| format!("create adapters share dir failed: {e}"))?;
    fs::create_dir_all(&global_cfg_dir)
        .map_err(|e| format!("create global config dir failed: {e}"))?;

    if let Some(exe) = current_exe {
        if exe.exists() {
            let dest_bin = bin_dir.join("octo");
            fs::copy(exe, &dest_bin).map_err(|e| format!("copy binary failed: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&dest_bin, fs::Permissions::from_mode(0o755));
            }
        }
    }

    if let Some(exe) = current_exe {
        if let Some(root) = repo_root_from_exe(exe) {
            let src_adapters = root.join("adapters");
            if src_adapters.is_dir() {
                copy_dir_recursive(&src_adapters, &share_adapters_dir)?;
            }
        }
    }

    if !global_cfg_path.exists() {
        let runner_path = share_adapters_dir.join("run_adapter.py");
        let template = format!(
            "# Global octomesh configuration installed by `octo install`\n\
             adapter_runner = [\"python3\", \"{}\"]\n\
             max_depth = 2\n\
             default_cooldown_secs = 300\n\n\
             [mesh]\n\
             project = \"default\"\n\
             presence_ttl_secs = 120\n\
             busy_ttl_secs = 300\n\n\
             [project]\n\
             name = \"default\"\n\
             allowed_connectors = [\"github\", \"drive\"]\n\n\
             [budgets]\n\
             default_max_prompt_chars = 20000\n\n\
             [peers.mock_primary]\n\
             adapter = \"mock\"\n\
             timeout_secs = 5\n\n\
             [roles]\n\
             demo = [\"mock_primary\"]\n",
            runner_path.display()
        );
        fs::write(&global_cfg_path, template)
            .map_err(|e| format!("write global config failed: {e}"))?;
    }

    Ok(format!(
        "export PATH=\"{}:$PATH\"\nexport OCTOMESH_CONFIG=\"{}\"\nexport OCTOMESH_DB=\"{}\"",
        bin_dir.display(),
        global_cfg_path.display(),
        global_db.display()
    ))
}

/// Structured `octo doctor` checks. Returns (checks, all_ok); exit code is non-zero
/// when any check FAILs.
pub fn run_doctor_checks(
    config_path: Option<&Path>,
    db_path: &Path,
    config: Option<&octo_core::MeshConfig>,
) -> (Vec<DiagnosticCheck>, bool) {
    let mut checks = Vec::new();

    match check_binary_on_path("octo") {
        Some(p) => checks.push(DiagnosticCheck::ok(
            "binary",
            format!("{} (v{})", p.display(), env!("CARGO_PKG_VERSION")),
        )),
        None => checks.push(DiagnosticCheck::ok(
            "binary",
            format!("invoked directly (v{})", env!("CARGO_PKG_VERSION")),
        )),
    }

    match config_path {
        Some(cp) if cp.exists() => checks.push(DiagnosticCheck::ok(
            "config",
            format!("loaded {}", cp.display()),
        )),
        Some(cp) => checks.push(DiagnosticCheck::fail(
            "config",
            format!("config path {} does not exist", cp.display()),
        )),
        None => checks.push(DiagnosticCheck::ok("config", "using default/env config")),
    }

    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                checks.push(DiagnosticCheck::fail(
                    "blackboard",
                    format!("cannot create db parent {}: {e}", parent.display()),
                ));
            }
        }
    }
    match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(db_path)
    {
        Ok(_) => checks.push(DiagnosticCheck::ok(
            "blackboard",
            format!("writable at {}", db_path.display()),
        )),
        Err(e) => checks.push(DiagnosticCheck::fail(
            "blackboard",
            format!("open failed for {}: {e}", db_path.display()),
        )),
    }

    if let Some(cfg) = config {
        if cfg.adapter_runner.is_empty() {
            checks.push(DiagnosticCheck::fail(
                "adapter_runner",
                "runner command is empty",
            ));
        } else {
            let prog = &cfg.adapter_runner[0];
            let exists = Path::new(prog).is_file() || check_binary_on_path(prog).is_some();
            if exists {
                checks.push(DiagnosticCheck::ok(
                    "adapter_runner",
                    format!("available ({})", cfg.adapter_runner.join(" ")),
                ));
            } else {
                checks.push(DiagnosticCheck::fail(
                    "adapter_runner",
                    format!("runner not found: {prog} (run from the repo or `octo install`)"),
                ));
            }
        }

        for (peer_name, peer) in &cfg.peers {
            if peer.adapter == "dbus" {
                let bus = peer
                    .window_bus
                    .clone()
                    .or_else(|| parse_kv_arg(&peer.args, "bus"))
                    .unwrap_or_default();
                checks.push(probe_window_peer(peer_name, &bus));
            } else if let Some(cmd) = &peer.command {
                if Path::new(cmd).is_file() || check_binary_on_path(cmd).is_some() {
                    checks.push(DiagnosticCheck::ok(
                        format!("peer:{peer_name}"),
                        format!("command '{cmd}' found"),
                    ));
                } else {
                    checks.push(DiagnosticCheck::fail(
                        format!("peer:{peer_name}"),
                        format!("command '{cmd}' missing on PATH"),
                    ));
                }
            }
        }
    }

    let dbus_addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    let has_gdbus = check_binary_on_path("gdbus").is_some();
    if !dbus_addr.is_empty() && has_gdbus {
        let out = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.DBus",
                "--object-path",
                "/org/freedesktop/DBus",
                "--method",
                "org.freedesktop.DBus.ListNames",
            ])
            .output();
        match out {
            Ok(o) if o.status.success() => checks.push(DiagnosticCheck::ok(
                "dbus",
                "session bus reachable via gdbus",
            )),
            _ => checks.push(DiagnosticCheck::warn(
                "dbus",
                "DBUS_SESSION_BUS_ADDRESS set, ListNames probe returned non-zero",
            )),
        }
    } else {
        checks.push(DiagnosticCheck::warn(
            "dbus",
            "session bus not active or gdbus absent (headless/CI mode)",
        ));
    }

    let all_ok = checks.iter().all(|c| c.ok);
    (checks, all_ok)
}

fn parse_kv_arg(args: &[String], key: &str) -> Option<String> {
    args.iter().find_map(|item| {
        item.split_once('=')
            .and_then(|(k, v)| (k == key).then(|| v.to_string()))
    })
}

fn probe_window_peer(peer_name: &str, bus_name: &str) -> DiagnosticCheck {
    let dbus_addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    let has_gdbus = check_binary_on_path("gdbus").is_some();
    if has_gdbus && !dbus_addr.is_empty() && !bus_name.is_empty() {
        let status = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.DBus",
                "--object-path",
                "/org/freedesktop/DBus",
                "--method",
                "org.freedesktop.DBus.NameHasOwner",
                bus_name,
            ])
            .output();
        match status {
            Ok(o) if o.status.success() && String::from_utf8_lossy(&o.stdout).contains("true") => {
                DiagnosticCheck::ok(
                    format!("window:{peer_name}"),
                    format!("bus '{bus_name}' active"),
                )
            }
            _ => DiagnosticCheck::warn(
                format!("window:{peer_name}"),
                format!("bus '{bus_name}' has no owner (window closed)"),
            ),
        }
    } else {
        DiagnosticCheck::warn(
            format!("window:{peer_name}"),
            format!("bus '{bus_name}' unchecked (no session bus)"),
        )
    }
}
