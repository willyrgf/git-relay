use std::path::Path;
use std::process::Command;

use thiserror::Error;

pub trait GitExecutor {
    fn git(&self, git_dir: &Path, args: &[&str]) -> Result<String, GitCommandError>;
}

#[derive(Debug, Default)]
pub struct SystemGitExecutor;

impl GitExecutor for SystemGitExecutor {
    fn git(&self, git_dir: &Path, args: &[&str]) -> Result<String, GitCommandError> {
        let output = Command::new("git")
            .arg(format!("--git-dir={}", git_dir.display()))
            .args(args)
            .output()
            .map_err(|error| GitCommandError::Spawn {
                args: args.iter().map(|item| (*item).to_owned()).collect(),
                error,
            })?;

        if !output.status.success() {
            return Err(GitCommandError::NonZeroExit {
                args: args.iter().map(|item| (*item).to_owned()).collect(),
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

#[derive(Debug, Error)]
pub enum GitCommandError {
    #[error("failed to spawn git for args {args:?}: {error}")]
    Spawn {
        args: Vec<String>,
        #[source]
        error: std::io::Error,
    },
    #[error("git failed for args {args:?} with status {status:?}: {stderr}")]
    NonZeroExit {
        args: Vec<String>,
        status: Option<i32>,
        stderr: String,
    },
}
