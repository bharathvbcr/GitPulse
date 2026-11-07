//! Params for Manvi's `work.tasks.organize`, validated before any process I/O.
//!
//! The proposal is Manvi's: it reads the open board and returns priority
//! changes, a column order and duplicate groups. GitPulse only names the ask.
//! The request is checked against the same bounds Manvi's `Organize` enforces
//! (`manvi/serve/organize.go`) so a bad one is refused here, with a message the
//! board can show, instead of costing a worker call. `request_id` and
//! `repository_id` are at most 128 bytes and may be empty.

use super::WorkbenchError;
use serde::Deserialize;
use serde_json::{json, Map, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrganizeInput {
    request_id: Option<String>,
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
        return Err(invalid("An organise request is a JSON object under 4 KiB."));
    }
    let parsed: OrganizeInput = serde_json::from_str(input).map_err(|e| invalid(&e.to_string()))?;
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
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_what_was_asked() {
        assert_eq!(params("{}").unwrap(), json!({}));
        assert_eq!(
            params(r#"{"repository_id":"r1","request_id":"q"}"#).unwrap(),
            json!({"repository_id":"r1","request_id":"q"})
        );
        assert_eq!(
            params(r#"{"request_id":"","repository_id":""}"#).unwrap(),
            json!({"request_id":"","repository_id":""})
        );
        let edge = format!(r#"{{"request_id":"{}"}}"#, "x".repeat(128));
        assert!(params(&edge).is_ok());
    }

    #[test]
    fn refuses_what_the_proposal_would_refuse() {
        for bad in [r#"{"limit":5}"#, r#"{"unknown":1}"#, "[]", ""] {
            assert_eq!(params(bad).unwrap_err().code, "invalid_input", "{bad}");
        }
        let long = format!(r#"{{"repository_id":"{}"}}"#, "x".repeat(129));
        assert_eq!(params(&long).unwrap_err().code, "invalid_input");
        let huge = format!(r#"{{"request_id":"{}"}}"#, "x".repeat(5000));
        assert_eq!(params(&huge).unwrap_err().code, "invalid_input");
    }
}
