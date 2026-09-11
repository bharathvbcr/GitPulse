//! Argument validation against a declared JSON Schema.
//!
//! MCP 2026-07-28 makes this a server obligation, not a nicety: *"Servers
//! **MUST**: Validate all tool inputs"*
//! ([tools#security-considerations](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)).
//! Before this existed, every tool read its arguments straight out of the
//! `Value` with `as_str()` / `as_u64()`, so the declared schema was decoration:
//! `{"limit": "500"}` produced `None`, which fell back to the default, and the
//! caller was told nothing. A request for 500 answered with 200 and reported as
//! success is the same failure shape as a check that could not run reporting a
//! pass.
//!
//! This is a validator for the schema subset the catalog actually uses —
//! `type`, `required`, `additionalProperties`, `enum`, `minimum`, `maximum`,
//! `minLength`, `maxLength` on a flat object — not a general JSON Schema
//! engine. That is deliberate: [`schema_is_supported`] walks every advertised
//! schema and a test fails the build if the catalog ever grows a keyword this
//! file would silently ignore. An unsupported keyword must never validate as
//! if it had been checked.

use serde_json::Value;

/// One violation, phrased for a language model that has to fix its own call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub message: String,
}

impl Violation {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn render(&self) -> String {
        if self.path.is_empty() {
            self.message.clone()
        } else {
            format!("{}: {}", self.path, self.message)
        }
    }
}

/// Keywords [`validate`] understands. Anything outside this set in an
/// advertised schema is a build failure, not a silent pass.
const SUPPORTED_KEYWORDS: &[&str] = &[
    "type",
    "description",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "items",
];

/// Every keyword in `schema` that [`validate`] would ignore.
///
/// The point is the honesty invariant: a schema carrying `pattern` would look
/// validated while `pattern` was never applied. The catalog test calls this and
/// fails on any non-empty result, so the two can never drift apart.
pub fn unsupported_keywords(schema: &Value) -> Vec<String> {
    let mut found = Vec::new();
    collect_unsupported(schema, "", &mut found);
    found.sort();
    found.dedup();
    found
}

fn collect_unsupported(schema: &Value, path: &str, found: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        return;
    };
    for (key, value) in object {
        if !SUPPORTED_KEYWORDS.contains(&key.as_str()) {
            found.push(if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            });
            continue;
        }
        match key.as_str() {
            "properties" => {
                if let Some(properties) = value.as_object() {
                    for (name, sub) in properties {
                        let child = if path.is_empty() {
                            format!("properties.{name}")
                        } else {
                            format!("{path}.properties.{name}")
                        };
                        collect_unsupported(sub, &child, found);
                    }
                }
            }
            "items" => {
                let child = if path.is_empty() {
                    "items".to_string()
                } else {
                    format!("{path}.items")
                };
                collect_unsupported(value, &child, found);
            }
            _ => {}
        }
    }
}

/// Check `arguments` against `schema`, returning every violation at once.
///
/// All violations rather than the first: a model that fixes one argument per
/// round trip burns a turn per mistake.
pub fn validate(arguments: &Value, schema: &Value) -> Vec<Violation> {
    let mut violations = Vec::new();
    check(arguments, schema, "", &mut violations);
    violations
}

