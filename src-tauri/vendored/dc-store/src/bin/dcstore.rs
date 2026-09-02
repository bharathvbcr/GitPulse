//! `dcstore` — the process boundary between the Go execution plane and the
//! Rust state store.
//!
//! The harness is Go; the store is Rust because SQLite is. Rather than link
//! them with cgo — which would forfeit the static binary and simple
//! cross-compilation the Go side was chosen for — the boundary is a process:
//! Go execs this binary, writes nothing, and reads one JSON object from stdout.
//!
//! Two properties make that boundary safe rather than merely convenient:
//!
//!   - **Every outcome is JSON on stdout, including failures.** A caller never
//!     has to parse prose or guess from an exit code alone. The exit code is a
//!     coarse duplicate of `ok`, for shell use.
//!   - **A lease conflict is a normal result, not an error.** `acquire` on a
//!     held task exits 0 with `{"ok":false,"code":"lease_held_by_other"}`,
//!     because contention is the expected case when two builders run, and a
//!     caller that has to distinguish "contended" from "the store is broken"
//!     by reading stderr will eventually get it wrong.
//!
//! JSON is emitted by hand. The alternative is a serde dependency for one
//! struct shape on a boundary this small, and the store deliberately carries
//! only rusqlite.

use std::process::ExitCode;

use dc_store::{AcquireRequest, Lease, LeaseCode, ScopeWrite, Store, StoreError};

