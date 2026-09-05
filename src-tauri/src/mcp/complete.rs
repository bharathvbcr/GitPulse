//! The `completions` capability: argument autocompletion for prompts and
//! resource templates.
//!
//! [Completion](https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/completion)
//! caps a response at 100 values and asks for relevance ordering.
//!
//! Only `repo_path` is completable, and the suggestions are the repository this
//! server process is running inside plus that repository's worktrees. That is a
//! deliberately narrow source: GitPulse keeps no registry of repositories a user
//! has opened, and a completion list assembled by scanning the disk would be
//! both slow and a directory-listing oracle for anything that can reach this
//! server. Offering nothing is the honest answer where there is nothing to
//! offer; it is never confused with a failure, which is a JSON-RPC error.

use serde_json::{json, Value};

/// Spec ceiling. Values beyond it are counted in `total` and flagged by
/// `hasMore`, never dropped silently.
pub const MAX_VALUES: usize = 100;

pub fn capability() -> Value {
    json!({})
}

#[derive(Debug)]
pub enum CompleteError {
    InvalidParams(String),
}

impl CompleteError {
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidParams(m) => m,
        }
    }
}

/// Answer one `completion/complete`.
pub fn complete(params: &Value) -> Result<Value, CompleteError> {
    let reference = params.get("ref").ok_or_else(|| {
        CompleteError::InvalidParams("missing ref: expected ref/prompt or ref/resource".into())
    })?;
    let argument = params.get("argument").ok_or_else(|| {
        CompleteError::InvalidParams("missing argument: expected { name, value }".into())
    })?;
    let argument_name = argument
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| CompleteError::InvalidParams("argument.name must be a string".into()))?;
    // An absent value is an empty prefix — the very first keystroke, where a
    // client asks for everything.
    let prefix = argument.get("value").and_then(Value::as_str).unwrap_or("");

    let accepted = match reference.get("type").and_then(Value::as_str) {
        Some("ref/prompt") => {
            let name = reference
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| CompleteError::InvalidParams("ref.name must be a string".into()))?;
            let arguments = super::prompts::argument_names(name);
            if arguments.is_empty() {
                return Err(CompleteError::InvalidParams(format!(
                    "no prompt named {name:?}; known: [{}]",
                    super::prompts::names().join(", ")
                )));
            }
            arguments
        }
        Some("ref/resource") => {
            let uri = reference
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| CompleteError::InvalidParams("ref.uri must be a string".into()))?;
            template_arguments(uri)?
        }
        Some(other) => {
            return Err(CompleteError::InvalidParams(format!(
                "unknown ref type {other:?}; expected ref/prompt or ref/resource"
            )))
        }
        None => {
            return Err(CompleteError::InvalidParams(
                "ref.type must be a string".into(),
            ))
        }
    };

    // An argument the reference does not have is the client's mistake, and
    // answering it with an empty list would look like "nothing matches".
    if !accepted.contains(&argument_name) {
        return Err(CompleteError::InvalidParams(format!(
            "no argument named {argument_name:?} on that reference; accepted: [{}]",
            accepted.join(", ")
        )));
    }

    let matches: Vec<String> = match argument_name {
        "repo_path" => repo_path_suggestions(prefix),
        // Reachable only if a prompt gains an argument with no source here; an
        // empty list is the truthful answer, not an error.
        _ => Vec::new(),
    };

    let total = matches.len();
    let values: Vec<String> = matches.into_iter().take(MAX_VALUES).collect();
    Ok(json!({
        "completion": {
            "values": values,
            "total": total,
            "hasMore": total > MAX_VALUES,
        }
    }))
}

/// The arguments a resource template exposes, or an error naming the templates
/// that exist.
fn template_arguments(uri: &str) -> Result<Vec<&'static str>, CompleteError> {
    // A client completing a template sends the template itself
    // (`gitpulse://insights{+repo_path}`), while one completing a partially
    // typed URI sends the concrete form (`gitpulse://insights/Users/...`).
    // The facet is what precedes the first `/` or `{` in either.
    let facet = uri
        .strip_prefix(super::uri::SCHEME)
        .map(|rest| rest.split(['/', '{']).next().unwrap_or_default())
        .unwrap_or_default();
    if super::resources::facet_names().contains(&facet) {
        Ok(vec!["repo_path"])
    } else {
        Err(CompleteError::InvalidParams(format!(
            "no resource template for {uri:?}; templates are {}<facet>{{+repo_path}} with facet in [{}]",
            super::uri::SCHEME,
            super::resources::facet_names().join(", ")
        )))
    }
}

