//! The `prompts` capability: the control plane's workflows, as templates a
//! user can pick.
//!
//! Prompts are *user-controlled*
//! ([prompts](https://modelcontextprotocol.io/specification/2026-07-28/server/prompts)) —
//! a host typically surfaces them as slash commands. Each one here answers a
//! question an agent working in a shared checkout actually has to ask before it
//! edits anything, and each resolves against live repository state rather than
//! emitting a static instruction the model then has to go and satisfy.
//!
//! Every prompt embeds the state it describes as a `resource` content block, so
//! the model reads the same document `resources/read` would return and the host
//! can show the user exactly what was injected.

use serde_json::{json, Value};

use super::uri;
use super::validate::Violation;

struct Argument {
    name: &'static str,
    description: &'static str,
    required: bool,
}

struct Prompt {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    arguments: &'static [Argument],
    /// Repository facets embedded when the prompt is resolved, in order.
    facets: &'static [&'static str],
}

const REPO_ARG: Argument = Argument {
    name: "repo_path",
    description: "Absolute path to the git repository",
    required: true,
};

const CATALOG: &[Prompt] = &[
    Prompt {
        name: "gitpulse_preflight",
        title: "Preflight before editing",
        description: "What is in flight in this repository right now — other worktrees, their uncommitted work, and whether anything overlaps what you are about to touch. Run before the first edit of a session.",
        arguments: &[REPO_ARG],
        facets: &["context", "collisions"],
    },
    Prompt {
        name: "gitpulse_collision_triage",
        title: "Triage overlapping work",
        description: "Every file with uncommitted changes in more than one worktree, with the parties involved, so the overlap can be resolved before it becomes a conflict.",
        arguments: &[REPO_ARG],
        facets: &["collisions"],
    },
    Prompt {
        name: "gitpulse_handoff",
        title: "Hand off this worktree",
        description: "A summary of one worktree's in-flight state — branch, dirty files, parked merge or rebase, bound task — written for the next agent or person to pick it up.",
        arguments: &[REPO_ARG],
        facets: &["context", "changes"],
    },
    Prompt {
        name: "gitpulse_session_brief",
        title: "Brief me on this repository",
        description: "A full snapshot: worktrees, agent sessions, changes, collisions, ledger, and code-graph availability, with unavailable facets called out as unavailable.",
        arguments: &[REPO_ARG],
        facets: &["insights"],
    },
];

pub fn capability() -> Value {
    // No `listChanged`: this catalog is compiled in and cannot change while the
    // process runs, so a client that subscribed would wait forever.
    json!({})
}

