//! Encrypted, append-only artifact exchange through an operator's rclone crypt remote.

use crate::workspace::Workspace;
use anyhow::{bail, Context, Result};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapabilityOptions};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};
use tempfile::{Builder, NamedTempFile};

const MANIFEST_LIMIT: usize = 64 * 1024;

#[derive(Debug, clap::Args)]
pub struct BackupArgs {
    /// Named rclone crypt remote and a dedicated folder, e.g. internxt-crypt:OctoMesh.
    #[arg(long)]
    remote: String,
    /// Stable project identifier, e.g. p_123.
    #[arg(long)]
    project: String,
    /// Unique machine identifier for this project, e.g. pc or vps.
    #[arg(long)]
    machine: String,
    /// Origin label for files captured by this worker.
    #[arg(long, default_value = "wspolne")]
    agent: String,
    /// Session label used for this worker.
    #[arg(long, default_value = "workspace")]
    session: String,
    /// Continue scanning, uploading and downloading while the process runs.
    #[arg(long)]
    watch: bool,
    /// Delay between passes, in seconds.
    #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..))]
    interval_secs: u64,
    /// Capture files once their last write is at least this many seconds old.
    #[arg(long, default_value_t = 2)]
    settle_secs: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Origin {
    machine: String,
    agent: String,
    session: String,
}

impl Origin {
    fn from_args(args: &BackupArgs) -> Result<Self> {
        for label in [&args.machine, &args.agent, &args.session] {
            if !is_label(label) {
                bail!("Machine, agent and session labels must contain 1–48 lowercase ASCII letters, digits, '-' or '_'");
            }
        }
        Ok(Self {
            machine: args.machine.clone(),
            agent: args.agent.clone(),
            session: args.session.clone(),
        })
    }

    fn cloud_path(&self, repo: Option<&str>) -> String {
        let path = format!(
            "Maszyny/{}/Agenty/{}/Sesje/{}/Wersje",
            self.machine, self.agent, self.session
        );
        repo.map_or_else(|| path.clone(), |repo| format!("Repozytoria/{repo}/{path}"))
    }
}

struct Remote {
    project: String,
}

impl Remote {
    fn new(remote: &str, project: &str) -> Result<Self> {
        let (name, folder) = remote
            .split_once(':')
            .context("Use a named rclone crypt remote and folder, e.g. internxt-crypt:OctoMesh")?;
        let valid_remote = name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        if !valid_remote || !folder.split('/').all(valid_component) {
            bail!("Remote must name an rclone crypt remote and a dedicated relative folder");
        }
        if !is_label(project) || !project.strip_prefix("p_").is_some_and(|id| !id.is_empty()) {
            bail!("Project must be a stable identifier beginning with p_, e.g. p_123");
        }
        Ok(Self {
            project: format!("{remote}/Projekty/{project}"),
        })
    }

    fn path(&self, subpath: &str) -> String {
        format!("{}/{subpath}", self.project)
    }

    fn ensure_encrypted(&self) -> Result<()> {
        let probe = "octomesh-crypt-probe.txt";
        let bytes = rclone(&["backend", "encode", &self.project, probe])
            .context("Could not verify the named rclone crypt remote")?;
        let encoded = std::str::from_utf8(&bytes)?.trim();
        if encoded.is_empty()
            || encoded == probe
            || encoded.contains(probe)
            || encoded.lines().count() != 1
        {
            bail!("Remote failed the encryption check");
        }
        Ok(())
    }
}

fn rclone(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("rclone")
        .args(args)
        .args([
            "--ask-password=false",
            "--crypt-filename-encryption=standard",
            "--crypt-directory-name-encryption=true",
            "--crypt-no-data-encryption=false",
            "--crypt-show-mapping=false",
            "--log-level=ERROR",
            "--contimeout=10s",
            "--timeout=1m",
            "--retries=1",
            "--low-level-retries=1",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("Could not start rclone")?;
    if !output.status.success() {
        bail!("rclone {} failed ({})", args[0], output.status);
    }
    Ok(output.stdout)
}

fn copy_one(source: &str, destination: &str) -> Result<()> {
    rclone(&["copyto", source, destination, "--immutable", "--size-only"])?;
    Ok(())
}

fn is_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && !value.contains(['/', '\\', ':'])
        && !value.chars().any(char::is_control)
        && Path::new(value)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}

fn digest(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

fn digest_is_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Serialize, Deserialize)]
struct Revision {
    path: Vec<String>,
    length: u64,
    sha256: String,
    modified_ns: u128,
}

impl Revision {
    fn repository(&self) -> Result<Option<&str>> {
        match self.path.as_slice() {
            [root, name, ..] if root == "Repozytoria" && is_label(name) && self.path.len() >= 3 => {
                Ok(Some(name))
            }
            [root, ..] if root == "Repozytoria" => bail!("Use Repozytoria/<repository-id>/<file>"),
            _ => Ok(None),
        }
    }

