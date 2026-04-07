use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use thiserror::Error;

#[derive(Debug, Clone)]
pub struct CommandCapture {
    pub program: String,
    pub args: Vec<String>,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandCapture {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.stderr.trim().is_empty() {
            parts.push(self.stderr.trim().to_owned());
        }
        if !self.stdout.trim().is_empty() {
            parts.push(self.stdout.trim().to_owned());
        }
        if parts.is_empty() {
            format!(
                "{} {:?} exited with {:?}",
                self.program, self.args, self.status
            )
        } else {
            parts.join(" | ")
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProofCommandRunner {
    base_env: BTreeMap<String, String>,
    secret_pairs: Vec<(String, String)>,
}

#[derive(Debug, Error)]
pub enum CommandRunnerError {
    #[error("failed to execute command {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

impl ProofCommandRunner {
    pub fn new(suite_home: &Path, xdg_root: &Path) -> Self {
        let mut base_env = BTreeMap::new();
        base_env.insert("PATH".to_owned(), std::env::var("PATH").unwrap_or_default());
        base_env.insert("HOME".to_owned(), suite_home.display().to_string());
        base_env.insert(
            "XDG_CONFIG_HOME".to_owned(),
            xdg_root.join("config").display().to_string(),
        );
        base_env.insert(
            "XDG_CACHE_HOME".to_owned(),
            xdg_root.join("cache").display().to_string(),
        );
        base_env.insert(
            "XDG_DATA_HOME".to_owned(),
            xdg_root.join("data").display().to_string(),
        );
        base_env.insert("TZ".to_owned(), "UTC".to_owned());
        base_env.insert("LC_ALL".to_owned(), "C".to_owned());
        base_env.insert("LANG".to_owned(), "C".to_owned());
        base_env.insert("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned());
        base_env.insert("GIT_CONFIG_SYSTEM".to_owned(), "/dev/null".to_owned());
        base_env.insert("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned());
        base_env.insert("GIT_AUTHOR_NAME".to_owned(), "Git Relay Proof".to_owned());
        base_env.insert(
            "GIT_AUTHOR_EMAIL".to_owned(),
            "git-relay-proof@example.com".to_owned(),
        );
        base_env.insert(
            "GIT_COMMITTER_NAME".to_owned(),
            "Git Relay Proof".to_owned(),
        );
        base_env.insert(
            "GIT_COMMITTER_EMAIL".to_owned(),
            "git-relay-proof@example.com".to_owned(),
        );
        base_env.insert(
            "GIT_AUTHOR_DATE".to_owned(),
            "2026-01-01T00:00:00Z".to_owned(),
        );
        base_env.insert(
            "GIT_COMMITTER_DATE".to_owned(),
            "2026-01-01T00:00:00Z".to_owned(),
        );

        Self {
            base_env,
            secret_pairs: Vec::new(),
        }
    }

    pub fn register_secret(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        if value.is_empty() {
            return;
        }
        self.secret_pairs.push((key, value));
    }

    pub fn secret_pairs(&self) -> &[(String, String)] {
        &self.secret_pairs
    }

    pub fn run(
        &self,
        program: impl AsRef<str>,
        args: &[String],
        cwd: Option<&Path>,
        extra_env: &[(String, String)],
    ) -> Result<CommandCapture, CommandRunnerError> {
        let program = program.as_ref().to_owned();
        let mut command = Command::new(&program);
        command.env_clear();
        for (key, value) in &self.base_env {
            command.env(key, value);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }

        let output = command
            .output()
            .map_err(|source| CommandRunnerError::Spawn {
                program: program.clone(),
                source,
            })?;

        Ok(CommandCapture {
            program,
            args: args.to_vec(),
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}
