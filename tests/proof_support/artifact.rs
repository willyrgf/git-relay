use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("failed to create directory {path}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write artifact file {path}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read artifact file {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to encode json")]
    Json { source: serde_json::Error },
    #[error("artifact contains unresolved secret material")]
    SecretLeak,
}

#[derive(Debug, Clone)]
pub struct GitConformanceEvidenceInput<'a> {
    pub profile: &'a str,
    pub platform: &'a str,
    pub git_version: &'a str,
    pub openssh_version: &'a str,
    pub nix_system: &'a str,
    pub service_manager: &'a str,
    pub filesystem_profile: &'a str,
    pub git_relay_commit: &'a str,
    pub flake_lock_sha256: &'a str,
    pub binary_digests: BTreeMap<String, String>,
    pub case_summaries: Vec<(String, bool)>,
    pub required_cases_passed: bool,
    pub normalized_summary_sha256: &'a str,
}

pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, ArtifactError> {
    let normalized = canonicalize(value);
    serde_json::to_vec_pretty(&normalized).map_err(|source| ArtifactError::Json { source })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

pub fn hash_file_sha256(path: &Path) -> Result<String, ArtifactError> {
    let bytes = fs::read(path).map_err(|source| ArtifactError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(sha256_hex(&bytes))
}

pub fn persist_json<P: AsRef<Path>>(path: P, value: &Value) -> Result<(), ArtifactError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ArtifactError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let bytes = canonical_json_bytes(value)?;
    let mut file = File::create(path).map_err(|source| ArtifactError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(&bytes)
        .map_err(|source| ArtifactError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(b"\n")
        .map_err(|source| ArtifactError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

pub fn persist_text<P: AsRef<Path>>(path: P, value: &str) -> Result<(), ArtifactError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ArtifactError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, value).map_err(|source| ArtifactError::Write {
        path: path.to_path_buf(),
        source,
    })
}

pub fn redact_json_value(
    value: &Value,
    secret_pairs: &[(String, String)],
) -> Result<Value, ArtifactError> {
    let redacted = redact_json_value_inner(value, secret_pairs);
    let encoded =
        serde_json::to_string(&redacted).map_err(|source| ArtifactError::Json { source })?;
    if contains_secret_patterns(&encoded, secret_pairs) {
        return Err(ArtifactError::SecretLeak);
    }
    Ok(redacted)
}

pub fn redact_and_persist_failures(
    path: &Path,
    raw: &str,
    secret_pairs: &[(String, String)],
) -> Result<(), ArtifactError> {
    let redacted = redact_secrets(raw, secret_pairs);
    if contains_secret_patterns(&redacted, secret_pairs) {
        return Err(ArtifactError::SecretLeak);
    }
    persist_text(path, &redacted)
}

pub fn redact_secrets(source: &str, secret_pairs: &[(String, String)]) -> String {
    let mut value = source.to_owned();

    for (key, secret) in secret_pairs {
        if !secret.is_empty() {
            value = value.replace(secret, "<redacted:env>");
        }
        if key_is_sensitive(key) {
            let marker = format!("{key}={secret}");
            value = value.replace(&marker, &format!("{key}=<redacted:env>"));
        }
    }

    value = redact_authorization_headers(&value);
    value = redact_url_credentials(&value);
    redact_pem_private_keys(&value)
}

pub fn contains_secret_material(source: &str, secret_pairs: &[(String, String)]) -> bool {
    contains_secret_patterns(source, secret_pairs)
}

pub fn git_conformance_evidence_value(input: GitConformanceEvidenceInput<'_>) -> Value {
    let case_values = input
        .case_summaries
        .iter()
        .map(|(case_id, pass)| {
            json!({
                "case_id": case_id,
                "status": if *pass { "pass" } else { "fail" },
            })
        })
        .collect::<Vec<_>>();

    let mut binary_digests = Map::new();
    for (name, digest) in &input.binary_digests {
        binary_digests.insert(name.clone(), Value::String(digest.clone()));
    }

    json!({
        "schema_version": 1,
        "profile": input.profile,
        "git_version_key": sanitize_key(input.git_version),
        "platform": input.platform,
        "nix_system": input.nix_system,
        "service_manager": input.service_manager,
        "git_version": input.git_version,
        "openssh_version": input.openssh_version,
        "filesystem_profile": input.filesystem_profile,
        "git_relay_commit": input.git_relay_commit,
        "flake_lock_sha256": input.flake_lock_sha256,
        "binary_digests": Value::Object(binary_digests),
        "cases": case_values,
        "all_mandatory_cases_passed": input.required_cases_passed,
        "normalized_summary_sha256": input.normalized_summary_sha256,
        "recorded_at_ms": 0,
    })
}

fn key_is_sensitive(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("pass")
        || lower.contains("key")
        || lower.contains("auth")
}

fn redact_json_value_inner(value: &Value, secret_pairs: &[(String, String)]) -> Value {
    match value {
        Value::Object(map) => {
            let mut normalized = Map::new();
            for (key, item) in map {
                normalized.insert(key.clone(), redact_json_value_inner(item, secret_pairs));
            }
            Value::Object(normalized)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_json_value_inner(item, secret_pairs))
                .collect(),
        ),
        Value::String(value) => Value::String(redact_secrets(value, secret_pairs)),
        Value::Number(value) => Value::Number(value.clone()),
        Value::Bool(value) => Value::Bool(*value),
        Value::Null => Value::Null,
    }
}

fn contains_secret_patterns(source: &str, secret_pairs: &[(String, String)]) -> bool {
    let lowered = source.to_ascii_lowercase();
    if lowered.contains("authorization:")
        && !lowered.contains("authorization: <redacted:authorization>")
    {
        return true;
    }
    if has_url_credentials(source) {
        return true;
    }
    if lowered.contains("private key") {
        return true;
    }
    for (_, secret) in secret_pairs {
        if !secret.is_empty() && source.contains(secret) {
            return true;
        }
    }
    false
}

fn has_url_credentials(source: &str) -> bool {
    let mut cursor = 0;
    while let Some(pos) = source[cursor..].find("://") {
        let start = cursor + pos + 3;
        let rest = &source[start..];
        let authority_end = rest
            .find('/')
            .or_else(|| rest.find('?'))
            .or_else(|| rest.find('#'))
            .unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if let Some(at) = authority.find('@') {
            let userinfo = &authority[..at];
            if userinfo.contains(':') && userinfo != "<redacted:url-credentials>" {
                return true;
            }
        }
        cursor = start;
    }
    false
}

fn redact_authorization_headers(source: &str) -> String {
    let mut lines = Vec::new();
    for line in source.lines() {
        if line.to_ascii_lowercase().starts_with("authorization:") {
            lines.push("authorization: <redacted:authorization>".to_owned());
        } else {
            lines.push(line.to_owned());
        }
    }
    let mut output = lines.join("\n");
    if source.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn redact_url_credentials(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut remaining = source;
    while let Some(scheme) = remaining.find("://") {
        let prefix = &remaining[..scheme + 3];
        out.push_str(prefix);
        let rest = &remaining[scheme + 3..];

        let delimiter = rest
            .find('/')
            .or_else(|| rest.find('?'))
            .or_else(|| rest.find('#'))
            .unwrap_or(rest.len());
        let authority = &rest[..delimiter];

        if let Some(at) = authority.find('@') {
            let userinfo = &authority[..at];
            if userinfo.contains(':') {
                out.push_str("<redacted:url-credentials>@");
                out.push_str(&authority[at + 1..]);
            } else {
                out.push_str(authority);
            }
        } else {
            out.push_str(authority);
        }

        remaining = &rest[delimiter..];
    }
    out.push_str(remaining);
    out
}

fn redact_pem_private_keys(source: &str) -> String {
    let begin_marker = "-----BEGIN";
    let key_marker = "PRIVATE KEY-----";
    let end_marker = "-----END";

    let mut output = String::new();
    let mut cursor = 0;
    while let Some(begin_rel) = source[cursor..].find(begin_marker) {
        let begin = cursor + begin_rel;
        output.push_str(&source[cursor..begin]);

        let after_begin = &source[begin..];
        let Some(key_pos_rel) = after_begin.find(key_marker) else {
            output.push_str(after_begin);
            return output;
        };
        let key_end = begin + key_pos_rel + key_marker.len();

        let after_key = &source[key_end..];
        let Some(end_rel) = after_key.find(end_marker) else {
            output.push_str("<redacted:private-key>");
            return output;
        };
        let end_start = key_end + end_rel;
        let after_end = &source[end_start..];
        let Some(end_line_rel) = after_end.find("PRIVATE KEY-----") else {
            output.push_str("<redacted:private-key>");
            return output;
        };
        let end_idx = end_start + end_line_rel + "PRIVATE KEY-----".len();

        output.push_str("<redacted:private-key>");
        cursor = end_idx;
    }
    output.push_str(&source[cursor..]);
    output
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: BTreeMap<&String, Value> = BTreeMap::new();
            for (key, value) in map {
                entries.insert(key, canonicalize(value));
            }
            let mut object = Map::new();
            for (key, value) in entries {
                object.insert(key.clone(), value);
            }
            Value::Object(object)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::String(value) => Value::String(value.clone()),
        Value::Number(value) => Value::Number(value.clone()),
        Value::Bool(value) => Value::Bool(*value),
        Value::Null => Value::Null,
    }
}

fn sanitize_key(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        contains_secret_patterns, git_conformance_evidence_value, redact_secrets,
        GitConformanceEvidenceInput,
    };

    #[test]
    fn redacts_known_secret_classes() {
        let source = "authorization: Basic abc\nurl=http://alice:pw@example.com/repo.git\n-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\nTOKEN=abc\n";
        let redacted = redact_secrets(source, &[("TOKEN".to_owned(), "abc".to_owned())]);
        assert!(redacted.contains("<redacted:authorization>"));
        assert!(redacted.contains("<redacted:url-credentials>"));
        assert!(redacted.contains("<redacted:private-key>"));
        assert!(redacted.contains("TOKEN=<redacted:env>"));
        assert!(!contains_secret_patterns(
            &redacted,
            &[("TOKEN".to_owned(), "abc".to_owned())]
        ));
    }

    #[test]
    fn git_conformance_evidence_uses_deterministic_timestamp() {
        let evidence = git_conformance_evidence_value(GitConformanceEvidenceInput {
            profile: "deterministic-core",
            platform: "linux",
            git_version: "git version 2.53.0",
            openssh_version: "OpenSSH_10.2p1",
            nix_system: "x86_64-linux",
            service_manager: "systemd",
            filesystem_profile: "ext4",
            git_relay_commit: "abc123",
            flake_lock_sha256: "deadbeef",
            binary_digests: BTreeMap::new(),
            case_summaries: vec![("P01".to_owned(), true)],
            required_cases_passed: true,
            normalized_summary_sha256: "cafebabe",
        });
        assert_eq!(evidence["recorded_at_ms"], 0);
    }
}
