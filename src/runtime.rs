use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use tempfile::TempDir;
use tokio::process::Command;

use crate::{ErrorCode, ToolError};

const RUNTIME_PREFIX: &str = "grok-build-search-runtime-";
const ACTIVE_LOCK_NAME: &str = ".active.lock";
const GLOBAL_LOCK_NAME: &str = ".grok-build-search-runtime-cleanup.lock";
const COPIED_CONFIG_FILES: &[&str] = &["config.toml", "models_cache.json", "agent_id"];

#[derive(Debug, Clone)]
pub(crate) struct RuntimeManager {
    root: PathBuf,
    source_grok_home: Option<PathBuf>,
    auth_path: Option<PathBuf>,
}

impl RuntimeManager {
    pub(crate) fn new(
        environment: &BTreeMap<OsString, OsString>,
        runtime_root: Option<PathBuf>,
    ) -> Self {
        let home = effective_environment(environment, "HOME").map(PathBuf::from);
        let source_grok_home = effective_environment(environment, "GROK_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|path| path.join(".grok")));
        let auth_path = effective_environment(environment, "GROK_AUTH_PATH")
            .map(PathBuf::from)
            .or_else(|| source_grok_home.as_ref().map(|path| path.join("auth.json")))
            .map(absolutize);

        Self {
            root: runtime_root.unwrap_or_else(std::env::temp_dir),
            source_grok_home,
            auth_path,
        }
    }

    pub(crate) fn start(&self) -> Result<GrokRuntime, ToolError> {
        fs::create_dir_all(&self.root).map_err(|error| {
            runtime_error(format!("could not create Grok runtime root: {error}"))
        })?;
        let global_lock = self.lock_global()?;
        let cleanup_deferred = self.cleanup_stale_locked();
        let directory = tempfile::Builder::new()
            .prefix(RUNTIME_PREFIX)
            .tempdir_in(&self.root)
            .map_err(|error| {
                runtime_error(format!("could not create isolated Grok runtime: {error}"))
            })?;
        let active_lock =
            open_lock_file(&directory.path().join(ACTIVE_LOCK_NAME)).map_err(|error| {
                runtime_error(format!(
                    "could not create Grok runtime activity lock: {error}"
                ))
            })?;
        active_lock.lock_exclusive().map_err(|error| {
            runtime_error(format!("could not lock the isolated Grok runtime: {error}"))
        })?;
        self.copy_configuration(directory.path())?;
        unlock(&global_lock);

        Ok(GrokRuntime {
            directory: Some(directory),
            active_lock: Some(active_lock),
            auth_path: self.auth_path.clone(),
            cleanup_deferred,
        })
    }

    pub(crate) fn cleanup_stale(&self) -> bool {
        if let Err(error) = fs::create_dir_all(&self.root) {
            tracing::warn!(%error, "Could not create the Grok runtime root for deferred cleanup");
            return true;
        }
        let Ok(global_lock) = self.lock_global().inspect_err(|error| {
            tracing::warn!(%error, "Could not acquire the Grok runtime cleanup lock");
        }) else {
            return true;
        };
        let cleanup_deferred = self.cleanup_stale_locked();
        unlock(&global_lock);
        cleanup_deferred
    }

    fn lock_global(&self) -> Result<File, ToolError> {
        let lock = open_lock_file(&self.root.join(GLOBAL_LOCK_NAME)).map_err(|error| {
            runtime_error(format!("could not open Grok runtime cleanup lock: {error}"))
        })?;
        lock.lock_exclusive().map_err(|error| {
            runtime_error(format!(
                "could not acquire Grok runtime cleanup lock: {error}"
            ))
        })?;
        Ok(lock)
    }

    fn cleanup_stale_locked(&self) -> bool {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(%error, "Could not scan deferred Grok runtimes");
                return true;
            }
        };
        let mut cleanup_deferred = false;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(%error, "Could not read a deferred Grok runtime entry");
                    cleanup_deferred = true;
                    continue;
                }
            };
            if !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(RUNTIME_PREFIX))
            {
                continue;
            }
            cleanup_deferred |= !cleanup_stale_path(&entry.path());
        }
        cleanup_deferred
    }

    fn copy_configuration(&self, destination: &Path) -> Result<(), ToolError> {
        let Some(source) = &self.source_grok_home else {
            return Ok(());
        };
        for file_name in COPIED_CONFIG_FILES {
            let source_file = source.join(file_name);
            if !source_file.is_file() {
                continue;
            }
            fs::copy(&source_file, destination.join(file_name)).map_err(|error| {
                runtime_error(format!(
                    "could not copy Grok configuration file {file_name}: {error}"
                ))
            })?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct GrokRuntime {
    directory: Option<TempDir>,
    active_lock: Option<File>,
    auth_path: Option<PathBuf>,
    cleanup_deferred: bool,
}

impl GrokRuntime {
    pub(crate) fn path(&self) -> &Path {
        self.directory
            .as_ref()
            .expect("runtime directory must exist until cleanup")
            .path()
    }

    pub(crate) fn apply_environment(&self, command: &mut Command) {
        command
            .env("HOME", self.path())
            .env("GROK_HOME", self.path())
            .env("GROK_STORAGE_MODE", "local");
        if let Some(auth_path) = &self.auth_path {
            command.env("GROK_AUTH_PATH", auth_path);
        }
    }

    pub(crate) fn finish(mut self) -> bool {
        if let Some(active_lock) = self.active_lock.take() {
            unlock(&active_lock);
        }
        let Some(directory) = self.directory.take() else {
            return self.cleanup_deferred;
        };
        if let Err(error) = directory.close() {
            tracing::warn!(%error, "Temporary Grok state cleanup was deferred");
            self.cleanup_deferred = true;
        }
        self.cleanup_deferred
    }
}

fn cleanup_stale_path(path: &Path) -> bool {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
        Err(error) => {
            tracing::warn!(%error, "Could not inspect a deferred Grok runtime");
            return false;
        }
    };
    if !metadata.file_type().is_dir() {
        return remove_file(path);
    }

    let active_lock_path = path.join(ACTIVE_LOCK_NAME);
    if fs::symlink_metadata(&active_lock_path)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        tracing::warn!("Refusing to follow a symlinked Grok runtime activity lock");
        return false;
    }
    let active_lock = match open_lock_file(&active_lock_path) {
        Ok(lock) => lock,
        Err(error) => {
            tracing::warn!(%error, "Could not open a deferred Grok runtime activity lock");
            return false;
        }
    };
    match active_lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
        Err(error) => {
            tracing::warn!(%error, "Could not check a deferred Grok runtime activity lock");
            return false;
        }
    }

    let removed = match fs::remove_dir_all(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(%error, "Could not remove a deferred Grok runtime");
            false
        }
    };
    unlock(&active_lock);
    removed
}

fn remove_file(path: &Path) -> bool {
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(%error, "Could not remove a deferred Grok runtime entry");
            false
        }
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn unlock(file: &File) {
    if let Err(error) = FileExt::unlock(file) {
        tracing::warn!(%error, "Could not unlock a Grok runtime lock file");
    }
}

fn effective_environment(
    environment: &BTreeMap<OsString, OsString>,
    name: &str,
) -> Option<OsString> {
    environment
        .get(OsStr::new(name))
        .cloned()
        .or_else(|| std::env::var_os(name))
}

fn absolutize(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|current| current.join(&path))
            .unwrap_or(path)
    }
}

fn runtime_error(message: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::GrokExitFailed, message)
}
