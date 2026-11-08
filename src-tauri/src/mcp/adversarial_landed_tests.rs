//! Adversarial review of card 15 (S3-11 + S6-06), MCP half: the live
//! `gitpulse_landed` and `gitpulse_provenance` payloads against their
//! advertised `outputSchema` on every fixture, and the read-only claim.

use super::*;
use crate::procguard::LockedSpawn;
use std::process::Command;

fn git(repo: &str, args: &[&str]) -> String {
    let mut command = Command::new("git");
    crate::test_support::pin_git(&mut command);
    let out = command
        .args(args)
        .current_dir(repo)
        .output_locked()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn repo_with_root() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().to_string_lossy().into_owned();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["config", "user.email", "t@e.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    crate::test_support::trust_repo(dir.path());
    put(&repo, "root.txt", "root\n", "root");
    (dir, repo)
}

fn put(repo: &str, file: &str, content: &str, message: &str) -> String {
    std::fs::write(std::path::Path::new(repo).join(file), content).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

/// `(label, repository, commit, base)` for every landing shape and failure.
#[allow(clippy::type_complexity)]
fn fixtures() -> (
    Vec<tempfile::TempDir>,
    Vec<(String, String, String, Option<String>)>,
) {
    let mut keep = Vec::new();
    let mut cases = Vec::new();
    let mut add =
        |label: &str, dir: tempfile::TempDir, repo: String, commit: String, base: Option<&str>| {
            keep.push(dir);
            cases.push((label.to_string(), repo, commit, base.map(str::to_string)));
        };

    // ff
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    let c = put(&repo, "a.txt", "1\n", "c1");
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["merge", "-q", "--ff-only", "f"]);
    add("ff", dir, repo, c, Some("main"));

    // merge
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    let c = put(&repo, "a.txt", "1\n", "c1");
    git(&repo, &["checkout", "-q", "main"]);
    put(&repo, "m.txt", "m\n", "main moves");
    git(&repo, &["merge", "-q", "--no-ff", "--no-edit", "f"]);
    add("merge", dir, repo, c, Some("main"));

    // squash
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    put(&repo, "a.txt", "1\n", "c1");
    let c = put(&repo, "b.txt", "2\n", "c2");
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["merge", "-q", "--squash", "f"]);
    git(&repo, &["commit", "-q", "-m", "squash"]);
    add("squash", dir, repo, c, Some("main"));

    // rebase
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    put(&repo, "a.txt", "1\n", "c1");
    let c = put(&repo, "b.txt", "2\n", "c2");
    git(&repo, &["checkout", "-q", "main"]);
    put(&repo, "m.txt", "m\n", "main moves");
    git(&repo, &["cherry-pick", "f~1", "f"]);
    add("rebase", dir, repo, c, Some("main"));

    // unmerged
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "-b", "f"]);
    let c = put(&repo, "a.txt", "1\n", "c1");
    git(&repo, &["checkout", "-q", "main"]);
    add("unmerged", dir, repo, c, Some("main"));

    // unrelated history
    let (dir, repo) = repo_with_root();
    git(&repo, &["checkout", "-q", "--orphan", "island"]);
    let c = put(&repo, "i.txt", "i\n", "island");
    git(&repo, &["checkout", "-q", "main"]);
    add("unrelated", dir, repo, c, Some("main"));

    // default base
    let (dir, repo) = repo_with_root();
    let c = git(&repo, &["rev-parse", "HEAD"]);
    add("default base", dir, repo, c, None);

    // unresolvable commit / base
    let (dir, repo) = repo_with_root();
    add("no such commit", dir, repo, "0".repeat(40), Some("main"));
    let (dir, repo) = repo_with_root();
    let c = git(&repo, &["rev-parse", "HEAD"]);
    add("no such base", dir, repo, c, Some("no-such-branch"));

    // option-looking input must stay an operand
    let (dir, repo) = repo_with_root();
    add(
        "option-like commit",
        dir,
        repo,
        "--output=/etc/gitpulse-should-not-exist".into(),
        Some("main"),
    );
    let (dir, repo) = repo_with_root();
    let c = git(&repo, &["rev-parse", "HEAD"]);
    add("option-like base", dir, repo, c, Some("--all"));

    // shallow clone, a commit below the boundary and an unmerged branch tip
    let (origin_dir, origin) = repo_with_root();
    git(&origin, &["checkout", "-q", "-b", "feature"]);
    let feature = put(&origin, "f.txt", "f\n", "feature");
    git(&origin, &["checkout", "-q", "main"]);
    let old = put(&origin, "old.txt", "o\n", "old");
    put(&origin, "new.txt", "n\n", "new");
    let clone_dir = tempfile::tempdir().expect("tempdir");
    let clone = clone_dir.path().to_string_lossy().into_owned();
    let out = Command::new("git")
        .args(["clone", "-q", "--depth", "1", "--no-single-branch"])
        .arg(format!("file://{origin}"))
        .arg(&clone)
        .output_locked()
        .expect("clone");
    assert!(out.status.success(), "{out:?}");
    crate::test_support::trust_repo(clone_dir.path());
    keep.push(origin_dir);
    cases.push((
        "shallow, below the boundary".into(),
        clone.clone(),
        old,
        Some("origin/main".into()),
    ));
    cases.push((
        "shallow, unmerged tip".into(),
        clone,
        feature,
        Some("origin/main".into()),
    ));
    keep.push(clone_dir);
    (keep, cases)
}

