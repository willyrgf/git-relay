use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

const HOOK_NAMES: [&str; 3] = ["pre-receive", "reference-transaction", "post-receive"];

pub fn install_hooks(
    repo_path: &Path,
    dispatcher: &Path,
) -> Result<Vec<PathBuf>, HookInstallError> {
    if !repo_path.exists() {
        return Err(HookInstallError::MissingRepository(repo_path.to_path_buf()));
    }
    if !dispatcher.is_absolute() {
        return Err(HookInstallError::RelativeDispatcher(
            dispatcher.to_path_buf(),
        ));
    }

    let hooks_dir = repo_path.join("hooks");
    fs::create_dir_all(&hooks_dir).map_err(|error| HookInstallError::CreateHooksDir {
        path: hooks_dir.clone(),
        error,
    })?;

    let mut installed = Vec::new();
    for hook_name in HOOK_NAMES {
        let path = hooks_dir.join(hook_name);
        fs::write(&path, render_hook_script(hook_name, repo_path, dispatcher)).map_err(
            |error| HookInstallError::WriteHook {
                path: path.clone(),
                error,
            },
        )?;
        let mut permissions = fs::metadata(&path)
            .map_err(|error| HookInstallError::StatHook {
                path: path.clone(),
                error,
            })?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).map_err(|error| HookInstallError::ChmodHook {
            path: path.clone(),
            error,
        })?;
        installed.push(path);
    }

    Ok(installed)
}

fn render_hook_script(hook_name: &str, repo_path: &Path, dispatcher: &Path) -> String {
    format!(
        "#!/bin/sh\nset -eu\nrepo=\"${{GIT_DIR:-{repo}}}\"\nexec {dispatcher} hook-dispatch --hook {hook} --repo \"$repo\" \"$@\"\n",
        repo = shell_quote(repo_path),
        dispatcher = shell_quote(dispatcher),
        hook = shell_quote(Path::new(hook_name)),
    )
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookDispatchEvent {
    pub hook: String,
    pub repo: PathBuf,
    pub args: Vec<String>,
}

pub fn dispatch_hook_event(
    hook: String,
    repo: PathBuf,
    args: Vec<String>,
) -> Result<HookDispatchEvent, HookDispatchError> {
    let event = HookDispatchEvent { hook, repo, args };
    if let Ok(path) = std::env::var("GIT_RELAY_HOOK_EVENT_LOG") {
        let event_log_path = PathBuf::from(&path);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| HookDispatchError::OpenEventLog {
                path: event_log_path.clone(),
                error,
            })?;
        writeln!(
            file,
            "{}",
            serde_json::to_string(&event).expect("serialize hook event")
        )
        .map_err(|error| HookDispatchError::WriteEventLog {
            path: event_log_path,
            error,
        })?;
    }
    Ok(event)
}

#[derive(Debug, Error)]
pub enum HookInstallError {
    #[error("repository {0} does not exist")]
    MissingRepository(PathBuf),
    #[error("dispatcher path must be absolute: {0}")]
    RelativeDispatcher(PathBuf),
    #[error("failed to create hooks directory {path}: {error}", path = path.display())]
    CreateHooksDir {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to write hook {path}: {error}", path = path.display())]
    WriteHook {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to stat hook {path}: {error}", path = path.display())]
    StatHook {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to chmod hook {path}: {error}", path = path.display())]
    ChmodHook {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
}

#[derive(Debug, Error)]
pub enum HookDispatchError {
    #[error("failed to open hook event log {path}: {error}", path = path.display())]
    OpenEventLog {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to write hook event log {path}: {error}", path = path.display())]
    WriteEventLog {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::{dispatch_hook_event, install_hooks};

    #[test]
    fn installs_executable_git_hooks() {
        let temp = TempDir::new().expect("tempdir");
        let repo = temp.path().join("repo.git");
        std::fs::create_dir_all(&repo).expect("repo");

        let dispatcher = temp.path().join("dispatcher");
        std::fs::write(&dispatcher, "#!/bin/sh\nexit 0\n").expect("dispatcher");

        let hooks = install_hooks(&repo, &dispatcher).expect("install hooks");
        assert_eq!(hooks.len(), 3);
        for hook in hooks {
            let mode = std::fs::metadata(&hook).expect("stat").permissions().mode();
            assert_eq!(mode & 0o111, 0o111);
        }
    }

    #[test]
    fn dispatch_hook_event_returns_structured_payload() {
        let event = dispatch_hook_event(
            "pre-receive".to_owned(),
            PathBuf::from("/tmp/repo.git"),
            vec!["arg1".to_owned()],
        )
        .expect("dispatch");
        assert_eq!(event.hook, "pre-receive");
        assert_eq!(event.args, vec!["arg1".to_owned()]);
    }
}
