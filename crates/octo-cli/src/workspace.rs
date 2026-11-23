use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

#[derive(Serialize)]
pub struct Workspace {
    pub project_root: PathBuf,
    pub files: PathBuf,
    pub exports: PathBuf,
    pub inbox: PathBuf,
}

impl Workspace {
    pub fn discover(start: &Path) -> Option<Self> {
        for project_root in start.ancestors() {
            if project_root.join("octomesh.toml").is_file() {
                let root = project_root.join(".octomesh/workspace");
                return Some(Self {
                    project_root: project_root.to_path_buf(),
                    files: root.join("files"),
                    exports: root.join("exports"),
                    inbox: root.join("inbox"),
                });
            }
            // A nested repository or linked worktree needs its own project.
            if project_root.join(".git").exists() {
                break;
            }
        }
        None
    }

    fn state_dir(&self) -> PathBuf {
        self.project_root.join(".octomesh")
    }

    fn root(&self) -> PathBuf {
        self.state_dir().join("workspace")
    }

    pub fn ensure_dirs(&self, create: bool) -> Result<()> {
        for path in [
            self.state_dir(),
            self.root(),
            self.files.clone(),
            self.exports.clone(),
        ] {
            if create {
                let mut builder = DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                match builder.create(&path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(err) => {
                        return Err(err).with_context(|| format!("Create {}", path.display()))
                    }
                }
            }
            let kind = fs::symlink_metadata(&path)
                .with_context(|| format!("Open {} (run octo workspace init)", path.display()))?
                .file_type();
            if !kind.is_dir() {
                bail!(
                    "Workspace directory must be a real directory: {}",
                    path.display()
                );
            }
        }
        if create {
            let ignore = self.root().join(".gitignore");
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&ignore)
            {
                Ok(mut file) => file.write_all(b"*\n")?,
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err).with_context(|| format!("Create {}", ignore.display()))
                }
            }
        }
        Ok(())
    }

    pub fn publish(&self, relative: &Path) -> Result<PathBuf> {
        self.ensure_dirs(false)?;
        let mut source = self.files.clone();
        for part in relative.components() {
            let Component::Normal(name) = part else {
                bail!("Publish requires a relative path inside the workspace files directory");
            };
            if name.to_string_lossy().starts_with('.') {
                bail!("Publish requires a visible file and visible parent directories");
            }
            source.push(name);
            if fs::symlink_metadata(&source)
                .with_context(|| format!("Inspect {}", source.display()))?
                .file_type()
                .is_symlink()
            {
                bail!("Publish paths cannot follow symlinks: {}", source.display());
            }
        }
        let source = source.canonicalize()?;
        if !source.starts_with(self.files.canonicalize()?) || !source.is_file() {
            bail!("Publish source must be a regular file inside the workspace files directory");
        }
        let name = source
            .file_name()
            .context("Publish source needs a file name")?;
        let destination = self.exports.join(name);
        let mut input =
            File::open(&source).with_context(|| format!("Read {}", source.display()))?;
        let before = input.metadata()?;
        let staging = self.root().join(format!(".publish-{}", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&staging)?;
        let published = (|| {
            let copied = io::copy(&mut input, &mut output)?;
            let after = input.metadata()?;
            if copied != before.len()
                || after.len() != before.len()
                || after.modified()? != before.modified()?
            {
                bail!(
                    "Publish source changed while being copied: {}",
                    source.display()
                );
            }
            output.sync_all()?;
            drop(output);
            // Staging stays outside exports. A hard link publishes the complete file
            // atomically and refuses to replace a concurrent publisher's result.
            fs::hard_link(&staging, &destination).with_context(|| {
                format!(
                    "Publish {} (exports require a new name and a filesystem with hard links)",
                    destination.display()
                )
            })
        })();
        let removed = fs::remove_file(&staging);
        published?;
        if let Err(err) = removed {
            tracing::warn!(file = %staging.display(), "Published successfully, but could not clean staging file: {err}");
        }
        Ok(destination)
    }
}
