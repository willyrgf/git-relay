use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedSshCommand {
    pub service: String,
    pub repo_path: PathBuf,
}

pub fn resolve_ssh_command(
    repo_root: &Path,
    original_command: &str,
) -> Result<ResolvedSshCommand, SshWrapperError> {
    let tokens = shlex::split(original_command)
        .ok_or_else(|| SshWrapperError::MalformedCommand(original_command.to_owned()))?;
    if tokens.len() != 2 {
        return Err(SshWrapperError::MalformedCommand(
            original_command.to_owned(),
        ));
    }

    let service = tokens[0].as_str();
    if !matches!(service, "git-upload-pack" | "git-receive-pack") {
        return Err(SshWrapperError::UnsupportedService(tokens[0].clone()));
    }

    let repo_root =
        repo_root
            .canonicalize()
            .map_err(|error| SshWrapperError::CanonicalizeRepoRoot {
                path: repo_root.to_path_buf(),
                error,
            })?;
    let requested_repo = PathBuf::from(&tokens[1]);
    let candidate = if requested_repo.is_absolute() {
        requested_repo
    } else {
        repo_root.join(requested_repo)
    };
    let repo_path =
        candidate
            .canonicalize()
            .map_err(|error| SshWrapperError::CanonicalizeRepoPath {
                path: candidate.clone(),
                error,
            })?;
    if !repo_path.starts_with(&repo_root) {
        return Err(SshWrapperError::RepoOutsideRoot {
            repo_root,
            repo_path,
        });
    }

    Ok(ResolvedSshCommand {
        service: service.to_owned(),
        repo_path,
    })
}

#[derive(Debug, Error)]
pub enum SshWrapperError {
    #[error("malformed SSH original command {0}")]
    MalformedCommand(String),
    #[error("unsupported SSH service {0}; only git-upload-pack and git-receive-pack are allowed")]
    UnsupportedService(String),
    #[error("failed to canonicalize repo root {path}: {error}", path = path.display())]
    CanonicalizeRepoRoot {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to canonicalize repo path {path}: {error}", path = path.display())]
    CanonicalizeRepoPath {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error(
        "requested repository {repo_path} is outside the configured repo root {repo_root}",
        repo_path = repo_path.display(),
        repo_root = repo_root.display()
    )]
    RepoOutsideRoot {
        repo_root: PathBuf,
        repo_path: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::resolve_ssh_command;

    #[test]
    fn resolves_allowed_git_service_under_repo_root() {
        let temp = TempDir::new().expect("tempdir");
        let repo_root = temp.path().join("repos");
        let repo = repo_root.join("example.git");
        std::fs::create_dir_all(&repo).expect("repo");

        let resolved =
            resolve_ssh_command(&repo_root, "git-receive-pack example.git").expect("resolve");

        assert_eq!(resolved.service, "git-receive-pack");
        assert_eq!(
            resolved.repo_path,
            repo.canonicalize().expect("canonical repo")
        );
    }

    #[test]
    fn rejects_non_git_services() {
        let temp = TempDir::new().expect("tempdir");
        let repo_root = temp.path().join("repos");
        std::fs::create_dir_all(&repo_root).expect("repo root");

        let error = resolve_ssh_command(&repo_root, "sh -c whoami").expect_err("reject service");
        assert!(matches!(
            error,
            super::SshWrapperError::UnsupportedService(_)
                | super::SshWrapperError::MalformedCommand(_)
        ));
    }
}