fn main() -> ExitCode {
    let args = match collect_args() {
        Ok(args) => args,
        Err(message) => {
            println!(
                "{}",
                object(&[("ok", &json_bool(false)), ("error", &quote(&message))])
            );
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(Failure::Conflict(json)) => {
            // Contention is an outcome, not a fault: exit 0 so a caller that
            // does check the exit code does not treat a busy task as an outage.
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(Failure::Fatal(message)) => {
            println!(
                "{}",
                object(&[("ok", &json_bool(false)), ("error", &quote(&message))])
            );
            ExitCode::from(2)
        }
    }
}

/// Collects the command line, refusing an argument that is not valid Unicode
/// rather than dying on it.
///
/// `std::env::args()` panics on such an argument, and a panic here breaks the
/// one property this boundary rests on and this file's own header states: every
/// outcome is JSON on stdout. What it produced instead was exit 101, a Rust
/// backtrace on stderr, and an empty stdout — which the Go client can only
/// report as "failed: exit status 101", handing an operator a panic dump in
/// place of a diagnosis. Argument vectors on Unix are bytes, and the values
/// crossing this boundary include branch names and identifiers that reach the
/// harness from the environment, so bytes that are not UTF-8 are reachable
/// input, not a hypothetical.
///
/// The offending value is named by position and never by content: one of the
/// arguments carries a lease token, and an error message that echoed it would
/// copy a credential into the caller's logs.
fn collect_args() -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    for (index, arg) in std::env::args_os().skip(1).enumerate() {
        match arg.into_string() {
            Ok(value) => args.push(value),
            Err(_) => {
                return Err(format!(
                    "argument {index} is not valid UTF-8; the store cannot parse a request it \
                     cannot read, and its value is withheld here because one argument is a lease token"
                ));
            }
        }
    }
    Ok(args)
}

/// Flags this binary accepts. Kept beside the parser so adding a flag without
/// listing it fails loudly at first use rather than being dropped.
const KNOWN_FLAGS: &[&str] = &[
    "db",
    "task",
    "owner",
    "agent",
    "client-id",
    "run-id",
    "branch",
    "ttl-seconds",
    "force",
    "token",
    "expected",
    "appended",
];

/// The identity a caller checks to confirm it is talking to this store and not
/// to some other program that happens to print JSON.
const STORE_IDENTITY: &str = "dc-store";

/// What `health` answers once the exclusion index in the opened database has
/// been read back and found to be the partial unique index on `task_id`. The
/// Go client requires this exact word, so a store that cannot make the
/// assertion cannot pass for one that can.
const EXCLUSION_INDEX_VERIFIED: &str = "verified";

enum Failure {
    Conflict(String),
    Fatal(String),
}

fn run(args: &[String]) -> Result<String, Failure> {
    let mut db: Option<String> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut flags: Vec<(String, String)> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if let Some(name) = arg.strip_prefix("--") {
            // An unrecognised flag is fatal rather than ignored. A silently
            // dropped "--frce true" looks exactly like a honoured one from the
            // caller's side: the setting appears to apply and does nothing.
            if !KNOWN_FLAGS.contains(&name) {
                return Err(Failure::Fatal(format!(
                    "unknown flag --{name} (known: {})",
                    KNOWN_FLAGS.join(", ")
                )));
            }
            let value = args
                .get(i + 1)
                .ok_or_else(|| Failure::Fatal(format!("flag --{name} needs a value")))?;
            if name == "db" {
                db = Some(value.clone());
            } else {
                flags.push((name.to_string(), value.clone()));
            }
            i += 2;
        } else {
            positional.push(arg);
            i += 1;
        }
    }

    let db = db.ok_or_else(|| Failure::Fatal("--db is required".into()))?;
    let command = *positional
        .first()
        .ok_or_else(|| Failure::Fatal("a command is required".into()))?;

    // health is the one command that must not bring a store into existence.
    // Every other command is doing work that implies the store — acquiring,
    // releasing, widening — but health is the question the Go client asks
    // *before* it will call the store reachable, and `Connection::open` creates
    // the file when it is absent. A mistyped --db therefore answered
    // "healthy, zero leases" from a private empty database nobody else was
    // using, which is exactly the confident zero Available() promises never to
    // report. A store that is not there is now an answer, not a side effect.
    let store = if command == "health" {
        Store::open_existing(&db)
    } else {
        Store::open(&db)
    }
    .map_err(|e| Failure::Fatal(e.to_string()))?;
    let flag = |name: &str| -> Option<&str> {
        flags
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    // An empty identifier is refused rather than looked up. Searching for the
    // task with the empty id returns "no such task", which is a plausible
    // answer to a question nobody meant to ask — it hides the caller's bug
    // behind a normal-looking result.
    let required = |name: &str| -> Result<&str, Failure> {
        match flag(name) {
            None => Err(Failure::Fatal(format!(
                "--{name} is required for {command}"
            ))),
            Some(v) if v.trim().is_empty() => Err(Failure::Fatal(format!(
                "--{name} is empty; an empty identifier is a caller bug, not a lookup"
            ))),
            Some(v) => Ok(v),
        }
    };

    match command {
        "acquire" => {
            let ttl = match flag("ttl-seconds") {
                Some(raw) => Some(raw.parse::<i64>().map_err(|_| {
                    Failure::Fatal(format!("--ttl-seconds {raw:?} is not a number"))
                })?),
                None => None,
            };
            // Parsed, not tested for equality with "true". `--force 1` used to
            // be silently false, which is the same class KNOWN_FLAGS exists to
            // stop one line up: a value the parser does not understand leaves
            // the caller believing a setting applied when it did nothing. It
            // failed safe — no steal — but "safe" and "what you asked for" are
            // different answers and only one of them was reported.
            let force = match flag("force") {
                None | Some("false") => false,
                Some("true") => true,
                Some(other) => {
                    return Err(Failure::Fatal(format!(
                        "--force {other:?} is neither \"true\" nor \"false\"; \
                         a value this parser does not understand would be honoured as \
                         false while looking to the caller like it applied"
                    )));
                }
            };
            let request = AcquireRequest {
                task_id: required("task")?.to_string(),
                owner: required("owner")?.to_string(),
                agent: flag("agent").map(str::to_string),
                client_id: flag("client-id").map(str::to_string),
                run_id: flag("run-id").map(str::to_string),
                branch: flag("branch").map(str::to_string),
                ttl_seconds: ttl,
                force,
            };
            match store.acquire(&request) {
                Ok(lease) => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("lease", &lease_json(&lease)),
                ])),
                Err(StoreError::LeaseHeld { task_id, holder }) => {
                    Err(Failure::Conflict(object(&[
                        ("ok", &json_bool(false)),
                        ("code", &quote(LeaseCode::HeldByOther.as_str())),
                        ("task_id", &quote(&task_id)),
                        ("holder", &quote(&holder)),
                        (
                            "error",
                            &quote(&format!("task {task_id} is held by {holder}")),
                        ),
                    ])))
                }
                Err(e) => Err(Failure::Fatal(e.to_string())),
            }
        }

        "diagnose" => {
            let code = store
                .diagnose(required("task")?, required("token")?)
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let mut fields: Vec<(&str, String)> = vec![
                ("ok", json_bool(code == LeaseCode::Valid)),
                ("code", quote(code.as_str())),
            ];
            if let Some((action, tool)) = code.recovery() {
                fields.push(("suggested_action", quote(action)));
                fields.push(("suggested_tool", quote(tool)));
            }
            Ok(object_owned(&fields))
        }

        "release" => {
            let released = store
                .release(required("task")?, required("token")?)
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("released", &json_bool(released)),
            ]))
        }

        "renew" => {
            let ttl: i64 = required("ttl-seconds")?
                .parse()
                .map_err(|_| Failure::Fatal("--ttl-seconds is not a number".into()))?;
            match store
                .renew(required("task")?, required("token")?, ttl)
                .map_err(|e| Failure::Fatal(e.to_string()))?
            {
                Some(lease) => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("lease", &lease_json(&lease)),
                ])),
                // No active lease to renew is a normal outcome an agent
                // branches on: renew only works before expiry.
                None => Ok(object(&[
                    ("ok", &json_bool(false)),
                    ("code", &quote(LeaseCode::Expired.as_str())),
                    ("suggested_action", &quote("checkout_again")),
                ])),
            }
        }

        // scope-append is the durable half of the override seam. A grant makes
        // one blocked write accountable and expires; this makes the *argument*
        // the grant recorded outlive it, by writing the path into the task's
        // own plan where the gate reads scope from.
        //
        // It takes the whole replacement array and the value the caller last
        // saw, because the store has no JSON parser to merge with and no way to
        // tell an intentional removal from a stale read. The caller merges; the
        // store guarantees the merge landed on what it was computed from.
        "scope-append" => {
            let outcome = store
                .set_agent_appended_planned_files(
                    required("task")?,
                    required("token")?,
                    required("expected")?,
                    required("appended")?,
                )
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            match outcome {
                ScopeWrite::Written => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("written", &json_bool(true)),
                ])),
                ScopeWrite::NoTask => Ok(object(&[
                    ("ok", &json_bool(false)),
                    ("written", &json_bool(false)),
                    ("code", &quote("no_task")),
                    (
                        "error",
                        &quote(
                            "no such task; scope cannot be widened onto a task that does not exist",
                        ),
                    ),
                ])),
                ScopeWrite::NotLeased(code) => {
                    let mut fields: Vec<(&str, String)> = vec![
                        ("ok", json_bool(false)),
                        ("written", json_bool(false)),
                        ("code", quote(code.as_str())),
                        (
                            "error",
                            quote("only the holder of this task's lease may widen its scope"),
                        ),
                    ];
                    if let Some((action, tool)) = code.recovery() {
                        fields.push(("suggested_action", quote(action)));
                        fields.push(("suggested_tool", quote(tool)));
                    }
                    Ok(object_owned(&fields))
                }
                // The current value travels with the refusal so the caller can
                // merge against it directly. Embedded raw: it is already JSON,
                // and quoting it would hand back a string where the caller
                // expects the array it has to merge with.
                ScopeWrite::Stale { current } => Ok(object(&[
                    ("ok", &json_bool(false)),
                    ("written", &json_bool(false)),
                    ("code", &quote("scope_stale")),
                    ("current_appended", &current),
                    (
                        "error",
                        &quote("the task's appended scope changed since it was read"),
                    ),
                ])),
            }
        }

        "active" => {
            match store
                .active_lease(required("task")?)
                .map_err(|e| Failure::Fatal(e.to_string()))?
            {
                Some(lease) => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("lease", &lease_json(&lease)),
                ])),
                None => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("lease", &"null".to_string()),
                ])),
            }
        }

        "task" => {
            match store
                .task(required("task")?)
                .map_err(|e| Failure::Fatal(e.to_string()))?
            {
                Some(t) => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("task", &task_json(&t)),
                ])),
                // A task that does not exist is a normal answer, not a fault:
                // a caller asking about an unknown id needs to branch, not to
                // handle an exception.
                None => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("task", &"null".to_string()),
                ])),
            }
        }

        "ready" => {
            let ids = store
                .ready_tasks()
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let items: Vec<String> = ids.iter().map(|id| quote(id)).collect();
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("tasks", &format!("[{}]", items.join(","))),
            ]))
        }

        "list" => {
            let leases = store
                .active_leases()
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let items: Vec<String> = leases.iter().map(lease_json).collect();
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("leases", &format!("[{}]", items.join(","))),
            ]))
        }

        // health is the liveness answer the Go client requires before it will
        // call the store reachable. It is a positive assertion — this binary,
        // this schema — rather than the absence of an error, so a program that
        // merely prints "{\"ok\":true}" cannot pass for a working store.
        //
        // `exclusion_index` is the third assertion, and the one that used to be
        // missing: reaching this line means Store::open_existing read the
        // opened file's own schema and found the partial unique index that
        // makes two racing acquires resolve to one winner. Before, health
        // reported a compile-time constant, and a database carrying a
        // same-named index with a different definition answered ok while
        // handing two agents the same task.
        "health" => {
            let leases = store
                .active_leases()
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("store", &quote(STORE_IDENTITY)),
                ("schema_version", &dc_store::SCHEMA_VERSION.to_string()),
                ("exclusion_index", &quote(EXCLUSION_INDEX_VERIFIED)),
                ("active_leases", &leases.len().to_string()),
            ]))
        }

        other => Err(Failure::Fatal(format!(
            "unknown command {other:?} (acquire, diagnose, release, renew, active, list, task, ready, scope-append, health)"
        ))),
    }
}