    fn id(&self) -> Result<String> {
        Ok(digest(serde_json::to_vec(&(
            &self.path,
            &self.sha256,
            self.length,
        ))?))
    }

    fn check(&self) -> Result<()> {
        if self.path.is_empty()
            || !self.path.iter().all(|part| valid_component(part))
            || !digest_is_valid(&self.sha256)
        {
            bail!("Invalid backup manifest");
        }
        self.repository()?;
        if serde_json::to_vec(self)?.len() > MANIFEST_LIMIT {
            bail!("Backup manifest exceeds its size limit");
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq)]
struct Fingerprint {
    length: u64,
    modified_ns: u128,
    #[cfg(unix)]
    unix_change: (u64, u64, i64, i64),
}

impl Fingerprint {
    fn read(meta: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            length: meta.len(),
            modified_ns: meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
            #[cfg(unix)]
            unix_change: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Seen {
    fingerprint: Fingerprint,
    revision: String,
}

struct Store {
    source: Dir,
    pending: PathBuf,
    sent: PathBuf,
    seen: PathBuf,
    staging: PathBuf,
    inbox: PathBuf,
    _lock: File,
}

impl Store {
    fn open(workspace: &Workspace, remote: &Remote, args: &BackupArgs) -> Result<Self> {
        workspace.ensure_dirs(false)?;
        let project = Dir::open_ambient_dir(&workspace.project_root, ambient_authority())?;
        let root = project
            .open_dir_nofollow(".octomesh")?
            .open_dir_nofollow("workspace")?;
        let source = root.open_dir_nofollow("files")?;
        let workspace_root = workspace
            .files
            .parent()
            .context("Missing workspace directory")?;
        let backup = private_dir(workspace_root, "backup")?;
        let namespace = digest(format!(
            "{}\n{}\n{}\n{}",
            remote.project, args.machine, args.agent, args.session
        ));
        let state = private_dir(&backup, &namespace)?;
        let lock_path = state.join("worker.lock");
        if fs::symlink_metadata(&lock_path).is_ok_and(|meta| !meta.is_file()) {
            bail!("Invalid backup worker lock");
        }
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        FileExt::try_lock_exclusive(&lock).context("A worker already owns this backup stream")?;
        let inbox = private_dir(&private_dir(workspace_root, "inbox")?, &args.project)?;
        Ok(Self {
            source,
            pending: private_dir(&state, "pending")?,
            sent: private_dir(&state, "sent")?,
            seen: private_dir(&state, "seen")?,
            staging: private_dir(&state, "staging")?,
            inbox,
            _lock: lock,
        })
    }

    fn scan(&self, settle: Duration) -> Result<()> {
        let mut queued = 0;
        let mut errors = 0;
        self.scan_dir(
            &self.source,
            &mut Vec::new(),
            settle,
            &mut queued,
            &mut errors,
        )?;
        if queued != 0 {
            tracing::info!(queued, "Staged workspace revisions");
        }
        if errors != 0 {
            bail!("{errors} workspace entries could not be captured; other files were staged");
        }
        Ok(())
    }

    fn scan_dir(
        &self,
        dir: &Dir,
        path: &mut Vec<String>,
        settle: Duration,
        queued: &mut usize,
        errors: &mut usize,
    ) -> Result<()> {
        for entry in dir.entries()? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    *errors += 1;
                    continue;
                }
            };
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|name| valid_component(name)) else {
                continue;
            };
            if name.ends_with(".part") || name.ends_with(".tmp") || name.ends_with('~') {
                continue;
            }
            path.push(name.to_string());
            let result = entry
                .file_type()
                .map_err(anyhow::Error::from)
                .and_then(|kind| {
                    if kind.is_dir() {
                        self.scan_dir(&dir.open_dir_nofollow(name)?, path, settle, queued, errors)
                    } else if kind.is_file() {
                        *queued += usize::from(self.capture(dir, name, path, settle)?);
                        Ok(())
                    } else {
                        Ok(())
                    }
                });
            path.pop();
            if result.is_err() {
                *errors += 1;
            }
        }
        Ok(())
    }