fn check(value: &Value, schema: &Value, path: &str, out: &mut Vec<Violation>) {
    let Some(schema) = schema.as_object() else {
        return;
    };

    if let Some(expected) = schema.get("type") {
        let matches = match expected {
            Value::String(name) => type_matches(value, name),
            Value::Array(names) if !names.is_empty() => {
                names.iter().try_fold(false, |matched, name| {
                    type_matches(value, name.as_str()?).map(|member| matched || member)
                })
            }
            _ => None,
        };
        if matches != Some(true) {
            out.push(Violation::new(
                path,
                format!(
                    "expected {}, got {}",
                    render_scalar(expected),
                    describe(value)
                ),
            ));
            // Every other keyword is typed; checking them against the wrong
            // kind of value would only produce noise on top of the real cause.
            return;
        }
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            let rendered: Vec<String> = allowed.iter().map(render_scalar).collect();
            out.push(Violation::new(
                path,
                format!("must be one of [{}]", rendered.join(", ")),
            ));
        }
    }

    match value {
        Value::Object(fields) => {
            let properties = schema.get("properties").and_then(Value::as_object);

            for required in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                // An explicit JSON `null` is absence, not a value: every
                // argument in this catalog is a string or a number.
                match fields.get(required) {
                    None | Some(Value::Null) => out.push(Violation::new(
                        join(path, required),
                        "required argument is missing",
                    )),
                    Some(_) => {}
                }
            }

            let closed = schema
                .get("additionalProperties")
                .and_then(Value::as_bool)
                .is_some_and(|open| !open);
            if closed {
                for name in fields.keys() {
                    if properties.is_none_or(|p| !p.contains_key(name)) {
                        let known: Vec<&str> = properties
                            .map(|p| p.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        out.push(Violation::new(
                            join(path, name),
                            format!("unknown argument; accepted: [{}]", known.join(", ")),
                        ));
                    }
                }
            }

            if let Some(properties) = properties {
                for (name, sub) in properties {
                    match fields.get(name) {
                        // Absent optional arguments are the normal case, and an
                        // explicit null is treated as absence to match the
                        // required check above.
                        None | Some(Value::Null) => {}
                        Some(present) => check(present, sub, &join(path, name), out),
                    }
                }
            }
        }
        Value::Array(items) => {
            let count = items.len() as u64;
            if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
                if count < min {
                    out.push(Violation::new(
                        path,
                        format!("must have at least {min} items, got {count}"),
                    ));
                }
            }
            if let Some(max) = schema.get("maxItems").and_then(Value::as_u64) {
                if count > max {
                    out.push(Violation::new(
                        path,
                        format!("must have at most {max} items, got {count}"),
                    ));
                }
            }
            if let Some(sub) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    check(item, sub, &format!("{path}[{index}]"), out);
                }
            }
        }
        Value::String(text) => {
            let len = text.chars().count() as u64;
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
                if len < min {
                    out.push(Violation::new(
                        path,
                        format!("must be at least {min} characters, got {len}"),
                    ));
                }
            }
            if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
                if len > max {
                    out.push(Violation::new(
                        path,
                        format!("must be at most {max} characters, got {len}"),
                    ));
                }
            }
        }
        Value::Number(_) => {
            // `as_f64` is the only accessor that answers for every JSON number
            // shape; the bounds in this catalog are small integers, so the
            // f64 round trip cannot lose a digit that matters here.
            if let Some(actual) = value.as_f64() {
                if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
                    if actual < min {
                        out.push(Violation::new(path, format!("must be >= {min}")));
                    }
                }
                if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
                    if actual > max {
                        out.push(Violation::new(path, format!("must be <= {max}")));
                    }
                }
            }
        }
        _ => {}
    }
}

/// JSON Schema `type` against a concrete value.
///
/// `integer` is the one that has to be spelled out: JSON has a single number
/// type, so `1.0` is an integer and `1.5` is not, and serde_json's `is_i64`
/// answers "was it written without a decimal point", which is a different
/// question.
fn type_matches(value: &Value, expected: &str) -> Option<bool> {
    Some(match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value
            .as_f64()
            .is_some_and(|n| n.fract() == 0.0 && n.is_finite()),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        // An unknown type keyword must not silently pass. `unsupported_keywords`
        // cannot catch this (the keyword itself is supported, its value is not),
        // so refusing here is what keeps it honest.
        _ => return None,
    })
}

fn describe(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Number(n) => {
            if n.as_f64().is_some_and(|f| f.fract() == 0.0) {
                "integer".into()
            } else {
                "number".into()
            }
        }
        Value::String(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(_) => "object".into(),
    }
}