fn lease_json(lease: &Lease) -> String {
    object(&[
        ("id", &quote(&lease.id)),
        ("task_id", &quote(&lease.task_id)),
        ("owner", &quote(&lease.owner)),
        ("agent", &maybe(lease.agent.as_deref())),
        ("client_id", &maybe(lease.client_id.as_deref())),
        ("run_id", &maybe(lease.run_id.as_deref())),
        ("branch", &maybe(lease.branch.as_deref())),
        ("token", &quote(&lease.token)),
        ("status", &quote(&lease.status)),
        ("created_at", &quote(&lease.created_at)),
        ("expires_at", &maybe(lease.expires_at.as_deref())),
        ("released_at", &maybe(lease.released_at.as_deref())),
    ])
}

fn task_json(t: &dc_store::Task) -> String {
    // The *_json fields are already JSON text, so they are embedded raw rather
    // than quoted — quoting them would deliver a string where the consumer
    // expects an array.
    object(&[
        ("id", &quote(&t.id)),
        ("title", &quote(&t.title)),
        ("status", &quote(&t.status)),
        ("difficulty", &maybe(t.difficulty.as_deref())),
        ("planned_files", &t.planned_files_json),
        // The union above is what the gate enforces; this is the part of it an
        // executor added to its own scope. A consumer that cannot tell them
        // apart would report a self-widened write as one the planner authorised.
        (
            "agent_appended_planned_files",
            &t.agent_appended_planned_files_json,
        ),
        ("allowed_commands", &t.allowed_commands_json),
        ("expected_tests", &t.expected_tests_json),
        ("forbidden_changes", &t.forbidden_changes_json),
        // Why the task exists, and what it must prove. Emitted even when empty
        // so a consumer can tell a task accountable to no requirement from a
        // store that does not report requirements at all — which is what this
        // boundary answered before, for every task.
        ("requirement_ids", &t.requirement_ids_json),
        ("acceptance_criterion_ids", &t.acceptance_criterion_ids_json),
    ])
}

fn object(fields: &[(&str, &String)]) -> String {
    let body: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("{}:{}", quote(k), v))
        .collect();
    format!("{{{}}}", body.join(","))
}

fn object_owned(fields: &[(&str, String)]) -> String {
    let body: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("{}:{}", quote(k), v))
        .collect();
    format!("{{{}}}", body.join(","))
}

fn maybe(value: Option<&str>) -> String {
    match value {
        Some(v) => quote(v),
        None => "null".to_string(),
    }
}

fn json_bool(v: bool) -> String {
    if v { "true".into() } else { "false".into() }
}

/// Escapes a string per RFC 8259. Control characters must be escaped or the
/// output is not parseable JSON — a task title with a newline in it would
/// otherwise produce a payload the Go side rejects.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