    fn capture(&self, dir: &Dir, name: &str, path: &[String], settle: Duration) -> Result<bool> {
        let mut source = dir
            .open_with(
                name,
                CapabilityOptions::new()
                    .read(true)
                    .follow(FollowSymlinks::No),
            )?
            .into_std();
        let metadata = source.metadata()?;
        if !metadata.is_file() || metadata.modified()?.elapsed().is_ok_and(|age| age < settle) {
            return Ok(false);
        }
        let fingerprint = Fingerprint::read(&metadata)?;
        let index = self.seen.join(digest(serde_json::to_vec(path)?));
        let unchanged = fs::read(&index)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Seen>(&bytes).ok())
            .is_some_and(|seen| {
                seen.fingerprint == fingerprint
                    && digest_is_valid(&seen.revision)
                    && self.is_stored(&seen.revision)
            });
        if unchanged {
            return Ok(false);
        }

        let stage = Builder::new()
            .prefix(".capture-")
            .tempdir_in(&self.staging)?;
        if fs2::available_space(stage.path())? < fingerprint.length {
            bail!("Insufficient local space to stage the workspace revision");
        }
        let mut payload = File::create(stage.path().join("payload"))?;
        let (sha256, length) = hash_and_copy(&mut source, &mut payload)?;
        if Fingerprint::read(&source.metadata()?)? != fingerprint || length != fingerprint.length {
            return Ok(false);
        }
        let revision = Revision {
            path: path.to_vec(),
            length,
            sha256,
            modified_ns: fingerprint.modified_ns,
        };
        revision.check()?;
        let id = revision.id()?;
        let queued = !self.is_stored(&id);
        if queued {
            payload.sync_all()?;
            drop(payload);
            write_json(&stage.path().join("manifest.json"), &revision)?;
            fs::rename(stage.path(), self.pending.join(&id)).context("Commit staged snapshot")?;
            sync_directory(&self.pending)?;
        }
        write_json(
            &index,
            &Seen {
                fingerprint,
                revision: id,
            },
        )?;
        Ok(queued)
    }

    fn is_stored(&self, id: &str) -> bool {
        self.pending.join(id).is_dir() || self.sent.join(id).is_file()
    }

    fn send_pending(&self, remote: &Remote, origin: &Origin) -> Result<usize> {
        let mut sent = 0;
        for entry in fs::read_dir(&self.pending)? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_dir() || !digest_is_valid(&id) {
                continue;
            }
            if self.sent.join(&id).exists() {
                fs::remove_dir_all(entry.path())?;
                continue;
            }
            let bytes = fs::read(entry.path().join("manifest.json"))?;
            let record: Revision = serde_json::from_slice(&bytes)?;
            record.check()?;
            if record.id()? != id {
                bail!("Pending revision and manifest differ");
            }
            let folder = remote.path(&format!(
                "{}/{}",
                origin.cloud_path(record.repository()?),
                id
            ));
            let payload = entry.path().join("payload");
            let manifest = entry.path().join("manifest.json");
            copy_one(as_str(&payload)?, &format!("{folder}/payload"))?;
            copy_one(as_str(&manifest)?, &format!("{folder}/manifest.json"))?;
            write_json(&self.sent.join(&id), &record)?;
            fs::remove_dir_all(entry.path())?;
            sent += 1;
        }
        Ok(sent)
    }

    fn receive(&self, remote: &Remote, current: &Origin) -> Result<usize> {
        let listing = rclone(&[
            "lsf",
            &remote.project,
            "--dirs-only",
            "--recursive",
            "--max-depth",
            "10",
        ])?;
        let listing = std::str::from_utf8(&listing)?;
        let mut received = 0;
        let mut errors = 0;
        for line in listing.lines() {
            let relative = line.trim_end_matches('/');
            let Some((origin, id, repo)) = read_route(relative) else {
                continue;
            };
            if origin.machine == current.machine {
                continue;
            }
            match self.receive_one(remote, relative, &origin, id, repo) {
                Ok(new) => received += usize::from(new),
                Err(_) => errors += 1,
            }
        }
        if received != 0 {
            tracing::info!(received, "Verified incoming workspace revisions");
        }
        if errors != 0 {
            bail!("{errors} incoming revisions are incomplete or could not be verified; retrying is safe");
        }
        Ok(received)
    }

    fn receive_one(
        &self,
        remote: &Remote,
        relative: &str,
        origin: &Origin,
        id: &str,
        repo: Option<&str>,
    ) -> Result<bool> {
        let machine = private_dir(&self.inbox, &origin.machine)?;
        let agents = private_dir(&machine, "Agenty")?;
        let agent = private_dir(&agents, &origin.agent)?;
        let sessions = private_dir(&agent, "Sesje")?;
        let session = private_dir(&sessions, &origin.session)?;
        let final_dir = session.join(id);
        if fs::symlink_metadata(&final_dir).is_ok_and(|meta| meta.is_dir()) {
            return Ok(false);
        }
        let folder = remote.path(relative);
        let meta = rclone(&[
            "cat",
            &format!("{folder}/manifest.json"),
            "--count",
            "65537",
        ])?;
        if meta.len() > MANIFEST_LIMIT {
            bail!("Incoming manifest exceeds its size limit");
        }
        let revision: Revision = serde_json::from_slice(&meta)?;
        revision.check()?;
        if revision.repository()? != repo || revision.id()? != id {
            bail!("Incoming revision and manifest differ");
        }
        let stage = Builder::new()
            .prefix(".receive-")
            .tempdir_in(&self.staging)?;
        if fs2::available_space(stage.path())? < revision.length {
            bail!("Insufficient local space for incoming revision");
        }
        let mut dir = private_dir(stage.path(), "files")?;
        let (file, parents) = revision
            .path
            .split_last()
            .context("Missing incoming path")?;
        for parent in parents {
            dir = private_dir(&dir, parent)?;
        }
        let payload = dir.join(file);
        copy_one(&format!("{folder}/payload"), as_str(&payload)?)?;
        let mut contents = OpenOptions::new().read(true).write(true).open(&payload)?;
        let (hash, length) = hash_and_copy(&mut contents, &mut std::io::sink())?;
        if length != revision.length || hash != revision.sha256 {
            bail!("Incoming file does not match its SHA-256 manifest");
        }
        contents.sync_all()?;
        drop(contents);
        sync_directory(&dir)?;
        write_json(&stage.path().join("manifest.json"), &revision)?;
        match fs::rename(stage.path(), &final_dir) {
            Ok(()) => {
                sync_directory(&session)?;
                Ok(true)
            }
            Err(_) if fs::symlink_metadata(&final_dir).is_ok_and(|meta| meta.is_dir()) => Ok(false),
            Err(err) => Err(err).context("Publish verified incoming revision"),
        }
    }
}

