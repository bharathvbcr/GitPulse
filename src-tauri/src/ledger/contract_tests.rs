//! `contracts/event.schema.json` against what this crate actually serializes.
//!
//! GitPulse owns the ledger event contract, and nothing held the two together:
//! `scripts/event-contract.test.ts` checks Tauri event *names*, not this. The
//! gap was real — the contract described `verdict_json` as a raw
//! verdict.schema.json decision, while the only writer (`harness::record_gate`
//! via `serde_json::to_string(&PolicyVerdict)`) and every reader used the
//! classified projection, so none of the 178 verdicts recorded across three
//! repositories' ledgers by 2026-10-08 matched the contract's description.
//!
//! These tests serialize the real types and validate the output against the
//! vendored schema with a validator for exactly the JSON-Schema keywords the
//! contract uses. A keyword the validator does not know is a test failure, so
//! the contract cannot grow a constraint this silently skips.

use super::{ActorKind, LedgerEvent, Outcome};
use crate::harness::policy::{PolicyStatus, PolicyVerdict};
use serde_json::{json, Value};

const SCHEMA: &str = include_str!("../../../contracts/event.schema.json");

fn schema() -> Value {
    serde_json::from_str(SCHEMA).expect("event.schema.json parses")
}

/// Keywords that annotate rather than constrain.
const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "title",
    "description",
    "$defs",
    "format",
    "contentMediaType",
    "contentSchema",
    "examples",
];

fn type_matches(value: &Value, ty: &str) -> bool {
    match ty {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        other => panic!("schema uses unknown type {other:?}"),
    }
}

/// Validate `value` against `schema`, resolving `#/$defs/…` against `root`.
/// Returns every violation, each prefixed with its JSON path.
fn validate(value: &Value, schema: &Value, root: &Value, at: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(object) = schema.as_object() else {
        return vec![format!("{at}: schema is not an object")];
    };
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        let name = reference
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("unsupported $ref {reference}"));
        let target = &root["$defs"][name];
        assert!(!target.is_null(), "{at}: $ref {reference} resolves to nothing");
        return validate(value, target, root, at);
    }
    for (keyword, rule) in object {
        match keyword.as_str() {
            k if ANNOTATIONS.contains(&k) => {}
            "type" => {
                let types: Vec<&str> = match rule {
                    Value::String(t) => vec![t.as_str()],
                    Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
                    _ => panic!("{at}: malformed type"),
                };
                if !types.iter().any(|t| type_matches(value, t)) {
                    errors.push(format!("{at}: {value} is not {types:?}"));
                }
            }
            "enum" => {
                if !rule.as_array().unwrap().contains(value) {
                    errors.push(format!("{at}: {value} is not one of {rule}"));
                }
            }
            "const" => {
                if value != rule {
                    errors.push(format!("{at}: {value} is not {rule}"));
                }
            }
            "minimum" => {
                if let Some(n) = value.as_f64() {
                    if n < rule.as_f64().unwrap() {
                        errors.push(format!("{at}: {n} is below {rule}"));
                    }
                }
            }
            "pattern" => {
                if let Some(text) = value.as_str() {
                    let re = regex::Regex::new(rule.as_str().unwrap()).unwrap();
                    if !re.is_match(text) {
                        errors.push(format!("{at}: {text:?} does not match {rule}"));
                    }
                }
            }
            "required" => {
                if let Some(fields) = value.as_object() {
                    for name in rule.as_array().unwrap() {
                        if !fields.contains_key(name.as_str().unwrap()) {
                            errors.push(format!("{at}: required {name} is absent"));
                        }
                    }
                }
            }
            "additionalProperties" => {
                assert_eq!(rule, &json!(false), "{at}: only `false` is supported");
                let known = object.get("properties").and_then(Value::as_object);
                if let (Some(fields), Some(known)) = (value.as_object(), known) {
                    for name in fields.keys() {
                        if !known.contains_key(name) {
                            errors.push(format!("{at}: {name} is not in the contract"));
                        }
                    }
                }
            }
            "properties" => {
                if let Some(fields) = value.as_object() {
                    for (name, sub) in rule.as_object().unwrap() {
                        if let Some(field) = fields.get(name) {
                            errors.extend(validate(field, sub, root, &format!("{at}.{name}")));
                        }
                    }
                }
            }
            "items" => {
                if let Some(items) = value.as_array() {
                    for (i, item) in items.iter().enumerate() {
                        errors.extend(validate(item, rule, root, &format!("{at}[{i}]")));
                    }
                }
            }
            other => panic!(
                "{at}: the contract uses `{other}`, which this validator does not check — \
                 teach it the keyword rather than letting a constraint pass unexamined"
            ),
        }
    }
    errors
}