fn render_scalar(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}.{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo_path": { "type": "string", "minLength": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500 },
                "mode": { "type": "string", "enum": ["fast", "full"] }
            },
            "required": ["repo_path"],
            "additionalProperties": false
        })
    }

    fn messages(arguments: Value) -> Vec<String> {
        validate(&arguments, &schema())
            .into_iter()
            .map(|v| v.render())
            .collect()
    }

    #[test]
    fn a_valid_call_has_no_violations() {
        assert!(messages(json!({ "repo_path": "/tmp/x", "limit": 10, "mode": "fast" })).is_empty());
    }

    #[test]
    fn nullable_types_accept_only_the_declared_alternatives() {
        let schema = json!({"type": ["boolean", "null"]});
        for value in [json!(true), json!(false), Value::Null] {
            assert!(validate(&value, &schema).is_empty());
        }
        for value in [json!("true"), json!(1), json!([]), json!({})] {
            assert_eq!(validate(&value, &schema).len(), 1, "{value}");
        }
    }

    #[test]
    fn malformed_type_declarations_never_disable_validation() {
        for declaration in [
            json!([]),
            json!(42),
            Value::Null,
            json!(["string", 42]),
            json!(["strong"]),
            json!(["string", "strong"]),
        ] {
            assert!(!validate(&json!("x"), &json!({"type": declaration})).is_empty());
        }
    }

    #[test]
    fn a_missing_required_argument_is_named() {
        let found = messages(json!({ "limit": 10 }));
        assert_eq!(found, vec!["repo_path: required argument is missing"]);
    }

    #[test]
    fn a_string_where_a_number_belongs_is_refused_not_defaulted() {
        // The regression this whole module exists for: `"500"` used to reach
        // `as_u64()`, return None, and silently become the default.
        let found = messages(json!({ "repo_path": "/tmp/x", "limit": "500" }));
        assert_eq!(found, vec!["limit: expected integer, got string"]);
    }

    #[test]
    fn a_fractional_number_is_not_an_integer() {
        let found = messages(json!({ "repo_path": "/tmp/x", "limit": 1.5 }));
        assert_eq!(found, vec!["limit: expected integer, got number"]);
    }

    #[test]
    fn a_whole_float_is_an_integer() {
        assert!(messages(json!({ "repo_path": "/tmp/x", "limit": 10.0 })).is_empty());
    }

    #[test]
    fn bounds_are_enforced_in_both_directions() {
        assert_eq!(
            messages(json!({ "repo_path": "/tmp/x", "limit": 0 })),
            vec!["limit: must be >= 1"]
        );
        assert_eq!(
            messages(json!({ "repo_path": "/tmp/x", "limit": 100_000 })),
            vec!["limit: must be <= 500"]
        );
        // Negative used to be indistinguishable from "not provided".
        assert_eq!(
            messages(json!({ "repo_path": "/tmp/x", "limit": -1 })),
            vec!["limit: must be >= 1"]
        );
    }

    #[test]
    fn an_empty_string_fails_min_length() {
        assert_eq!(
            messages(json!({ "repo_path": "" })),
            vec!["repo_path: must be at least 1 characters, got 0"]
        );
    }

    #[test]
    fn a_closed_schema_names_the_arguments_it_accepts() {
        let found = messages(json!({ "repo_path": "/tmp/x", "reppo_path": "/tmp/y" }));
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("reppo_path: unknown argument; accepted: ["));
        assert!(found[0].contains("repo_path"));
    }

    #[test]
    fn enum_violations_list_the_allowed_values() {
        assert_eq!(
            messages(json!({ "repo_path": "/tmp/x", "mode": "turbo" })),
            vec!["mode: must be one of [fast, full]"]
        );
    }

    #[test]
    fn every_violation_is_reported_not_just_the_first() {
        let found = messages(json!({ "limit": "x", "mode": "turbo" }));
        assert_eq!(found.len(), 3, "{found:?}");
    }

    #[test]
    fn an_explicit_null_is_absence_for_required_and_optional_alike() {
        assert_eq!(
            messages(json!({ "repo_path": null })),
            vec!["repo_path: required argument is missing"]
        );
        assert!(messages(json!({ "repo_path": "/tmp/x", "limit": null })).is_empty());
    }

    #[test]
    fn a_non_object_argument_bag_is_a_type_violation() {
        let found = validate(&json!([1, 2, 3]), &schema());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].render(), "expected object, got array");
    }

    #[test]
    fn an_unknown_type_keyword_refuses_rather_than_passing() {
        // A typo in the catalog must fail closed. Nothing else in this file can
        // catch it: `type` is a supported keyword, so only the match arm can.
        let found = validate(&json!("x"), &json!({ "type": "strong" }));
        assert_eq!(found.len(), 1);
        assert!(found[0].render().contains("expected strong"));
    }

    #[test]
    fn unsupported_keywords_are_reported_rather_than_ignored() {
        let found = unsupported_keywords(&json!({
            "type": "object",
            "properties": { "a": { "type": "string", "pattern": "^x" } },
            "oneOf": []
        }));
        assert_eq!(found, vec!["oneOf", "properties.a.pattern"]);
    }

    #[test]
    fn a_fully_supported_schema_reports_nothing_unsupported() {
        assert!(unsupported_keywords(&schema()).is_empty());
    }
}