fn schema_of(name: &str) -> Value {
    tools()
        .into_iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("{name} not advertised"))["outputSchema"]
        .clone()
}

/// Every key the payload has is a property the schema names (the schema does
/// not close the object, so a missing declaration would otherwise pass), and
/// every property it requires is present.
fn assert_conforms(label: &str, payload: &Value, schema: &Value) {
    let violations: Vec<String> = validate::validate(payload, schema)
        .iter()
        .map(validate::Violation::render)
        .collect();
    assert!(violations.is_empty(), "{label}: {violations:?}\n{payload}");
    let properties = schema["properties"].as_object().expect("properties");
    for key in payload.as_object().expect("object").keys() {
        assert!(
            properties.contains_key(key),
            "{label}: undeclared field {key}"
        );
    }
    for required in schema["required"].as_array().expect("required") {
        assert!(
            payload.get(required.as_str().unwrap()).is_some(),
            "{label}: missing {required}"
        );
    }
}

#[test]
fn live_landed_and_provenance_payloads_meet_their_output_schemas_on_every_fixture() {
    let (_keep, cases) = fixtures();
    let landed_schema = schema_of("gitpulse_landed");
    let provenance_schema = schema_of("gitpulse_provenance");
    let mut seen = std::collections::BTreeSet::new();
    for (label, repo, commit, base) in &cases {
        let mut arguments = json!({ "repo_path": repo, "commit": commit });
        let mut provenance_arguments = json!({ "repo_path": repo, "commit_sha": commit });
        if let Some(base) = base {
            arguments["base"] = json!(base);
            provenance_arguments["base_branch"] = json!(base);
        }
        // An argument the boundary refuses never reaches the tool: that is an
        // answer too, and no payload to validate.
        if validate_arguments("gitpulse_landed", &arguments).is_err() {
            println!("OUTSCHEMA {label}: refused at the argument boundary");
            continue;
        }
        let landed = handle_tool_call("gitpulse_landed", &arguments)
            .unwrap_or_else(|e| panic!("{label}: landed errored: {e}"));
        assert_conforms(&format!("landed/{label}"), &landed, &landed_schema);
        println!(
            "OUTSCHEMA {label}: landed={} via={} error={:?}",
            landed["landed"], landed["via"], landed["error"]
        );
        // The enum is the schema's own claim about `via`.
        let via = &landed["via"];
        assert!(
            via.is_null()
                || ["ff", "merge", "squash", "rebase", "tree"].contains(&via.as_str().unwrap()),
            "{label}: via {via}"
        );
        // null landed always says why; true/false never carries a null-reason.
        if landed["landed"].is_null() {
            assert!(
                !landed["error"].as_str().unwrap().is_empty(),
                "{label}: {landed}"
            );
        }
        seen.insert(landed["landed"].to_string());

        if validate_arguments("gitpulse_provenance", &provenance_arguments).is_ok() {
            let provenance = handle_tool_call("gitpulse_provenance", &provenance_arguments)
                .unwrap_or_else(|e| panic!("{label}: provenance errored: {e}"));
            assert_conforms(
                &format!("provenance/{label}"),
                &provenance,
                &provenance_schema,
            );
            // A null relation always says why; a measured one never has a reason.
            let measured = !provenance["landed_on_base"].is_null();
            assert_eq!(
                provenance["base_relation_reason"]
                    .as_str()
                    .unwrap()
                    .is_empty(),
                measured,
                "{label}: {provenance}"
            );
            assert_eq!(
                provenance["on_base"], provenance["landed_on_base"],
                "{label}"
            );
        }
    }
    // The fixtures reached all three answers: true, false and null.
    for answer in ["true", "false", "null"] {
        assert!(
            seen.contains(answer),
            "no fixture answered {answer}: {seen:?}"
        );
    }
}