/// Every `PolicyStatus`. The `match` has no wildcard, so a new variant fails
/// to compile here until it is listed — and then has to be in the contract.
fn every_status() -> Vec<PolicyStatus> {
    let all = [
        PolicyStatus::Allowed,
        PolicyStatus::Demoted,
        PolicyStatus::Granted,
        PolicyStatus::Widened,
        PolicyStatus::Degraded,
        PolicyStatus::Warned,
        PolicyStatus::Blocked,
        PolicyStatus::Unchecked,
    ];
    for status in all {
        match status {
            PolicyStatus::Allowed
            | PolicyStatus::Demoted
            | PolicyStatus::Granted
            | PolicyStatus::Widened
            | PolicyStatus::Degraded
            | PolicyStatus::Warned
            | PolicyStatus::Blocked
            | PolicyStatus::Unchecked => {}
        }
    }
    all.to_vec()
}

fn verdict(status: PolicyStatus) -> PolicyVerdict {
    let checked = status != PolicyStatus::Unchecked;
    PolicyVerdict {
        status,
        checked,
        target: "git push --force origin main".into(),
        rule: if checked { "command.force_push".into() } else { String::new() },
        severity: if checked { "soft".into() } else { String::new() },
        reason: "fixture".into(),
        demoted: if status == PolicyStatus::Demoted { "posture".into() } else { String::new() },
        grant_id: if status == PolicyStatus::Granted { "g-1".into() } else { String::new() },
        granted_by: if status == PolicyStatus::Granted { "ada".into() } else { String::new() },
        widened: if status == PolicyStatus::Widened { "src/**".into() } else { String::new() },
        degraded: if status == PolicyStatus::Degraded { vec!["repo_map".into()] } else { vec![] },
        task_id: "TASK-7".into(),
        detail: if checked { String::new() } else { "no manvi binary".into() },
        detail_code: if checked { String::new() } else { "not_installed".into() },
    }
}