fn read_route(relative: &str) -> Option<(Origin, &str, Option<&str>)> {
    let components: Vec<_> = relative.split('/').collect();
    let (repo, stream) = match components.as_slice() {
        ["Repozytoria", repo, rest @ ..] if is_label(repo) => (Some(*repo), rest),
        rest => (None, rest),
    };
    let ["Maszyny", machine, "Agenty", agent, "Sesje", session, "Wersje", id] = stream else {
        return None;
    };
    if ![machine, agent, session]
        .into_iter()
        .all(|label| is_label(label))
        || !digest_is_valid(id)
    {
        return None;
    }
    Some((
        Origin {
            machine: (*machine).into(),
            agent: (*agent).into(),
            session: (*session).into(),
        },
        id,
        repo,
    ))
}

fn as_str(path: &Path) -> Result<&str> {
    path.to_str().context("Backup paths must be UTF-8")
}

fn private_dir(parent: &Path, name: impl AsRef<OsStr>) -> Result<PathBuf> {
    let path = parent.join(name.as_ref());
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }
    if !fs::symlink_metadata(&path)?.is_dir() {
        bail!("Backup state requires real local directories");
    }
    Ok(path)
}

fn write_json(path: &Path, data: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("Missing state parent")?;
    let mut file = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(file.as_file_mut(), data)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    sync_directory(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn hash_and_copy(input: &mut impl Read, output: &mut impl Write) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut length = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        length += count as u64;
    }
    Ok((format!("{:x}", hash.finalize()), length))
}

fn exchange(store: &Store, remote: &Remote, origin: &Origin) -> Result<()> {
    remote.ensure_encrypted()?;
    rclone(&["mkdir", &remote.path("Repozytoria")])?;
    rclone(&["mkdir", &remote.path("Maszyny")])?;
    let uploaded = store.send_pending(remote, origin);
    let incoming = store.receive(remote, origin);
    let uploaded = uploaded?;
    if uploaded != 0 {
        tracing::info!(uploaded, "Uploaded encrypted workspace revisions");
    }
    incoming?;
    Ok(())
}

pub fn run(workspace: &Workspace, args: BackupArgs) -> Result<()> {
    let origin = Origin::from_args(&args)?;
    let remote = Remote::new(&args.remote, &args.project)?;
    let store = Store::open(workspace, &remote, &args)?;
    let interval = Duration::from_secs(args.interval_secs);
    let settle = Duration::from_secs(args.settle_secs);

    if !args.watch {
        let scan = store.scan(settle);
        let transfer = exchange(&store, &remote, &origin);
        scan?;
        return transfer;
    }
    thread::scope(|scope| {
        scope.spawn(|| loop {
            if let Err(err) = store.scan(settle) {
                tracing::warn!("{err:#}");
            }
            thread::sleep(interval);
        });
        loop {
            if let Err(err) = exchange(&store, &remote, &origin) {
                tracing::warn!(error = %err, "Encrypted exchange will retry");
            }
            thread::sleep(interval);
        }
    })
}
