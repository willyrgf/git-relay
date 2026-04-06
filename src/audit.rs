use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredLogEvent {
    pub recorded_at_ms: u128,
    pub event_type: String,
    pub request_id: Option<String>,
    pub repo_id: Option<String>,
    pub push_id: Option<String>,
    pub reconcile_run_id: Option<String>,
    pub upstream_id: Option<String>,
    pub attempt_id: Option<String>,
    pub client_identity: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("failed to create audit directory {path}: {error}", path = path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to open audit log {path}: {error}", path = path.display())]
    Open {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("failed to serialize structured audit event: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to append audit log {path}: {error}", path = path.display())]
    Write {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
}

pub fn structured_log_file_path(state_root: &Path) -> PathBuf {
    state_root.join("logs").join("structured.jsonl")
}

pub fn current_client_identity() -> Option<String> {
    for name in [
        "GIT_RELAY_CLIENT_IDENTITY",
        "GIT_RELAY_AUTHENTICATED_IDENTITY",
        "SSH_USER",
        "USER",
        "LOGNAME",
    ] {
        if let Ok(value) = std::env::var(name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }

    std::env::var("SSH_CONNECTION")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(|value| format!("ssh:{value}"))
}

pub fn new_structured_log_event(event_type: impl Into<String>) -> StructuredLogEvent {
    StructuredLogEvent {
        recorded_at_ms: current_time_ms(),
        event_type: event_type.into(),
        request_id: None,
        repo_id: None,
        push_id: None,
        reconcile_run_id: None,
        upstream_id: None,
        attempt_id: None,
        client_identity: current_client_identity(),
        payload: Value::Null,
    }
}

pub fn record_structured_log(
    state_root: &Path,
    event: &StructuredLogEvent,
) -> Result<(), AuditError> {
    let path = structured_log_file_path(state_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| AuditError::CreateDir {
            path: parent.to_path_buf(),
            error,
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| AuditError::Open {
            path: path.clone(),
            error,
        })?;
    let encoded = serde_json::to_string(event)?;
    writeln!(file, "{encoded}").map_err(|error| AuditError::Write { path, error })?;
    Ok(())
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
