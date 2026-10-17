//! Params for Manvi's `work.tasks.suggest`, validated before any process I/O.
//!
//! The ranking is Manvi's: it reads the open board and returns which tasks to
//! start next with a reason each. GitPulse only names the ask. The request is
//! checked against the same bounds Manvi's `Suggest` enforces
//! (`manvi/serve/suggest.go`) so a bad one is refused here, with a message the
//! board can show, instead of costing a worker call. The bounds are Go's
//! exactly: `limit` 0 is accepted (Manvi reads it as its default of 5), 1 to 20
//! is a count, anything above 20 or negative is refused; `request_id` and
//! `repository_id` are at most 128 bytes and may be empty (an empty
//! `repository_id` is no scope); `max_priority` is 0 to 3.

use super::WorkbenchError;
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Manvi's `suggestMaxLimit`.
const MAX_LIMIT: u64 = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuggestInput {
    request_id: Option<String>,
    limit: Option<u64>,
    max_priority: Option<u64>,
    repository_id: Option<String>,
}

fn invalid(message: &str) -> WorkbenchError {
    WorkbenchError::new("invalid_input", message)
}

fn short_text(value: &str) -> bool {
    value.len() <= 128
}

pub(crate) fn params(input: &str) -> Result<Value, WorkbenchError> {
    if input.len() > 4096 || !input.trim_start().starts_with('{') {
        return Err(invalid(
            "A suggestion request is a JSON object under 4 KiB.",
        ));
    }
    let parsed: SuggestInput = serde_json::from_str(input).map_err(|e| invalid(&e.to_string()))?;
    let mut out = Map::new();
    if let Some(id) = parsed.request_id {
        if !short_text(&id) {
            return Err(invalid("request_id is at most 128 bytes."));
        }
        out.insert("request_id".into(), json!(id));
    }
    if let Some(id) = parsed.repository_id {
        if !short_text(&id) {
            return Err(invalid("repository_id is at most 128 bytes."));
        }
        out.insert("repository_id".into(), json!(id));
    }
    if let Some(limit) = parsed.limit {
        if limit > MAX_LIMIT {
            return Err(invalid("limit is 1 to 20 (0 means the default)."));
        }
        out.insert("limit".into(), json!(limit));
    }
    if let Some(priority) = parsed.max_priority {
        if priority > 3 {
            return Err(invalid("max_priority is 0 (urgent) to 3 (low)."));
        }
        out.insert("max_priority".into(), json!(priority));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_what_was_asked_and_nothing_else() {
        assert_eq!(params("{}").unwrap(), json!({}));
        assert_eq!(
            params(r#"{"limit":3,"max_priority":1,"repository_id":"r1","request_id":"q"}"#)
                .unwrap(),
            json!({"limit":3,"max_priority":1,"repository_id":"r1","request_id":"q"})
        );
    }

    #[test]
    fn accepts_what_the_ranking_accepts() {
        // Manvi reads limit 0 as its default and an empty id as absent.
        assert_eq!(
            params(r#"{"limit":0,"request_id":"","repository_id":""}"#).unwrap(),
            json!({"limit":0,"request_id":"","repository_id":""})
        );
        assert_eq!(params(r#"{"limit":20}"#).unwrap(), json!({"limit":20}));
        let edge = format!(r#"{{"request_id":"{}"}}"#, "x".repeat(128));
        assert!(params(&edge).is_ok());
    }

    #[test]
    fn refuses_what_the_ranking_would_refuse() {
        for bad in [
            r#"{"limit":21}"#,
            r#"{"limit":-1}"#,
            r#"{"max_priority":4}"#,
            r#"{"repository_id":"x"}x"#,
            r#"{"unknown":1}"#,
            "[]",
            "",
        ] {
            assert_eq!(params(bad).unwrap_err().code, "invalid_input", "{bad}");
        }
        let long = format!(r#"{{"request_id":"{}"}}"#, "x".repeat(129));
        assert_eq!(params(&long).unwrap_err().code, "invalid_input");
        let huge = format!(r#"{{"repository_id":"{}"}}"#, "x".repeat(5000));
        assert_eq!(params(&huge).unwrap_err().code, "invalid_input");
    }
}