/// How long a worktree listing is reused for completion.
///
/// Completion fires per keystroke. Spawning `git worktree list` each time
/// measured at 12.4 ms p50 — three orders of magnitude past the ~µs the rest of
/// the protocol layer costs, and squarely in the range a user feels while
/// typing. A short window collapses a whole typing burst into one spawn while
/// staying far below the lifetime of a worktree, so a newly added one shows up
/// on the next word rather than the next session.
const SUGGESTION_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// The worktree paths this process last saw, and when.
fn cached_worktrees() -> Vec<String> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);

    // A poisoned lock means a previous caller panicked mid-refresh; completion
    // is a convenience, so it degrades to "no suggestions" rather than
    // propagating. It never degrades to a *wrong* list.
    let Ok(mut cache) = CACHE.lock() else {
        return Vec::new();
    };
    if let Some((at, paths)) = cache.as_ref() {
        if at.elapsed() < SUGGESTION_TTL {
            return paths.clone();
        }
    }
    let fresh = scan_worktrees();
    *cache = Some((Instant::now(), fresh.clone()));
    fresh
}

fn scan_worktrees() -> Vec<String> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    let Some(root) = cwd.to_str() else {
        return Vec::new();
    };
    match crate::engine::worktree::list_worktrees(root) {
        Ok(worktrees) => worktrees.into_iter().map(|w| w.path).collect(),
        // Not a repository, or the scan failed. Either way there is nothing to
        // suggest; the client sees an empty list, which is what it would see
        // from a repository with no worktrees.
        Err(_) => Vec::new(),
    }
}