pub fn list() -> Vec<Value> {
    CATALOG
        .iter()
        .map(|prompt| {
            json!({
                "name": prompt.name,
                "title": prompt.title,
                "description": prompt.description,
                "arguments": prompt.arguments.iter().map(|arg| json!({
                    "name": arg.name,
                    "description": arg.description,
                    "required": arg.required,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

pub fn names() -> Vec<&'static str> {
    CATALOG.iter().map(|p| p.name).collect()
}

/// Argument names a prompt accepts, for `completion/complete`.
pub fn argument_names(prompt: &str) -> Vec<&'static str> {
    CATALOG
        .iter()
        .find(|p| p.name == prompt)
        .map(|p| p.arguments.iter().map(|a| a.name).collect())
        .unwrap_or_default()
}

#[derive(Debug)]
pub enum PromptError {
    /// `-32602`: an unknown name or a missing argument. Both are the client's
    /// to fix, so both are invalid params rather than an internal error.
    InvalidParams(String),
}

impl PromptError {
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidParams(m) => m,
        }
    }
}

/// Resolve one prompt against live repository state.
pub fn get(name: &str, arguments: &Value) -> Result<Value, PromptError> {
    let prompt = CATALOG.iter().find(|p| p.name == name).ok_or_else(|| {
        PromptError::InvalidParams(format!(
            "no prompt named {name:?}; known: [{}]",
            names().join(", ")
        ))
    })?;

    // Prompt arguments are always strings on the wire, so this is the same
    // required/unknown check the tool schemas get, expressed against the
    // catalog rather than a JSON Schema.
    let mut violations: Vec<Violation> = Vec::new();
    let empty = serde_json::Map::new();
    let provided = arguments.as_object().unwrap_or(&empty);
    for argument in prompt.arguments {
        match provided.get(argument.name) {
            Some(Value::String(value)) if !value.trim().is_empty() => {}
            Some(Value::String(_)) | None | Some(Value::Null) if argument.required => {
                violations.push(Violation {
                    path: argument.name.to_string(),
                    message: "required argument is missing or empty".into(),
                });
            }
            Some(value) if !value.is_string() && !value.is_null() => violations.push(Violation {
                path: argument.name.to_string(),
                message: format!("must be a string, got {value}"),
            }),
            _ => {}
        }
    }
    for name in provided.keys() {
        if !prompt.arguments.iter().any(|a| a.name == name) {
            violations.push(Violation {
                path: name.clone(),
                message: format!(
                    "unknown argument; accepted: [{}]",
                    argument_names(prompt.name).join(", ")
                ),
            });
        }
    }
    if !violations.is_empty() {
        return Err(PromptError::InvalidParams(
            violations
                .iter()
                .map(Violation::render)
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }

    let repo = provided["repo_path"].as_str().unwrap_or_default();
    let mut content = vec![json!({
        "type": "text",
        "text": format!("{}\n\nRepository: {repo}\n\nThe blocks below are live GitPulse state, read at the moment this prompt was resolved. A facet marked `\"ok\": false` was not scanned — treat it as unknown, never as clean.", prompt.description),
    })];

    for facet in prompt.facets {
        let target = uri::repo_uri(facet, repo);
        // A facet that could not be read is embedded as an explicit failure
        // rather than dropped: a prompt that silently omits the collision block
        // reads exactly like one that found no collisions.
        let block = match super::resources::read(&target) {
            Ok(contents) => contents.into_iter().next().unwrap_or_else(|| {
                json!({
                    "uri": target,
                    "mimeType": "application/json",
                    "text": json!({ "ok": false, "error": "facet returned no content" }).to_string(),
                })
            }),
            Err(error) => json!({
                "uri": target,
                "mimeType": "application/json",
                "text": json!({
                    "ok": false,
                    "facet": facet,
                    "error": error.message(),
                }).to_string(),
            }),
        };
        content.push(json!({ "type": "resource", "resource": block }));
    }

    Ok(json!({
        "description": prompt.description,
        "messages": [{ "role": "user", "content": content }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advertised_prompt_resolves_and_names_its_facets() {
        for prompt in CATALOG {
            assert!(!prompt.facets.is_empty(), "{} embeds nothing", prompt.name);
            for facet in prompt.facets {
                assert!(
                    super::super::resources::facet_names().contains(facet),
                    "{} embeds unknown facet {facet}",
                    prompt.name
                );
            }
        }
    }

    #[test]
    fn a_resolved_prompt_carries_one_block_per_facet_plus_the_instruction() {
        let resolved = get(
            "gitpulse_preflight",
            &json!({ "repo_path": "/no/such/repo" }),
        )
        .expect("resolves even when the repository does not");
        let content = resolved["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "resource");
        assert_eq!(content[2]["type"], "resource");
        assert_eq!(resolved["messages"][0]["role"], "user");
    }

    #[test]
    fn a_facet_that_could_not_be_read_is_embedded_as_a_failure_not_dropped() {
        // The honesty case: a prompt that quietly omitted the collision block
        // would be indistinguishable from one that found no collisions.
        let resolved = get(
            "gitpulse_collision_triage",
            &json!({ "repo_path": "/no/such/repo" }),
        )
        .expect("resolves");
        let content = resolved["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "the failed facet was dropped");
        let text = content[1]["resource"]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).expect("block is JSON");
        assert_eq!(parsed["ok"], false, "{text}");
    }

    #[test]
    fn the_instruction_tells_the_model_how_to_read_an_unavailable_facet() {
        let resolved = get("gitpulse_session_brief", &json!({ "repo_path": "/tmp" })).unwrap();
        let text = resolved["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("never as clean"), "{text}");
    }

    #[test]
    fn an_unknown_prompt_names_the_ones_that_exist() {
        let error = get("nope", &json!({})).expect_err("unknown prompt");
        assert!(error.message().contains("gitpulse_preflight"));
    }

    #[test]
    fn a_missing_or_empty_required_argument_is_invalid_params() {
        for arguments in [
            json!({}),
            json!({ "repo_path": "" }),
            json!({ "repo_path": "   " }),
        ] {
            let error = get("gitpulse_preflight", &arguments).expect_err("{arguments}");
            assert!(error.message().contains("repo_path"), "{}", error.message());
        }
    }

    #[test]
    fn a_non_string_argument_is_refused_rather_than_stringified() {
        let error = get("gitpulse_preflight", &json!({ "repo_path": 7 })).expect_err("number");
        assert!(error.message().contains("must be a string"));
    }

    #[test]
    fn an_unknown_argument_is_refused_and_the_accepted_ones_named() {
        let error = get(
            "gitpulse_preflight",
            &json!({ "repo_path": "/tmp", "repoo_path": "/tmp" }),
        )
        .expect_err("typo");
        assert!(error.message().contains("repoo_path"));
        assert!(error.message().contains("accepted: [repo_path]"));
    }

    #[test]
    fn prompt_names_are_unique_and_namespaced() {
        let mut seen = std::collections::BTreeSet::new();
        for name in names() {
            assert!(name.starts_with("gitpulse_"), "{name} is not namespaced");
            assert!(seen.insert(name), "{name} is listed twice");
        }
    }

    #[test]
    fn listed_arguments_match_what_get_actually_enforces() {
        // The drift this catches: an argument advertised as required that the
        // resolver never checks, which a client discovers as a confusing empty
        // result rather than an error.
        for prompt in list() {
            let name = prompt["name"].as_str().unwrap();
            for argument in prompt["arguments"].as_array().unwrap() {
                if argument["required"] == json!(true) {
                    let error = get(name, &json!({})).expect_err("{name} with no arguments");
                    assert!(
                        error.message().contains(argument["name"].as_str().unwrap()),
                        "{name} advertises {} as required but does not enforce it",
                        argument["name"]
                    );
                }
            }
        }
    }
}