fn full_event(verdict_json: Option<String>) -> LedgerEvent {
    LedgerEvent {
        id: 42,
        ulid: "01J9ZQ3V5X7Y8Z9A0B1C2D3E4F".into(),
        ts_utc: "2026-10-08T12:00:00.000Z".into(),
        schema_version: super::SCHEMA_VERSION,
        repo_path: "/repo".into(),
        worktree_path: Some("/repo/.claude/worktrees/a".into()),
        actor_kind: ActorKind::Agent.as_str().into(),
        actor_id: Some("claude-code".into()),
        session_id: Some("s-1".into()),
        task_id: Some("TASK-7".into()),
        action: "git.push".into(),
        object: Some("refs/heads/main".into()),
        argv_json: Some(r#"["git","push"]"#.into()),
        outcome: Outcome::Blocked.as_str().into(),
        verdict_json,
        before_ref: Some("a".repeat(40)),
        after_ref: Some("b".repeat(40)),
        duration_ms: Some(12),
        detail_json: Some(r#"{"phase":"gate"}"#.into()),
    }
}

fn minimal_event() -> LedgerEvent {
    LedgerEvent {
        id: 1,
        ulid: "01J9ZQ3V5X7Y8Z9A0B1C2D3E4F".into(),
        ts_utc: "2026-10-08T12:00:00Z".into(),
        schema_version: super::SCHEMA_VERSION,
        repo_path: "/repo".into(),
        worktree_path: None,
        actor_kind: ActorKind::System.as_str().into(),
        actor_id: None,
        session_id: None,
        task_id: None,
        action: "session.start".into(),
        object: None,
        argv_json: None,
        outcome: Outcome::Ok.as_str().into(),
        verdict_json: None,
        before_ref: None,
        after_ref: None,
        duration_ms: None,
        detail_json: None,
    }
}

#[test]
fn a_full_and_a_minimal_event_conform_to_the_contract() {
    let root = schema();
    for event in [full_event(Some("{}".into())), minimal_event()] {
        let value = serde_json::to_value(&event).unwrap();
        let errors = validate(&value, &root, &root, "event");
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }
}

#[test]
fn every_field_the_contract_names_is_one_this_struct_serializes() {
    let root = schema();
    let contract: std::collections::BTreeSet<&String> =
        root["properties"].as_object().unwrap().keys().collect();
    let value = serde_json::to_value(minimal_event()).unwrap();
    let ours: std::collections::BTreeSet<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(contract, ours, "LedgerEvent and event.schema.json name different fields");
}

#[test]
fn the_actor_and_outcome_vocabularies_are_the_contracts() {
    let root = schema();
    let actors: Vec<Value> = [ActorKind::Human, ActorKind::Agent, ActorKind::System]
        .into_iter()
        .map(|a| json!(a.as_str()))
        .collect();
    let outcomes: Vec<Value> = [Outcome::Ok, Outcome::Failed, Outcome::Blocked]
        .into_iter()
        .map(|o| json!(o.as_str()))
        .collect();
    assert_eq!(root["properties"]["actor_kind"]["enum"], json!(actors));
    assert_eq!(root["properties"]["outcome"]["enum"], json!(outcomes));
    assert_eq!(
        root["properties"]["schema_version"]["const"],
        json!(super::SCHEMA_VERSION)
    );
}

/// The writer serializes `PolicyVerdict` into `verdict_json`; every status it
/// can carry must conform to the contract's `contentSchema`.
#[test]
fn every_verdict_the_gate_can_record_conforms_to_the_contract() {
    let root = schema();
    let content = &root["properties"]["verdict_json"]["contentSchema"];
    assert!(
        !content.is_null(),
        "verdict_json must declare the shape of its content (contentSchema)"
    );
    let mut spelled = Vec::new();
    for status in every_status() {
        let text = serde_json::to_string(&verdict(status)).unwrap();
        let errors = validate(
            &serde_json::from_str(&text).unwrap(),
            content,
            &root,
            &format!("verdict_json[{status:?}]"),
        );
        assert!(errors.is_empty(), "{}", errors.join("\n"));
        let event = serde_json::to_value(full_event(Some(text))).unwrap();
        assert!(validate(&event, &root, &root, "event").is_empty());
        spelled.push(serde_json::to_value(status).unwrap());
    }
    assert_eq!(
        root["$defs"]["ledgerVerdict"]["properties"]["status"]["enum"],
        json!(spelled),
        "the contract's status vocabulary must be exactly PolicyStatus, in order"
    );
}

#[test]
fn the_unchecked_constructor_conforms_too() {
    let root = schema();
    let unchecked = PolicyVerdict::unchecked(
        "git commit -m x",
        &crate::harness::HarnessError::NotInstalled("no manvi binary".into()),
    );
    let value = serde_json::to_value(&unchecked).unwrap();
    let errors = validate(
        &value,
        &root["properties"]["verdict_json"]["contentSchema"],
        &root,
        "verdict_json",
    );
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// The validator is the instrument; show it can fail before trusting a pass.
#[test]
fn the_validator_rejects_what_the_contract_forbids() {
    let root = schema();
    let mut bad = serde_json::to_value(minimal_event()).unwrap();
    bad["actor_kind"] = json!("robot");
    bad["schema_version"] = json!(2);
    bad["surprise"] = json!(true);
    bad["ulid"] = json!("not-a-ulid");
    let errors = validate(&bad, &root, &root, "event");
    for needle in ["actor_kind", "schema_version", "surprise", "ulid"] {
        assert!(
            errors.iter().any(|e| e.contains(needle)),
            "{needle} must be reported: {errors:?}"
        );
    }
    // The pre-2026-10-08 description: a raw verdict.schema.json decision.
    let raw_decision = json!({"action": "allow", "rule": "", "severity": "none", "reason": "", "target": "x"});
    let errors = validate(
        &raw_decision,
        &root["properties"]["verdict_json"]["contentSchema"],
        &root,
        "verdict_json",
    );
    assert!(!errors.is_empty(), "a raw decision is not what the ledger records");
}