/// Repository paths worth suggesting: the repository containing this process's
/// working directory, and every worktree attached to it.
fn repo_path_suggestions(prefix: &str) -> Vec<String> {
    let mut found = cached_worktrees();
    found.sort();
    found.dedup();

    let needle = prefix.to_lowercase();
    let mut ranked: Vec<String> = found
        .into_iter()
        .filter(|path| path.to_lowercase().contains(&needle))
        .collect();
    // Prefix matches before interior matches, then shortest first: the
    // repository root outranks its own worktrees, which is the common answer.
    ranked.sort_by_key(|path| {
        let starts = !path.to_lowercase().starts_with(&needle);
        (starts, path.len(), path.clone())
    });
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(params: Value) -> Value {
        complete(&params).expect("completes")
    }

    #[test]
    fn a_prompt_argument_completes_with_a_bounded_well_formed_result() {
        let result = ok(json!({
            "ref": { "type": "ref/prompt", "name": "gitpulse_preflight" },
            "argument": { "name": "repo_path", "value": "" }
        }));
        let completion = &result["completion"];
        let values = completion["values"].as_array().unwrap();
        assert!(values.len() <= MAX_VALUES);
        assert!(completion["total"].is_number());
        assert!(completion["hasMore"].is_boolean());
        assert_eq!(
            completion["hasMore"],
            json!(completion["total"].as_u64().unwrap() as usize > MAX_VALUES)
        );
        for value in values {
            assert!(value.is_string());
        }
    }

    #[test]
    fn a_resource_template_reference_completes_the_same_argument() {
        let result = ok(json!({
            "ref": { "type": "ref/resource", "uri": "gitpulse://insights{+repo_path}" },
            "argument": { "name": "repo_path", "value": "" }
        }));
        assert!(result["completion"]["values"].is_array());
    }

    #[test]
    fn an_absent_argument_value_is_an_empty_prefix_not_an_error() {
        let result = ok(json!({
            "ref": { "type": "ref/prompt", "name": "gitpulse_preflight" },
            "argument": { "name": "repo_path" }
        }));
        assert!(result["completion"]["values"].is_array());
    }

    #[test]
    fn an_unknown_prompt_is_invalid_params_and_names_the_real_ones() {
        let error = complete(&json!({
            "ref": { "type": "ref/prompt", "name": "nope" },
            "argument": { "name": "repo_path", "value": "" }
        }))
        .expect_err("unknown prompt");
        assert!(error.message().contains("gitpulse_preflight"));
    }

    #[test]
    fn an_unknown_template_is_invalid_params_and_names_the_facets() {
        let error = complete(&json!({
            "ref": { "type": "ref/resource", "uri": "gitpulse://nope{+repo_path}" },
            "argument": { "name": "repo_path", "value": "" }
        }))
        .expect_err("unknown template");
        assert!(error.message().contains("insights"));
    }

    #[test]
    fn an_argument_the_reference_does_not_have_errors_rather_than_returning_nothing() {
        // An empty list here would read as "no matches" for an argument that
        // does not exist at all.
        let error = complete(&json!({
            "ref": { "type": "ref/prompt", "name": "gitpulse_preflight" },
            "argument": { "name": "not_an_argument", "value": "" }
        }))
        .expect_err("unknown argument");
        assert!(error.message().contains("accepted: [repo_path]"));
    }

    #[test]
    fn malformed_requests_say_which_part_is_missing() {
        for (params, needle) in [
            (json!({}), "missing ref"),
            (
                json!({ "ref": { "type": "ref/prompt", "name": "gitpulse_preflight" } }),
                "missing argument",
            ),
            (
                json!({ "ref": { "type": "ref/other" }, "argument": { "name": "x" } }),
                "unknown ref type",
            ),
            (
                json!({ "ref": {}, "argument": { "name": "x" } }),
                "ref.type",
            ),
            (
                json!({ "ref": { "type": "ref/prompt" }, "argument": { "name": "x" } }),
                "ref.name",
            ),
            (
                json!({ "ref": { "type": "ref/resource" }, "argument": { "name": "x" } }),
                "ref.uri",
            ),
            (
                json!({ "ref": { "type": "ref/prompt", "name": "gitpulse_preflight" }, "argument": { "value": "x" } }),
                "argument.name",
            ),
        ] {
            let error = complete(&params).expect_err("malformed");
            assert!(
                error.message().contains(needle),
                "expected {needle:?} in {:?}",
                error.message()
            );
        }
    }

    #[test]
    fn suggestions_are_absolute_paths_that_the_resource_uri_round_trips() {
        // Whatever this machine offers must be usable verbatim as `repo_path`.
        for value in repo_path_suggestions("") {
            let target = super::super::uri::repo_uri("insights", &value);
            match super::super::uri::parse(&target).expect("suggestion makes a valid URI") {
                super::super::uri::Target::Repo { path, .. } => assert_eq!(path, value),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn both_the_template_form_and_a_partly_typed_uri_resolve_to_the_same_facet() {
        // A client may send either. Splitting only on `/` read the whole of
        // `insights{+repo_path}` as the facet name and refused the template.
        for uri in [
            "gitpulse://insights{+repo_path}",
            "gitpulse://insights",
            "gitpulse://insights/Users/me/half-typed",
        ] {
            assert_eq!(template_arguments(uri).unwrap(), vec!["repo_path"], "{uri}");
        }
    }

    #[test]
    fn repeated_completions_do_not_respawn_git_for_every_keystroke() {
        // Completion fires per keystroke. The first call may spawn `git
        // worktree list`; the ones behind it inside the TTL must not, and they
        // must answer identically — a cache that returned a different list
        // mid-word would be worse than the spawn it saved.
        let first = repo_path_suggestions("");
        let started = std::time::Instant::now();
        for _ in 0..50 {
            assert_eq!(repo_path_suggestions(""), first);
        }
        let per_call = started.elapsed() / 50;
        // A spawn is milliseconds; a clone of a short Vec is microseconds. The
        // bound is loose on purpose so a loaded CI machine does not fail this
        // for reasons unrelated to whether a subprocess ran.
        assert!(
            per_call < std::time::Duration::from_millis(2),
            "{per_call:?} per completion — the worktree scan is not being reused"
        );
    }

    #[test]
    fn a_prefix_that_matches_nothing_yields_an_empty_list_not_an_error() {
        let result = ok(json!({
            "ref": { "type": "ref/prompt", "name": "gitpulse_preflight" },
            "argument": { "name": "repo_path", "value": "zzz-no-such-path-zzz" }
        }));
        assert_eq!(result["completion"]["values"], json!([]));
        assert_eq!(result["completion"]["total"], 0);
        assert_eq!(result["completion"]["hasMore"], false);
    }
}
