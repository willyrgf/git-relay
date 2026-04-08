use serde_json::Value;

#[derive(Debug, Clone)]
pub struct NormalizeContext {
    pub temp_roots: Vec<String>,
}

impl NormalizeContext {
    pub fn for_temp_root(root: &std::path::Path) -> Self {
        Self {
            temp_roots: vec![root.display().to_string()],
        }
    }
}

pub fn normalize_value(value: &Value, context: &NormalizeContext) -> Value {
    normalize_with_key(None, value, context)
}

fn normalize_with_key(key: Option<&str>, value: &Value, context: &NormalizeContext) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Bool(value) => Value::Bool(*value),
        Value::Number(number) => {
            if key_indicates_dynamic_number(key) {
                Value::Number(serde_json::Number::from(0))
            } else {
                Value::Number(number.clone())
            }
        }
        Value::String(text) => Value::String(normalize_string(text, context)),
        Value::Array(values) => {
            let mut normalized = values
                .iter()
                .map(|entry| normalize_with_key(None, entry, context))
                .collect::<Vec<_>>();
            sort_semantic_array(&mut normalized);
            Value::Array(normalized)
        }
        Value::Object(map) => {
            let mut normalized = serde_json::Map::new();
            for (entry_key, entry_value) in map {
                normalized.insert(
                    entry_key.clone(),
                    normalize_with_key(Some(entry_key), entry_value, context),
                );
            }
            Value::Object(normalized)
        }
    }
}

fn key_indicates_dynamic_number(key: Option<&str>) -> bool {
    let Some(key) = key else {
        return false;
    };
    let lower = key.to_ascii_lowercase();
    lower.ends_with("_ms")
        || lower.ends_with("_at")
        || lower.contains("timestamp")
        || lower.contains("pid")
        || lower.contains("port")
}

fn normalize_string(source: &str, context: &NormalizeContext) -> String {
    let mut output = source.to_owned();

    for root in &context.temp_roots {
        if !root.is_empty() {
            output = output.replace(root, "<tmp>");
        }
    }

    output = normalize_loopback_ports(&output);
    output = normalize_ids(&output, "reconcile-");
    output = normalize_ids(&output, "probe-");
    output = normalize_ids(&output, "request-");
    output = normalize_ids(&output, "push-");
    output = normalize_ids(&output, "proof-");
    output = normalize_ssh_fingerprint(&output);

    output
}

fn normalize_loopback_ports(source: &str) -> String {
    let mut output = String::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if source[i..].starts_with("127.0.0.1:") {
            output.push_str("127.0.0.1:<port>");
            i += "127.0.0.1:".len();
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        } else {
            output.push(bytes[i] as char);
            i += 1;
        }
    }
    output
}

fn normalize_ids(source: &str, prefix: &str) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    while let Some(start_rel) = source[cursor..].find(prefix) {
        let start = cursor + start_rel;
        output.push_str(&source[cursor..start]);

        let rest = &source[start..];
        let mut end = rest.len();
        for (index, ch) in rest.char_indices() {
            if index < prefix.len() {
                continue;
            }
            if !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')) {
                end = index;
                break;
            }
        }
        output.push_str(prefix);
        output.push_str("<id>");
        cursor = start + end;
    }
    output.push_str(&source[cursor..]);
    output
}

fn normalize_ssh_fingerprint(source: &str) -> String {
    if !source.contains("SHA256:") {
        return source.to_owned();
    }
    let mut output = String::new();
    let mut cursor = 0;
    while let Some(start_rel) = source[cursor..].find("SHA256:") {
        let start = cursor + start_rel;
        output.push_str(&source[cursor..start]);
        output.push_str("SHA256:<fingerprint>");

        let rest = &source[start + "SHA256:".len()..];
        let mut consumed = 0;
        for ch in rest.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=') {
                consumed += ch.len_utf8();
            } else {
                break;
            }
        }
        cursor = start + "SHA256:".len() + consumed;
    }
    output.push_str(&source[cursor..]);
    output
}

fn sort_semantic_array(values: &mut [Value]) {
    let key = if values
        .iter()
        .all(|entry| entry.get("repo_id").and_then(Value::as_str).is_some())
    {
        Some("repo_id")
    } else if values
        .iter()
        .all(|entry| entry.get("case_id").and_then(Value::as_str).is_some())
    {
        Some("case_id")
    } else if values
        .iter()
        .all(|entry| entry.get("upstream_id").and_then(Value::as_str).is_some())
    {
        Some("upstream_id")
    } else if values
        .iter()
        .all(|entry| entry.get("target_id").and_then(Value::as_str).is_some())
    {
        Some("target_id")
    } else {
        None
    };

    if let Some(key) = key {
        values.sort_by(|left, right| {
            let left_key = left
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let right_key = right
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            left_key.cmp(&right_key)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_value, NormalizeContext};

    #[test]
    fn normalizes_dynamic_values() {
        let raw = serde_json::json!({
            "run_id": "reconcile-123-456",
            "recorded_at_ms": 1234,
            "url": "http://127.0.0.1:54321/repo.git",
            "path": "/tmp/proof-x/state",
            "fingerprint": "SHA256:abcXYZ123",
        });
        let ctx = NormalizeContext {
            temp_roots: vec!["/tmp/proof-x".to_owned()],
        };
        let normalized = normalize_value(&raw, &ctx);
        assert_eq!(normalized["recorded_at_ms"], 0);
        assert_eq!(
            normalized["url"].as_str().expect("url"),
            "http://127.0.0.1:<port>/repo.git"
        );
        assert!(normalized["path"].as_str().expect("path").contains("<tmp>"));
        assert!(normalized["run_id"]
            .as_str()
            .expect("run_id")
            .contains("reconcile-<id>"));
        assert!(normalized["fingerprint"]
            .as_str()
            .expect("fingerprint")
            .contains("SHA256:<fingerprint>"));
    }

    #[test]
    fn sorts_repo_arrays_semantically() {
        let raw = serde_json::json!([
            {"repo_id": "zeta", "value": 1},
            {"repo_id": "alpha", "value": 2}
        ]);
        let normalized = normalize_value(&raw, &NormalizeContext { temp_roots: vec![] });
        let repos = normalized
            .as_array()
            .expect("repo array")
            .iter()
            .map(|entry| entry["repo_id"].as_str().expect("repo_id"))
            .collect::<Vec<_>>();
        assert_eq!(repos, vec!["alpha", "zeta"]);
    }
}