#[test]
fn gitpulse_landed_is_read_only_everywhere_the_server_says_so() {
    let tool = tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_landed")
        .expect("advertised");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["destructiveHint"], false, "{tool}");
    let contents = resources::read("gitpulse://server/manifest").expect("manifest");
    let manifest: Value =
        serde_json::from_str(contents[0]["text"].as_str().expect("text")).expect("json");
    let writes = manifest["writes"].as_array().expect("writes");
    assert!(
        !writes.iter().any(|name| name == "gitpulse_landed"),
        "{writes:?}"
    );
    // And it is in the manifest's tool list at all.
    let listed = manifest["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .any(|t| t == "gitpulse_landed" || t["name"] == "gitpulse_landed")
        })
        .unwrap_or(true);
    assert!(
        listed,
        "gitpulse_landed missing from the manifest's tools: {manifest}"
    );
}

/// Ledger rows as catch-up writes them for a commit: transcript commands of the
/// given sessions, then the commit's reflog entry.
fn record_commit_run(repo: &str, commit: &str, commands: &[(&str, u64)], reflog_at: u64) {
    use crate::ledger::{append, ids, ActorKind, Draft, EventStamp, Outcome};
    let stamp = |ms: u64, basis: &str| EventStamp {
        occurred_at: ids::iso8601_utc(ms),
        occurred_at_basis: basis.into(),
        task_basis: "unbound".into(),
    };
    for (session, at) in commands {
        append(Draft {
            repo_path: repo.into(),
            actor_kind: Some(ActorKind::Agent),
            session_id: Some((*session).into()),
            action: "session.command".into(),
            object: Some("git commit -m x".into()),
            outcome: Some(Outcome::Ok),
            duration_ms: Some(10_000),
            detail_json: Some(r#"{"result":"ok"}"#.into()),
            event_stamp: Some(stamp(*at, "transcript")),
            ..Draft::default()
        })
        .expect("append command");
    }
    append(Draft {
        repo_path: repo.into(),
        actor_kind: Some(ActorKind::System),
        actor_id: Some("reflog".into()),
        action: "reflog.commit".into(),
        object: Some(format!("{commit} x")),
        outcome: Some(Outcome::Ok),
        after_ref: Some(commit.into()),
        event_stamp: Some(stamp(reflog_at, "reflog")),
        ..Draft::default()
    })
    .expect("append reflog");
}

/// `session_basis` and `session_ambiguous` are plain `String` and `bool`
/// fields, serialized on every row (never null, never omitted). The schema
/// declares them non-nullable, so each shape of answer must validate: an
/// inferred session, an ambiguous one, a hand commit, and no ledger at all.
#[test]
fn the_session_fields_validate_against_the_output_schema_attributed_or_not() {
    let schema = schema_of("gitpulse_provenance");
    let now = crate::ledger::ids::now_millis() / 1000 * 1000;
    let ask = |repo: &str, commit: &str| {
        let args = json!({ "repo_path": repo, "commit_sha": commit, "base_branch": "main" });
        validate_arguments("gitpulse_provenance", &args).expect("arguments");
        handle_tool_call("gitpulse_provenance", &args).expect("provenance")
    };

    let (_a, attributed) = repo_with_root();
    let sha = git(&attributed, &["rev-parse", "HEAD"]);
    record_commit_run(&attributed, &sha, &[("S1", now - 3_000)], now);
    let got = ask(&attributed, &sha);
    assert_conforms("attributed", &got, &schema);
    assert_eq!(got["session_basis"], "inferred_from_command");
    assert_eq!(got["session_ambiguous"], false);
    assert_eq!(got["session"]["session_id"], "S1");
    assert_eq!(got["session"]["attribution"], "inferred");

    let (_b, ambiguous) = repo_with_root();
    let sha = git(&ambiguous, &["rev-parse", "HEAD"]);
    record_commit_run(
        &ambiguous,
        &sha,
        &[("S1", now - 9_000), ("S2", now - 2_000)],
        now,
    );
    let got = ask(&ambiguous, &sha);
    assert_conforms("ambiguous", &got, &schema);
    assert!(got["session"].is_null());
    assert_eq!(got["session_ambiguous"], true);
    assert_eq!(got["session_basis"], "ambiguous_sessions");

    let (_c, hand) = repo_with_root();
    let sha = git(&hand, &["rev-parse", "HEAD"]);
    record_commit_run(&hand, &sha, &[], now);
    let got = ask(&hand, &sha);
    assert_conforms("hand commit", &got, &schema);
    assert!(got["session"].is_null());
    assert_eq!(got["session_basis"], "no_ingested_session_command");
    assert_eq!(got["session_ambiguous"], false);

    let (_d, no_ledger) = repo_with_root();
    let sha = git(&no_ledger, &["rev-parse", "HEAD"]);
    let got = ask(&no_ledger, &sha);
    assert_conforms("no ledger", &got, &schema);
    assert_eq!(got["session_basis"], "ledger_not_initialised");
    assert!(got["session_basis"].is_string() && got["session_ambiguous"].is_boolean());
}

/// One ledger row for the every-basis test: a transcript command or a reflog
/// entry, stamped (or not) the way the case needs.
fn put_row(
    repo: &str,
    kind: &str,
    message: &str,
    commit: &str,
    stamp: Option<(u64, &str)>,
    result: Option<&str>,
) {
    use crate::ledger::{append, ids, ActorKind, Draft, EventStamp, Outcome};
    let is_command = kind == "command";
    // A reflog row as ingest writes it: git's message split at its first
    // colon, the head folded into the action and the tail kept in `object`.
    let (head, tail) = message.split_once(':').unwrap_or(("action", message));
    append(Draft {
        repo_path: repo.into(),
        actor_kind: Some(if is_command {
            ActorKind::Agent
        } else {
            ActorKind::System
        }),
        session_id: is_command.then(|| "S1".to_string()),
        action: if is_command {
            "session.command".into()
        } else {
            format!("reflog.{}", crate::ledger::action_segment(head.trim()))
        },
        object: Some(if is_command {
            message.to_string()
        } else {
            format!("{commit} {}", tail.trim())
        }),
        outcome: Some(Outcome::Ok),
        after_ref: (!is_command).then(|| commit.to_string()),
        // A call whose result was read ok was timed; one whose result was never
        // read has no end.
        duration_ms: (result == Some("ok")).then_some(5_000),
        detail_json: result.map(|word| format!(r#"{{"result":"{word}"}}"#)),
        event_stamp: stamp.map(|(at, basis)| EventStamp {
            occurred_at: ids::iso8601_utc(at),
            occurred_at_basis: basis.into(),
            task_basis: "unbound".into(),
        }),
        ..Draft::default()
    })
    .expect("append row");
}

/// Every `session_basis` the matcher can give, produced by the live tool from a
/// real repository and a real ledger, validated against the advertised schema,
/// with the two session fields present and typed whatever the answer.
#[test]
fn every_session_basis_validates_against_the_output_schema() {
    use crate::ledger::attribution::basis;
    let schema = schema_of("gitpulse_provenance");
    let now = crate::ledger::ids::now_millis() / 1000 * 1000;
    let cmd = "git commit -m x";
    let ok = Some("ok");
    // (expected basis, rows to record before asking)
    type Setup = Box<dyn Fn(&str, &str)>;
    let cases: Vec<(&str, Setup)> = vec![
        (
            basis::INFERRED,
            Box::new(move |r, c| {
                put_row(r, "command", cmd, c, Some((now - 3_000, "transcript")), ok);
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::AMBIGUOUS,
            Box::new(move |r, c| {
                put_row(r, "command", cmd, c, Some((now - 3_000, "transcript")), ok);
                // A second session: same text, recorded under another session id.
                crate::ledger::append(crate::ledger::Draft {
                    repo_path: r.into(),
                    actor_kind: Some(crate::ledger::ActorKind::Agent),
                    session_id: Some("S2".into()),
                    action: "session.command".into(),
                    object: Some(cmd.into()),
                    outcome: Some(crate::ledger::Outcome::Ok),
                    duration_ms: Some(10_000),
                    detail_json: Some(r#"{"result":"ok"}"#.into()),
                    event_stamp: Some(crate::ledger::EventStamp {
                        occurred_at: crate::ledger::ids::iso8601_utc(now - 2_000),
                        occurred_at_basis: "transcript".into(),
                        task_basis: "unbound".into(),
                    }),
                    ..crate::ledger::Draft::default()
                })
                .expect("second session");
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::NO_INGESTED_SESSION_COMMAND,
            Box::new(move |r, c| {
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::COMMAND_OUTSIDE_WINDOW,
            Box::new(move |r, c| {
                put_row(
                    r,
                    "command",
                    cmd,
                    c,
                    Some((now - 150_000, "transcript")),
                    ok,
                );
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::COMMAND_AFTER_REFLOG,
            Box::new(move |r, c| {
                put_row(r, "command", cmd, c, Some((now + 5_000, "transcript")), ok);
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::COMMAND_UNTIMED,
            Box::new(move |r, c| {
                put_row(r, "command", cmd, c, None, ok);
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::COMMAND_OUTCOME_UNKNOWN,
            Box::new(move |r, c| {
                put_row(
                    r,
                    "command",
                    cmd,
                    c,
                    Some((now - 3_000, "transcript")),
                    None,
                );
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::COMMAND_TARGET_UNKNOWN,
            Box::new(move |r, c| {
                put_row(
                    r,
                    "command",
                    "env GIT_DIR=/x/.git git commit -m x",
                    c,
                    Some((now - 3_000, "transcript")),
                    ok,
                );
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::COMMAND_SPENT,
            Box::new(move |r, c| {
                // A `git pull` that found nothing, and a commit of another kind
                // after it: the command does not write this entry.
                put_row(
                    r,
                    "command",
                    "git pull",
                    c,
                    Some((now - 3_000, "transcript")),
                    ok,
                );
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::COMMAND_ORDER_UNKNOWN,
            Box::new(move |r, c| {
                // Another commit's origin entry that git could not date.
                let other = "d".repeat(40);
                put_row(r, "command", cmd, c, Some((now - 5_000, "transcript")), ok);
                put_row(
                    r,
                    "reflog",
                    "commit: x",
                    &other,
                    Some((now - 3_000, "ingest")),
                    None,
                );
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
            }),
        ),
        (
            basis::NO_REFLOG_ENTRY,
            Box::new(move |r, c| {
                put_row(r, "command", cmd, c, Some((now - 3_000, "transcript")), ok);
            }),
        ),
        (
            basis::NO_ORIGIN_ENTRY,
            Box::new(move |r, c| {
                put_row(
                    r,
                    "reflog",
                    "merge x: Fast-forward",
                    c,
                    Some((now, "reflog")),
                    None,
                );
            }),
        ),
        (
            basis::REFLOG_UNTIMED,
            Box::new(move |r, c| {
                put_row(r, "reflog", "commit: x", c, Some((now, "ingest")), None);
            }),
        ),
        (
            basis::SCAN_CAPPED,
            Box::new(move |r, c| {
                put_row(r, "reflog", "commit: x", c, Some((now, "reflog")), None);
                let db =
                    rusqlite::Connection::open(crate::ledger::ledger_path(r)).expect("open ledger");
                let tx = db.unchecked_transaction().expect("tx");
                for n in 0..(crate::ledger::attribution::LINK_SCAN_ROWS + 10) {
                    tx.execute(
                        "INSERT INTO events (ulid, ts_utc, repo_path, actor_kind, action, outcome)
                     VALUES (?1, '2026-01-01T00:00:00.000Z', 'r', 'system', 'noise', 'ok')",
                        [format!("{n:026}")],
                    )
                    .expect("noise");
                }
                tx.commit().expect("commit noise");
            }),
        ),
        (basis::LEDGER_ABSENT, Box::new(|_, _| {})),
        (
            basis::LEDGER_UNREADABLE,
            Box::new(|r, _| {
                let dir = std::path::Path::new(r).join(".devcouncil");
                std::fs::create_dir_all(&dir).expect("state dir");
                std::fs::write(dir.join("ledger.sqlite"), vec![0x42u8; 4096]).expect("garbage");
            }),
        ),
    ];
    let mut produced = std::collections::BTreeSet::new();
    for (expected, setup) in &cases {
        let (_keep, repo) = repo_with_root();
        let sha = git(&repo, &["rev-parse", "HEAD"]);
        setup(&repo, &sha);
        let args = json!({ "repo_path": repo, "commit_sha": sha, "base_branch": "main" });
        validate_arguments("gitpulse_provenance", &args).expect("arguments");
        let got = handle_tool_call("gitpulse_provenance", &args).expect("provenance");
        assert_conforms(expected, &got, &schema);
        assert_eq!(got["session_basis"], *expected, "{got}");
        assert!(got["session_basis"].is_string() && got["session_ambiguous"].is_boolean());
        assert_eq!(
            got["session_ambiguous"],
            *expected == basis::AMBIGUOUS,
            "{expected}"
        );
        assert_eq!(
            !got["session"].is_null(),
            *expected == basis::INFERRED,
            "{expected}"
        );
        produced.insert(*expected);
    }
    // The one value a single call cannot reach: past the per-call commit bound.
    produced.insert(basis::NOT_EXAMINED);
    let listed: std::collections::BTreeSet<&str> = basis::ALL.into_iter().collect();
    assert_eq!(
        produced, listed,
        "a basis value the schema test did not exercise"
    );
}
