//! `gitpulse_*` is accepted as a legacy alias of every listed `waylode_*` tool
//! while the product transitions from GitPulse to Waylode. These tests hold the
//! alias to the same path as the listed name: same schema, same refusals, same
//! answer, nothing legacy listed by default, and opt-in `WAYLODE_MCP_EXPOSE_LEGACY=1`
//! listing all 55 legacy aliases alongside the 66 canonical tools.

use super::*;

fn request(params: Value) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params,
    }
}

/// The whole response to a `tools/call`, as JSON, for comparing two calls.
fn answer(name: &str, arguments: Value) -> Value {
    let mut era = Era::Unknown;
    let response = process_request(
        request(json!({
            "name": name,
            "arguments": arguments,
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        })),
        &mut era,
    )
    .expect("tools/call is answered");
    serde_json::to_value(&response).expect("response serialises")
}

fn listed() -> Vec<String> {
    tools()
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn an_alias_reads_as_the_listed_name_and_nothing_else_changes() {
    assert_eq!(canonical_tool_name("gitpulse_insights"), "waylode_insights");
    assert_eq!(canonical_tool_name("waylode_insights"), "waylode_insights");
    assert_eq!(canonical_tool_name("devmap_search"), "devmap_search");
    // The bare prefix names no tool; it is not turned into one.
    assert_eq!(canonical_tool_name("gitpulse_"), "gitpulse_");
    assert_eq!(
        canonical_tool_name("GitPulse_insights"),
        "GitPulse_insights"
    );
    assert_eq!(canonical_tool_name(""), "");
}

#[test]
fn every_listed_tool_has_its_alias_and_none_is_listed_by_default() {
    let serial = crate::test_support::env::env_serial();
    let _off = crate::test_support::env::bind_env(&serial)
        .remove(EXPOSE_LEGACY_ENV)
        .remove(LEGACY_EXPOSE_LEGACY_ENV);

    let names = listed();
    assert_eq!(names.len(), 66, "expected 55 waylode_* + 11 devmap_*");
    let own: Vec<&String> = names
        .iter()
        .filter(|n| n.starts_with(TOOL_PREFIX))
        .collect();
    assert_eq!(own.len(), 55, "expected 55 waylode_* tools");
    for name in &names {
        assert!(
            !name.starts_with(TOOL_ALIAS_PREFIX),
            "{name} is listed under the legacy alias prefix by default"
        );
    }
    for name in own {
        let alias = format!("{TOOL_ALIAS_PREFIX}{}", &name[TOOL_PREFIX.len()..]);
        assert_eq!(canonical_tool_name(&alias), name.as_str(), "{alias}");
    }
}

#[test]
fn expose_legacy_lists_all_55_deprecated_aliases_beside_canonical_66() {
    let serial = crate::test_support::env::env_serial();
    let guard = crate::test_support::env::bind_env(&serial);
    let _on = guard.set(EXPOSE_LEGACY_ENV, "1");

    let names = listed();
    assert_eq!(names.len(), 121, "expected 66 canonical + 55 legacy tools");
    assert!(names.contains(&"waylode_insights".to_string()));
    assert!(names.contains(&"gitpulse_insights".to_string()));
    assert!(names.contains(&"devmap_search".to_string()));
    let legacy_entry = tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_insights")
        .expect("gitpulse_insights exposed");
    assert!(
        legacy_entry["description"]
            .as_str()
            .unwrap_or_default()
            .contains("DEPRECATED"),
        "{legacy_entry}"
    );
}

#[test]
fn an_aliased_call_meets_the_same_schema_refusal_as_the_listed_name() {
    // Unknown keys and a missing required argument are refused by the schema
    // before any handler runs; the alias must not skip that step.
    for (listed, alias) in [
        ("waylode_insights", "gitpulse_insights"),
        ("waylode_worktree_prune", "gitpulse_worktree_prune"),
        ("waylode_add_task", "gitpulse_add_task"),
        ("waylode_delete_task", "gitpulse_delete_task"),
    ] {
        let probe = json!({ "alias_probe_unknown_key": true });
        let expected = answer(listed, probe.clone());
        let got = answer(alias, probe);
        assert_eq!(got, expected, "{alias} answered differently from {listed}");
        assert_eq!(
            got["result"]["isError"], true,
            "{alias} was not refused: {got}"
        );
    }
}

#[test]
fn an_aliased_write_on_an_untrusted_repository_is_refused_like_the_listed_name() {
    // A real repository, not yet trusted: the only thing standing between the
    // call and the write is the trust gate. Plain `git init`, not
    // `test_support::git_in`, which trusts every repository it initialises.
    let dir = tempfile::tempdir().unwrap();
    use crate::procguard::LockedSpawn;
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .status_locked()
        .unwrap()
        .success());
    let repo = dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let args = json!({ "repo_path": repo });
    let expected = answer("waylode_worktree_prune", args.clone());
    let got = answer("gitpulse_worktree_prune", args.clone());
    assert_eq!(got, expected);
    assert_eq!(got["result"]["isError"], true, "{got}");

    // Trusting the same repository changes the answer under both names, so the
    // refusal above came from the trust gate and not from something else the
    // alias got wrong.
    crate::test_support::trust_repo(dir.path());
    for name in ["gitpulse_worktree_prune", "waylode_worktree_prune"] {
        let trusted = answer(name, args.clone());
        assert_ne!(
            trusted, expected,
            "{name} is refused the same way once trusted"
        );
    }
}

#[test]
fn an_unknown_alias_is_a_protocol_error_that_names_what_was_sent() {
    let got = answer("gitpulse_no_such_tool", json!({}));
    assert_eq!(got["error"]["code"], -32602, "{got}");
    let message = got["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("gitpulse_no_such_tool"), "{message}");
}
