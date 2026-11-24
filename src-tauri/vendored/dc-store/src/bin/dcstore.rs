//! `dcstore` — the process boundary between the Go execution plane and the
//! Rust state store.
//!
//! The harness is Go; the store is Rust because SQLite is. Rather than link
//! them with cgo — which would forfeit the static binary and simple
//! cross-compilation the Go side was chosen for — the boundary is a process:
//! Go runs this binary and reads JSON objects from its stdout.
//!
//! It is reached two ways, and they are the same code underneath. A one-shot
//! call passes a command on argv and gets one object back. A `serve` session
//! reads framed requests on stdin and answers each with one object, holding a
//! single SQLite connection open across them — which is what keeps a lease
//! check from costing a process. Both land in `dispatch`, so a served command
//! and an exec'd one cannot mean different things.
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

use std::io::{BufRead, BufReader, Read, Write};
use std::process::ExitCode;

use dc_proc::cli::{collect_args_with_note, is_version_arg, quote, wants_version};
use dc_store::{AcquireRequest, Lease, LeaseCode, ReadyMode, ScopeWrite, Store, StoreError};

fn main() -> ExitCode {
    let args = match collect_args_with_note(
        "; the store cannot parse a request it cannot read, and its value is withheld here because one argument is a lease token",
    ) {
        Ok(args) => args,
        Err(message) => {
            dc_proc::respond!("{}", fatal_object(&message));
            return ExitCode::from(2);
        }
    };
    // `serve` is a transport rather than a command: it answers many requests
    // over one process instead of returning one document, so it is dispatched
    // before run() rather than from inside dispatch(). A parse that fails here
    // is deliberately dropped and left to run() to report, so that every
    // argument error keeps the single wording it has always had.
    if let Ok(parsed) = parse(&args)
        && parsed.command.as_deref() == Some("serve")
    {
        return serve(&parsed);
    }
    match run(&args) {
        Ok(json) => {
            dc_proc::respond!("{json}");
            ExitCode::SUCCESS
        }
        Err(Failure::Conflict(json)) => {
            // Contention is an outcome, not a fault: exit 0 so a caller that
            // does check the exit code does not treat a busy task as an outage.
            dc_proc::respond!("{json}");
            ExitCode::SUCCESS
        }
        Err(Failure::Fatal(message)) => {
            dc_proc::respond!("{}", fatal_object(&message));
            ExitCode::from(2)
        }
    }
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
    "method",
    "input",
    "type",
    "requirement-id",
    "acceptance-criterion-id",
    "subject-hash",
    "record",
    "data",
    "id",
    "from-agent",
    "to-agent",
    "manifest-path",
    "status",
    "sandbox",
    "environment",
    "commands",
    "started-at",
    "finished-at",
    "created-at",
    // `gap-upsert`'s own vocabulary. Its handler has always read all ten of
    // these, and none of them was listed here — so the parser refused the argv
    // before the handler could run, and the command failed on its first flag
    // with "unknown flag --severity". Two of them (`gap-type`, `description`)
    // are `required`, so there was no argument vector that reached it at all.
    //
    // `dc/store.GapsReplace` spells a replacement as `gaps-clear` plus one
    // `gap-upsert` per gap, and `verify/orchestrate.go` calls it after every
    // verification, so persisting a task's gaps has been failing for as long as
    // both have existed. Nothing caught it because the Go tests that exercise
    // GapsReplace pass an empty list, which never reaches the upsert loop.
    //
    // This is the comment above KNOWN_FLAGS made good on: adding a flag without
    // listing it must fail loudly at first use. It did fail — the failure was
    // just being returned to a caller that logged it and continued.
    "severity",
    "gap-type",
    "description",
    "recommended-fix",
    "evidence",
    "blocking",
    "file",
    "line",
    "suggested-command",
    "expected-verification-method",
    // Where a failed command's captured output was written. Listed with the
    // handler that reads them, not after it, which is the order the comment
    // above asks for.
    "stdout-path",
    "stderr-path",
    // `gaps-replace`: one verification (run record and gap set) in the framing
    // dc_store::commit_wire documents.
    "wire",
    // `ready`'s ordering: how a blocked task is treated, by gate mode.
    "mode",
];

/// The identity a caller checks to confirm it is talking to this store and not
/// to some other program that happens to print JSON.
const STORE_IDENTITY: &str = "dc-store";
const COMPONENT_ID: &str = "dcstore";

/// What `--version` answers.
///
/// `CARGO_PKG_VERSION` and never a literal: every crate in this workspace takes
/// `version.workspace = true`, so the one number in `rust/Cargo.toml` is what
/// reaches here, and there is no second place for a release to forget.
///
/// `workbench_schema` is the profile version this build opens, and the only
/// field that tells a schema 10 build from a schema 11 one: both are 0.2.4.
/// A host that hands its profile to whatever `dcstore` is on PATH checks it
/// first, rather than learning from `schema_unsupported` on every request.
fn version_object() -> String {
    dc_proc::cli::version_object(
        COMPONENT_ID,
        STORE_IDENTITY,
        env!("CARGO_PKG_VERSION"),
        &[(
            "workbench_schema",
            &dc_store::workbench::WORKBENCH_SCHEMA.to_string(),
        )],
    )
}

/// What `health` answers once the exclusion index in the opened database has
/// been read back and found to be the partial unique index on `task_id`. The
/// Go client requires this exact word, so a store that cannot make the
/// assertion cannot pass for one that can.
const EXCLUSION_INDEX_VERIFIED: &str = "verified";

enum Failure {
    Conflict(String),
    Fatal(String),
}

/// One argument vector, separated into the database it names and the work it
/// asks for.
///
/// `command` stays an `Option` here rather than being required during parsing,
/// so the order of the two refusals in [`run`] is the order it has always
/// been: a call with neither `--db` nor a command is answered "--db is
/// required", and a caller who fixes the path then hears about the command.
struct Parsed {
    db: Option<String>,
    command: Option<String>,
    flags: Vec<(String, String)>,
}

/// Splits an argument vector into a database, a command, and flags.
///
/// Extracted from [`run`] so `serve` parses each framed request under exactly
/// the same rules — in particular the refusal of an unknown flag below, which
/// is the check that keeps a mistyped setting from looking honoured. A served
/// request that parsed more leniently than an exec'd one would be a second
/// contract wearing the first one's name.
fn parse(args: &[String]) -> Result<Parsed, Failure> {
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

    Ok(Parsed {
        db,
        command: positional.first().map(|c| (*c).to_string()),
        flags,
    })
}

/// Opens the store the way `command` is permitted to open it.
///
/// health is the one command that must not bring a store into existence.
/// Every other command is doing work that implies the store — acquiring,
/// releasing, widening — but health is the question the Go client asks
/// *before* it will call the store reachable, and `Connection::open` creates
/// the file when it is absent. A mistyped --db therefore answered
/// "healthy, zero leases" from a private empty database nobody else was
/// using, which is exactly the confident zero Available() promises never to
/// report. A store that is not there is now an answer, not a side effect.
fn open_for(command: &str, db: &str, for_serve: bool) -> Result<Store, Failure> {
    match command {
        "runs" | "run-get" | "handoff-get" if !for_serve => Store::open_readonly(db),
        "health" | "runs" | "run-get" | "handoff-get" => Store::open_existing(db),
        _ => Store::open(db),
    }
    .map_err(|e| Failure::Fatal(e.to_string()))
}

fn run(args: &[String]) -> Result<String, Failure> {
    if wants_version(args) {
        return Ok(version_object());
    }
    let parsed = parse(args)?;
    let db = parsed
        .db
        .ok_or_else(|| Failure::Fatal("--db is required".into()))?;
    let command = parsed
        .command
        .ok_or_else(|| Failure::Fatal("a command is required".into()))?;
    let store = open_for(&command, &db, false)?;

    dispatch(&store, &command, &parsed.flags)
}

/// Runs one already-parsed command against an already-open store.
///
/// This is split from [`run`] so that the one-shot path and `serve` cannot
/// drift: a served `acquire` and an exec'd `acquire` are the same code reached
/// two ways, rather than two implementations of one contract that agree until
/// somebody edits one of them. Everything above this line is process
/// mechanics — argv, the database path, whether opening may create the file —
/// and everything a caller acts on is decided here.
fn dispatch(store: &Store, command: &str, flags: &[(String, String)]) -> Result<String, Failure> {
    if is_version_arg(command) {
        return Ok(version_object());
    }
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
        "work" => match store.workbench_request(required("method")?, required("input")?) {
            Ok(reply) => Ok(reply),
            Err(error) => Ok(object(&[
                ("ok", &json_bool(false)),
                ("code", &quote(error.code)),
                ("error", &quote(&error.message)),
            ])),
        },
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
                worktree_path: flag("worktree-path").map(str::to_string),
                worktree_id: flag("worktree-id").map(str::to_string),
            };
            match store.acquire_outcome(&request) {
                Ok((lease, retired)) => {
                    let mut reply =
                        object(&[("ok", &json_bool(true)), ("lease", &lease_json(&lease))]);
                    // A forced acquire over a lease whose expiry could not be
                    // read cleared it rather than stole it; say which, so the
                    // record shows what was retired and why.
                    if let Some(r) = retired {
                        reply.pop();
                        reply.push_str(&format!(
                            ",\"retired_unreadable_lease\":{}}}",
                            object(&[
                                ("lease_id", &quote(&r.lease_id)),
                                ("owner", &quote(&r.owner)),
                                ("expires_at", &quote(&r.expires_at)),
                            ])
                        ));
                    }
                    Ok(reply)
                }
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
                // Not a fault: the item is gone, archived or not workable. A
                // caller branches on the code instead of reading a message.
                Err(StoreError::TaskUnavailable {
                    task_id,
                    code,
                    detail,
                }) => Err(Failure::Conflict(object(&[
                    ("ok", &json_bool(false)),
                    ("code", &quote(code)),
                    ("task_id", &quote(&task_id)),
                    ("error", &quote(&detail)),
                ]))),
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

        // `ready` lists what may be checked out next, in the order the gate
        // mode dictates: under enforce a task with a blocking gap is left out;
        // under advisory or off it is listed after every unblocked one, flagged
        // `ready_to_checkout: false`. The mode is the caller's (the host reads
        // it from config) and defaults to enforce, so a caller that forgets it
        // gets the strict answer; a mode that is not one of the three is
        // refused rather than read as the permissive one.
        "ready" => {
            let mode = match flag("mode") {
                None => ReadyMode::Enforce,
                Some(raw) => ReadyMode::parse(raw).ok_or_else(|| {
                    Failure::Fatal(format!(
                        "--mode {raw:?} is not one of enforce, advisory, off"
                    ))
                })?,
            };
            let ready = store
                .ready_tasks(mode)
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let ids: Vec<String> = ready.iter().map(|t| quote(&t.id)).collect();
            let detail: Vec<String> = ready
                .iter()
                .map(|t| {
                    object(&[
                        ("id", &quote(&t.id)),
                        ("ready_to_checkout", &json_bool(t.ready_to_checkout())),
                        ("blocking_gap_count", &t.blocking_gap_count.to_string()),
                    ])
                })
                .collect();
            // Tasks held by a lease whose expiry cannot be read are left out of
            // `tasks` (they are not free) and named here, so an operator sees
            // why a task is missing instead of the whole store failing.
            let unreadable = store
                .unreadable_leases()
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("mode", &quote(mode.as_str())),
                ("tasks", &format!("[{}]", ids.join(","))),
                ("ready", &format!("[{}]", detail.join(","))),
                ("unreadable_leases", &unreadable_json(&unreadable)),
            ]))
        }

        // The repository row every item names as its primary. A host that
        // creates items asks for it first rather than spelling its id.
        "local-repository" => {
            store
                .ensure_local_repository()
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[
                ("ok", &json_bool(true)),
                (
                    "repository_id",
                    &quote(dc_store::workbench::LOCAL_REPOSITORY_ID),
                ),
            ]))
        }

        "list" => {
            let leases = store
                .active_leases()
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let items: Vec<String> = leases.iter().map(lease_json).collect();
            let unreadable = store
                .unreadable_leases()
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("leases", &format!("[{}]", items.join(","))),
                ("unreadable_leases", &unreadable_json(&unreadable)),
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
            // Still `ok`: the store works and every other task is served. But a
            // row whose expiry cannot be read is a fault someone has to fix, so
            // health says so instead of reporting the same thing as a clean
            // store.
            let unreadable = store
                .unreadable_leases()
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("degraded", &json_bool(!unreadable.is_empty())),
                ("unreadable_leases", &unreadable_json(&unreadable)),
                ("id", &quote(COMPONENT_ID)),
                ("component", &quote(STORE_IDENTITY)),
                ("store", &quote(STORE_IDENTITY)),
                ("version", &quote(env!("CARGO_PKG_VERSION"))),
                ("schema_version", &dc_store::SCHEMA_VERSION.to_string()),
                ("exclusion_index", &quote(EXCLUSION_INDEX_VERIFIED)),
                ("active_leases", &leases.len().to_string()),
            ]))
        }

        "evidence-append" => {
            let id = store
                .evidence_append(
                    required("type")?,
                    flag("task"),
                    flag("requirement-id"),
                    flag("acceptance-criterion-id"),
                    required("data")?,
                )
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[("ok", &json_bool(true)), ("id", &id.to_string())]))
        }

        "evidence-list" => {
            let (rows, truncated) = store
                .evidence_list(flag("task"))
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let items: Vec<String> = rows
                .iter()
                .map(|r| {
                    let (data, data_error) =
                        embed_raw(&r.data_json, dc_store::json::check_container, "null");
                    with_error(
                        object(&[
                            ("id", &r.id.to_string()),
                            ("type", &quote(&r.kind)),
                            (
                                "task_id",
                                &r.task_id
                                    .as_deref()
                                    .map(quote)
                                    .unwrap_or_else(|| "null".into()),
                            ),
                            (
                                "requirement_id",
                                &r.requirement_id
                                    .as_deref()
                                    .map(quote)
                                    .unwrap_or_else(|| "null".into()),
                            ),
                            (
                                "acceptance_criterion_id",
                                &r.acceptance_criterion_id
                                    .as_deref()
                                    .map(quote)
                                    .unwrap_or_else(|| "null".into()),
                            ),
                            ("data_json", &data),
                        ]),
                        "data_error",
                        data_error,
                    )
                })
                .collect();
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("evidence", &format!("[{}]", items.join(","))),
                ("truncated", &json_bool(truncated)),
                ("shown", &rows.len().to_string()),
            ]))
        }

        // One independent review of a task's change against an llm_review
        // criterion. The answer's key is `review_id`, not `id`: one envelope
        // decodes every reply on the Go side, and `id` is already the
        // evidence row's.
        "review-append" => {
            let id = store
                .review_append(
                    required("task")?,
                    required("acceptance-criterion-id")?,
                    required("subject-hash")?,
                    required("record")?,
                )
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("review_id", &id.to_string()),
            ]))
        }

        // A task's reviews, newest first. The record is embedded as the JSON
        // object it was validated as on the way in; one that no longer parses
        // is carried as null beside a `record_error`, never as a reply that
        // does not parse.
        "reviews" => {
            let (rows, truncated) = store
                .reviews_list(required("task")?, flag("acceptance-criterion-id"))
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let items: Vec<String> = rows
                .iter()
                .map(|r| {
                    let (record, record_error) =
                        embed_raw(&r.record_json, dc_store::json::check_container, "null");
                    with_error(
                        object(&[
                            ("id", &r.id.to_string()),
                            ("task_id", &quote(&r.task_id)),
                            (
                                "acceptance_criterion_id",
                                &quote(&r.acceptance_criterion_id),
                            ),
                            ("subject_hash", &quote(&r.subject_hash)),
                            ("record", &record),
                            ("recorded_at", &quote(&r.recorded_at)),
                        ]),
                        "record_error",
                        record_error,
                    )
                })
                .collect();
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("reviews", &format!("[{}]", items.join(","))),
                ("truncated", &json_bool(truncated)),
                ("shown", &rows.len().to_string()),
            ]))
        }

        // The requirement rows a task links to, which the verifier dispatches
        // on by verification method. `acceptance_criteria_json` is carried as a
        // string, not embedded: the column is not validated here, and a
        // malformed one must reach the reader as a decode error it reports,
        // not as a reply that no longer parses.
        "requirements" => {
            match store
                .task_requirements(required("task")?)
                .map_err(|e| Failure::Fatal(e.to_string()))?
            {
                None => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("task_found", &json_bool(false)),
                    ("requirements", &"[]".to_string()),
                    ("missing", &"[]".to_string()),
                    ("truncated", &json_bool(false)),
                ])),
                Some(linked) => {
                    let items: Vec<String> = linked
                        .rows
                        .iter()
                        .map(|r| {
                            object(&[
                                ("id", &quote(&r.id)),
                                ("title", &quote(&r.title)),
                                ("description", &quote(&r.description)),
                                ("priority", &quote(&r.priority)),
                                ("source", &quote(&r.source)),
                                (
                                    "acceptance_criteria_json",
                                    &quote(&r.acceptance_criteria_json),
                                ),
                            ])
                        })
                        .collect();
                    let missing: Vec<String> = linked.missing.iter().map(|m| quote(m)).collect();
                    Ok(object(&[
                        ("ok", &json_bool(true)),
                        ("task_found", &json_bool(true)),
                        ("requirements", &format!("[{}]", items.join(","))),
                        ("missing", &format!("[{}]", missing.join(","))),
                        ("truncated", &json_bool(linked.truncated)),
                    ]))
                }
            }
        }

        "gaps" => {
            let (rows, truncated) = store
                .gaps_list(flag("task"))
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let items: Vec<String> = rows
                .iter()
                .map(|r| {
                    let (evidence, evidence_error) =
                        embed_raw(&r.evidence_json, dc_store::json::check_array, "[]");
                    with_error(
                        object(&[
                            ("id", &quote(&r.id)),
                            ("severity", &quote(&r.severity)),
                            ("gap_type", &quote(&r.gap_type)),
                            ("task_id", &maybe(r.task_id.as_deref())),
                            ("description", &quote(&r.description)),
                            ("recommended_fix", &quote(&r.recommended_fix)),
                            ("blocking", &json_bool(r.blocking)),
                            ("evidence_json", &evidence),
                            // What a criterion gap is about and how it was meant to
                            // be proven. Stored by `gap-upsert` all along; without
                            // them here the reply named a criterion gap but not its
                            // criterion. `null` when the gap is not about one.
                            ("requirement_id", &maybe(r.requirement_id.as_deref())),
                            (
                                "acceptance_criterion_id",
                                &maybe(r.acceptance_criterion_id.as_deref()),
                            ),
                            (
                                "expected_verification_method",
                                &maybe(r.expected_verification_method.as_deref()),
                            ),
                            // Where the gap is and what reproduces it, dropped the
                            // same way and for as long.
                            ("file", &maybe(r.file.as_deref())),
                            (
                                "line",
                                &r.line.map_or_else(|| "null".to_string(), |n| n.to_string()),
                            ),
                            ("suggested_command", &maybe(r.suggested_command.as_deref())),
                            ("stdout_path", &maybe(r.stdout_path.as_deref())),
                            ("stderr_path", &maybe(r.stderr_path.as_deref())),
                        ]),
                        "evidence_error",
                        evidence_error,
                    )
                })
                .collect();
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("gaps", &format!("[{}]", items.join(","))),
                ("truncated", &json_bool(truncated)),
                ("shown", &rows.len().to_string()),
            ]))
        }

        // The negative memory `gaps` cannot hold. `gaps` answers "what blocks
        // me now" and is replaced wholesale each run; this answers "what has
        // been reported before, and what came back after being reported
        // fixed".
        "gap-history" => {
            let (rows, truncated) = store
                .gap_history_list(flag("task"))
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let items: Vec<String> = rows
                .iter()
                .map(|r| {
                    object(&[
                        ("task_id", &quote(&r.task_id)),
                        ("gap_id", &quote(&r.gap_id)),
                        ("gap_type", &quote(&r.gap_type)),
                        ("first_seen_run", &r.first_seen_run.to_string()),
                        ("last_seen_run", &r.last_seen_run.to_string()),
                        ("occurrences", &r.occurrences.to_string()),
                        ("resurfaces", &r.resurfaces.to_string()),
                    ])
                })
                .collect();
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("history", &format!("[{}]", items.join(","))),
                ("truncated", &json_bool(truncated)),
                ("shown", &rows.len().to_string()),
            ]))
        }

        "gaps-clear" => {
            let task = required("task")?.to_string();
            store
                .gaps_replace(&task, &[])
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let zero = String::from("0");
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("task_id", &quote(&task)),
                ("count", &zero),
            ]))
        }

        "gaps-replace" => {
            let task = required("task")?.to_string();
            let (run, gaps) = dc_store::commit_wire::decode(required("wire")?)
                .map_err(|e| Failure::Fatal(format!("--wire: {e}")))?;
            store
                .verification_commit(run.as_ref(), &task, &gaps)
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let count = gaps.len().to_string();
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("task_id", &quote(&task)),
                ("count", &count),
            ]))
        }

        "gap-upsert" => {
            let blocking = matches!(
                flag("blocking").unwrap_or("false"),
                "1" | "true" | "True" | "yes"
            );
            // Refused rather than parsed with `.ok()`: a `--line 4x` used to
            // store a gap with no location and report success, so the caller
            // believed it had recorded where the gap was.
            let line = match flag("line") {
                Some(raw) => Some(
                    raw.parse::<i64>()
                        .map_err(|_| Failure::Fatal(format!("--line {raw:?} is not a number")))?,
                ),
                None => None,
            };
            let gap = dc_store::records::GapRow {
                id: required("id")?.to_string(),
                severity: flag("severity").unwrap_or("medium").to_string(),
                gap_type: required("gap-type")?.to_string(),
                requirement_id: flag("requirement-id").map(str::to_string),
                task_id: flag("task").map(str::to_string),
                description: required("description")?.to_string(),
                evidence_json: flag("evidence").unwrap_or("[]").to_string(),
                recommended_fix: flag("recommended-fix").unwrap_or("").to_string(),
                blocking,
                file: flag("file").map(str::to_string),
                line,
                suggested_command: flag("suggested-command").map(str::to_string),
                acceptance_criterion_id: flag("acceptance-criterion-id").map(str::to_string),
                expected_verification_method: flag("expected-verification-method")
                    .map(str::to_string),
                stdout_path: flag("stdout-path").map(str::to_string),
                stderr_path: flag("stderr-path").map(str::to_string),
            };
            store
                .gap_upsert(&gap)
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            // `gap_id`, not `id`. The Go client decodes every reply on this
            // boundary through one envelope, where `id` is already
            // `evidence-append`'s new row number — an integer. Answering with a
            // string under the same key made the reply unparseable for the
            // caller, which is the second half of why this command has never
            // completed: the flags were refused first, and anything that got
            // past them died here.
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("gap_id", &quote(&gap.id)),
            ]))
        }

        "run-record" => {
            let run = dc_store::records::VerificationRun {
                id: required("id")?.to_string(),
                task_id: required("task")?.to_string(),
                sandbox: required("sandbox")?.to_string(),
                environment_json: flag("environment").unwrap_or("{}").to_string(),
                commands_json: flag("commands").unwrap_or("[]").to_string(),
                status: required("status")?.to_string(),
                started_at: required("started-at")?.to_string(),
                finished_at: flag("finished-at").map(str::to_string),
            };
            store
                .run_record(&run)
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[("ok", &json_bool(true)), ("id", &quote(&run.id))]))
        }

        "handoff" => {
            let handoff = dc_store::records::AgentHandoff {
                id: required("id")?.to_string(),
                task_id: required("task")?.to_string(),
                from_agent: required("from-agent")?.to_string(),
                to_agent: required("to-agent")?.to_string(),
                run_id: required("run-id")?.to_string(),
                manifest_path: required("manifest-path")?.to_string(),
                status: required("status")?.to_string(),
                created_at: required("created-at")?.to_string(),
            };
            store
                .handoff_record(&handoff)
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("id", &quote(&handoff.id)),
            ]))
        }

        "runs" => {
            let (rows, truncated) = store
                .runs_list(flag("task"))
                .map_err(|e| Failure::Fatal(e.to_string()))?;
            let items: Vec<String> = rows.iter().map(run_json).collect();
            Ok(object(&[
                ("ok", &json_bool(true)),
                ("runs", &format!("[{}]", items.join(","))),
                ("shown", &rows.len().to_string()),
                ("truncated", &json_bool(truncated)),
            ]))
        }

        "run-get" => {
            let id = required("id")?;
            if id.trim().is_empty() {
                return Err(Failure::Fatal("--id must not be blank".into()));
            }
            match store
                .run_get(id)
                .map_err(|e| Failure::Fatal(e.to_string()))?
            {
                Some(run) => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("found", &json_bool(true)),
                    ("run", &run_json(&run)),
                ])),
                None => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("found", &json_bool(false)),
                    ("run", &"null".to_string()),
                ])),
            }
        }

        "handoff-get" => {
            let id = required("id")?;
            if id.trim().is_empty() {
                return Err(Failure::Fatal("--id must not be blank".into()));
            }
            match store
                .handoff_get(id)
                .map_err(|e| Failure::Fatal(e.to_string()))?
            {
                Some(handoff) => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("found", &json_bool(true)),
                    ("handoff", &handoff_json(&handoff)),
                ])),
                None => Ok(object(&[
                    ("ok", &json_bool(true)),
                    ("found", &json_bool(false)),
                    ("handoff", &"null".to_string()),
                ])),
            }
        }

        other => Err(Failure::Fatal(format!(
            "unknown command {other:?} (acquire, diagnose, release, renew, active, list, task, ready, scope-append, health, serve, local-repository, evidence-append, evidence-list, requirements, review-append, reviews, gaps, gaps-clear, gaps-replace, gap-history, gap-upsert, run-record, runs, run-get, handoff, handoff-get, work)"
        ))),
    }
}

fn opt_json_str(value: Option<&str>) -> String {
    match value {
        Some(v) => quote(v),
        None => "null".to_string(),
    }
}

fn run_json(run: &dc_store::records::VerificationRun) -> String {
    object(&[
        ("id", &quote(&run.id)),
        ("task_id", &quote(&run.task_id)),
        ("sandbox", &quote(&run.sandbox)),
        ("environment_json", &quote(&run.environment_json)),
        ("commands_json", &quote(&run.commands_json)),
        ("status", &quote(&run.status)),
        ("started_at", &quote(&run.started_at)),
        ("finished_at", &opt_json_str(run.finished_at.as_deref())),
    ])
}

fn handoff_json(handoff: &dc_store::records::AgentHandoff) -> String {
    object(&[
        ("id", &quote(&handoff.id)),
        ("task_id", &quote(&handoff.task_id)),
        ("from_agent", &quote(&handoff.from_agent)),
        ("to_agent", &quote(&handoff.to_agent)),
        ("run_id", &quote(&handoff.run_id)),
        ("manifest_path", &quote(&handoff.manifest_path)),
        ("status", &quote(&handoff.status)),
        ("created_at", &quote(&handoff.created_at)),
    ])
}

// ---------------------------------------------------------------------------
// serve — many commands over one process
// ---------------------------------------------------------------------------
//
// The one-shot path above pays a fork and an exec per call, and measured from
// the Go client that was 2.1ms of a 3.9ms `diagnose` — more than half of every
// lease check on the write gate spent starting a process rather than answering.
// `serve` keeps the boundary exactly where it is (a separate process, no cgo,
// no shared library) and stops paying that cost per call: the Go client starts
// this once and sends it many requests.
//
// What it deliberately does not do is open the database more than once. The
// SQLite connection, its WAL mode check and its schema verification are the
// rest of that 3.9ms, and holding one open connection for the session is what
// turns the saving from "half" into "nearly all of it".

/// How many arguments one served request may carry.
///
/// The longest real request is `scope-append` with nine, so this is slack
/// rather than a limit anything approaches. It exists because the count is
/// read off the wire and used to size an allocation.
const MAX_SERVE_ARGS: usize = 64;

/// How many bytes one served argument may carry.
///
/// Matched to the 8 MiB cap the Go client puts on a reply, because the largest
/// value crossing in either direction is the same document — a task's appended
/// scope — and two different caps on one payload would mean a request the
/// sender considered legal and the receiver did not.
const MAX_SERVE_ARG_BYTES: usize = 8 << 20;

/// How many bytes one request may declare across all of its arguments, summed
/// as each length header arrives and checked before that argument's body is
/// read. The per-argument cap alone would let 64 arguments of 8 MiB each
/// through. Two arguments' worth is the most the Go host sends: every request
/// it builds carries at most one large value (an appended scope, a record, a
/// verification wire) beside short ids and tokens.
const MAX_SERVE_REQUEST_BYTES: usize = 2 * MAX_SERVE_ARG_BYTES;

/// How many bytes a length header may run to before it is refused. Lengths are
/// short decimal numbers; anything longer is a peer that will never send the
/// newline this is waiting for.
const MAX_SERVE_HEADER_BYTES: usize = 32;

/// Reads one newline-terminated header, bounded.
///
/// `Ok(None)` is a clean end of stream *between* requests — the Go client
/// closing stdin because it is done — and is the one way this session ends
/// without an error. A stream that stops mid-request is not that.
fn read_header(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut buf = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte) {
            Ok(0) => {
                if buf.is_empty() {
                    return Ok(None);
                }
                return Err("stream ended part way through a header".into());
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
                if buf.len() > MAX_SERVE_HEADER_BYTES {
                    return Err(format!(
                        "header exceeded {MAX_SERVE_HEADER_BYTES} bytes without a newline"
                    ));
                }
            }
            Err(e) => return Err(format!("reading a header: {e}")),
        }
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| "header is not UTF-8".to_string())
}

/// Reads one length header and parses it as a bounded count.
fn read_bounded_len(
    reader: &mut impl BufRead,
    limit: usize,
    what: &str,
) -> Result<Option<usize>, String> {
    let Some(header) = read_header(reader)? else {
        return Ok(None);
    };
    let value: usize = header
        .trim()
        .parse()
        .map_err(|_| format!("{what} header {header:?} is not a number"))?;
    if value > limit {
        return Err(format!("{what} declares {value}, over the {limit} cap"));
    }
    Ok(Some(value))
}

/// Reads one framed request: an argument count, then that many
/// length-prefixed arguments.
///
/// Lengths rather than delimiters because the values crossing here are not
/// line-shaped. A task's appended scope is JSON the store hands back and takes
/// in raw, and it may legally contain newlines; a wire that split on them would
/// silently cut such a request in half and run the front of it as a command.
fn read_request(reader: &mut impl BufRead) -> Result<Option<Vec<String>>, String> {
    let Some(count) = read_bounded_len(reader, MAX_SERVE_ARGS, "argument count")? else {
        return Ok(None);
    };
    let mut args = Vec::with_capacity(count);
    let mut declared_total = 0usize;
    for index in 0..count {
        let len = read_bounded_len(reader, MAX_SERVE_ARG_BYTES, &format!("argument {index}"))?
            .ok_or_else(|| format!("stream ended before argument {index}"))?;
        declared_total = declared_total
            .checked_add(len)
            .filter(|total| *total <= MAX_SERVE_REQUEST_BYTES)
            .ok_or_else(|| {
                format!(
                    "argument {index} takes the request past the {MAX_SERVE_REQUEST_BYTES} byte cap"
                )
            })?;
        // Memory follows the bytes that arrive, not the length that was
        // declared: a peer that declares 8 MiB and sends nothing costs nothing.
        let mut buf = Vec::new();
        reader
            .by_ref()
            .take(len as u64)
            .read_to_end(&mut buf)
            .map_err(|e| format!("reading argument {index}: {e}"))?;
        if buf.len() != len {
            return Err(format!(
                "reading argument {index}: stream ended after {} of {len} bytes",
                buf.len()
            ));
        }
        // Named by position and never by content: one of these arguments
        // carries a lease token, and an error echoing it would copy a
        // credential into the caller's logs. Same rule as collect_args.
        args.push(String::from_utf8(buf).map_err(|_| format!("argument {index} is not UTF-8"))?);
    }
    Ok(Some(args))
}

/// Writes one framed reply: a status, a byte count, then the document.
///
/// The status line carries what the exit code carries on the one-shot path.
/// Without it a served failure would arrive as a plain `{"ok":false}` and the
/// Go client's `Renew` — which reads `ok:false` as "the lease had already
/// expired" — would report a broken store as an expired lease.
fn write_reply(writer: &mut impl Write, status: &str, body: &str) -> std::io::Result<()> {
    write!(writer, "{status}\n{}\n", body.len())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

fn fatal_object(message: &str) -> String {
    object(&[("ok", &json_bool(false)), ("error", &quote(message))])
}

/// Maps a [`Failure`] onto the status its exit code would have carried.
fn failure_reply(failure: Failure) -> (&'static str, String) {
    match failure {
        // Contention is an outcome, not a fault — the same call the one-shot
        // path makes when it exits 0 on a held task.
        Failure::Conflict(json) => ("ok", json),
        Failure::Fatal(message) => ("err", fatal_object(&message)),
    }
}

/// Answers one served request against the session's store, opening it on first
/// use.
///
/// The open is lazy, and keyed on the first request's command, so that the
/// health rule survives being served: a session whose first request is
/// `health` must not bring the database into existence in order to answer it.
/// Opening eagerly at startup would have made every served health check report
/// a healthy empty store for a path nobody meant to type.
fn serve_one(store: &mut Option<Store>, db: &str, args: &[String]) -> (&'static str, String) {
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(failure) => return failure_reply(failure),
    };
    if parsed.db.is_some() {
        return (
            "err",
            fatal_object(
                "--db is fixed for a serve session and cannot be set per request; \
                 a request naming a different database would be answered from the \
                 one this session already opened",
            ),
        );
    }
    let Some(command) = parsed.command else {
        return ("err", fatal_object("a command is required"));
    };
    if command == "serve" {
        return ("err", fatal_object("serve cannot be nested"));
    }
    if store.is_none() {
        match open_for(&command, db, true) {
            Ok(opened) => *store = Some(opened),
            Err(failure) => return failure_reply(failure),
        }
    }
    let opened = store.as_ref().expect("the store was opened directly above");
    match dispatch(opened, &command, &parsed.flags) {
        Ok(json) => ("ok", json),
        Err(failure) => failure_reply(failure),
    }
}

/// Answers framed requests on stdin until the caller closes it.
fn serve(parsed: &Parsed) -> ExitCode {
    let Some(db) = parsed.db.as_deref() else {
        dc_proc::respond!("{}", fatal_object("--db is required"));
        return ExitCode::from(2);
    };
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = std::io::BufWriter::new(stdout.lock());
    let mut store: Option<Store> = None;

    loop {
        let request = match read_request(&mut reader) {
            Ok(None) => return ExitCode::SUCCESS,
            Ok(Some(args)) => args,
            Err(message) => {
                // Framing errors end the session rather than being answered and
                // stepped over. Once a declared length is wrong there is no
                // longer a known offset where the next request starts, so
                // continuing would run whatever the remaining bytes happened to
                // spell as a command.
                let _ = write_reply(
                    &mut writer,
                    "err",
                    &fatal_object(&format!("serve: {message}")),
                );
                return ExitCode::from(2);
            }
        };
        let (status, body) = serve_one(&mut store, db, &request);
        if write_reply(&mut writer, status, &body).is_err() {
            // The caller is gone. Nothing can be reported to it.
            return ExitCode::from(2);
        }
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
        ("worktree_path", &maybe(lease.worktree_path.as_deref())),
        ("worktree_id", &maybe(lease.worktree_id.as_deref())),
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
        // Derived from the task's gaps on every read. An explicit checkout of a
        // blocked task still works; this is how it says why it is blocked.
        ("blocking_gap_count", &t.blocking_gap_count.to_string()),
    ])
}

/// Text from a column the store embeds into its reply *unquoted*. A value that
/// is not provably one JSON value of the expected kind is never spliced in —
/// that is how a stored `[]}],"gaps":[],"x":[{"q":0` made two gaps decode as
/// none. The row is kept, `fallback` stands in for the value, and the reason
/// travels beside it so the reader reports the row instead of trusting it.
fn embed_raw(
    text: &str,
    check: fn(&str) -> Result<(), String>,
    fallback: &str,
) -> (String, Option<String>) {
    match check(text) {
        Ok(()) => (text.to_string(), None),
        Err(why) => (fallback.to_string(), Some(why)),
    }
}

/// Appends `"key": <reason>` to an already-built object when there is a reason.
fn with_error(mut object: String, key: &str, error: Option<String>) -> String {
    if let Some(why) = error {
        object.pop();
        object.push_str(&format!(",{}:{}}}", quote(key), quote(&why)));
    }
    object
}

/// The leases a sweep could not judge, each with its expiry as stored.
fn unreadable_json(leases: &[Lease]) -> String {
    let items: Vec<String> = leases
        .iter()
        .map(|l| {
            object(&[
                ("task_id", &quote(&l.task_id)),
                ("lease_id", &quote(&l.id)),
                ("expires_at", &maybe(l.expires_at.as_deref())),
            ])
        })
        .collect();
    format!("[{}]", items.join(","))
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

#[cfg(test)]
mod serve_frame_tests {
    use super::*;

    /// One stretch of a scripted request stream.
    enum Seg {
        Bytes(Vec<u8>),
        /// `n` zero bytes, produced on demand so a declared-but-large body
        /// costs the test nothing until the code under test asks for it.
        Zeros(usize),
        /// Reading this far is the failure: the code under test went past a
        /// point where it had already been obliged to refuse.
        Tripwire,
    }

    struct Scripted {
        segs: std::collections::VecDeque<Seg>,
        consumed: usize,
        max_read_request: usize,
    }

    impl Scripted {
        fn new(segs: Vec<Seg>) -> Self {
            Scripted {
                segs: segs.into(),
                consumed: 0,
                max_read_request: 0,
            }
        }
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.max_read_request = self.max_read_request.max(buf.len());
            let Some(seg) = self.segs.front_mut() else {
                return Ok(0);
            };
            let n = match seg {
                Seg::Tripwire => {
                    panic!("the request was read past the point it should have been refused")
                }
                Seg::Bytes(bytes) => {
                    let n = bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&bytes[..n]);
                    bytes.drain(..n);
                    if bytes.is_empty() {
                        self.segs.pop_front();
                    }
                    n
                }
                Seg::Zeros(left) => {
                    let n = (*left).min(buf.len());
                    buf[..n].fill(0);
                    *left -= n;
                    if *left == 0 {
                        self.segs.pop_front();
                    }
                    n
                }
            };
            self.consumed += n;
            Ok(n)
        }
    }

    impl BufRead for Scripted {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            Err(std::io::Error::other(
                "read_request reads through Read only",
            ))
        }
        fn consume(&mut self, _amt: usize) {}
    }

    fn header(n: usize) -> Seg {
        Seg::Bytes(format!("{n}\n").into_bytes())
    }

    #[test]
    fn total_over_the_cap_is_refused_before_that_body_is_read() {
        let mut reader = Scripted::new(vec![
            header(3),
            header(MAX_SERVE_ARG_BYTES),
            Seg::Zeros(MAX_SERVE_ARG_BYTES),
            header(MAX_SERVE_ARG_BYTES),
            Seg::Zeros(MAX_SERVE_ARG_BYTES),
            header(1),
            Seg::Tripwire,
        ]);
        let err =
            read_request(&mut reader).expect_err("a third argument pushes the total over the cap");
        assert!(
            err.contains("takes the request past the") && err.contains("byte cap"),
            "{err}"
        );
    }

    #[test]
    fn many_arguments_each_under_the_cap_are_refused_in_total() {
        let mut segs = vec![header(MAX_SERVE_ARGS)];
        for _ in 0..MAX_SERVE_ARGS {
            segs.push(header(1 << 20));
            segs.push(Seg::Zeros(1 << 20));
        }
        let mut reader = Scripted::new(segs);
        let err = read_request(&mut reader).expect_err("64 MiB declared across a request");
        assert!(
            err.contains("takes the request past the") && err.contains("byte cap"),
            "{err}"
        );
        assert!(
            reader.consumed <= MAX_SERVE_REQUEST_BYTES + 1024,
            "read {} bytes of a request refused at {}",
            reader.consumed,
            MAX_SERVE_REQUEST_BYTES
        );
    }

    #[test]
    fn a_declared_length_is_not_allocated_ahead_of_the_bytes() {
        let mut reader = Scripted::new(vec![
            header(1),
            header(MAX_SERVE_ARG_BYTES),
            Seg::Bytes(b"abc".to_vec()),
        ]);
        let err =
            read_request(&mut reader).expect_err("the stream ends 3 bytes into an 8 MiB argument");
        assert!(
            err.contains("argument 0") && err.contains("stream ended after 3 of"),
            "{err}"
        );
        // The invariant is that no read is sized to the declared length; how
        // much smaller each read is belongs to std and is not asserted.
        assert!(
            reader.max_read_request < MAX_SERVE_ARG_BYTES,
            "a read asked for {} bytes against 3 sent",
            reader.max_read_request
        );
    }

    #[test]
    fn an_eight_mib_scope_append_still_passes() {
        let mut reader = Scripted::new(vec![
            header(2),
            header(12),
            Seg::Bytes(b"scope-append".to_vec()),
            header(MAX_SERVE_ARG_BYTES),
            Seg::Zeros(MAX_SERVE_ARG_BYTES),
        ]);
        let args = read_request(&mut reader)
            .expect("an 8 MiB argument is legal")
            .expect("a request, not end of stream");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "scope-append");
        assert_eq!(args[1].len(), MAX_SERVE_ARG_BYTES);
    }

    fn run(segs: Vec<Seg>) -> Result<Option<Vec<String>>, String> {
        read_request(&mut Scripted::new(segs))
    }

    fn raw(s: &str) -> Seg {
        Seg::Bytes(s.as_bytes().to_vec())
    }

    #[test]
    fn adversarial_total_exactly_at_the_cap_passes_and_one_over_is_refused() {
        let args = run(vec![
            header(2),
            header(MAX_SERVE_ARG_BYTES),
            Seg::Zeros(MAX_SERVE_ARG_BYTES),
            header(MAX_SERVE_ARG_BYTES),
            Seg::Zeros(MAX_SERVE_ARG_BYTES),
        ])
        .expect("exactly 16 MiB is legal")
        .expect("a request");
        assert_eq!(
            args.iter().map(String::len).sum::<usize>(),
            MAX_SERVE_REQUEST_BYTES
        );

        let err = run(vec![
            header(3),
            header(MAX_SERVE_ARG_BYTES),
            Seg::Zeros(MAX_SERVE_ARG_BYTES),
            header(MAX_SERVE_ARG_BYTES),
            Seg::Zeros(MAX_SERVE_ARG_BYTES),
            header(1),
            Seg::Tripwire,
        ])
        .expect_err("16 MiB + 1 is over");
        assert!(
            err.contains("argument 2 takes the request past the"),
            "{err}"
        );
    }

    #[test]
    fn adversarial_zero_length_arguments_at_the_count_cap_pass() {
        let mut segs = vec![header(MAX_SERVE_ARGS)];
        segs.extend((0..MAX_SERVE_ARGS).map(|_| header(0)));
        let args = run(segs).expect("64 empty arguments").expect("a request");
        assert_eq!(args.len(), MAX_SERVE_ARGS);
        assert!(args.iter().all(String::is_empty));
        let empty = run(vec![header(0)])
            .expect("zero arguments")
            .expect("a request");
        assert!(empty.is_empty());
    }

    #[test]
    fn adversarial_huge_argument_count_is_refused_before_any_allocation_or_read() {
        for count in [
            "1000000000",
            "65",
            "18446744073709551615",
            "18446744073709551616",
        ] {
            let err =
                run(vec![raw(&format!("{count}\n")), header(0), Seg::Tripwire]).expect_err(count);
            assert!(err.contains("argument count"), "{count}: {err}");
        }
    }

    #[test]
    fn adversarial_argument_over_the_per_argument_cap_is_refused_first() {
        for len in [MAX_SERVE_ARG_BYTES + 1, usize::MAX, usize::MAX - 1] {
            let err = run(vec![header(1), header(len), Seg::Tripwire]).expect_err("over the cap");
            assert!(
                err.contains("argument 0 declares") && err.contains("cap"),
                "{err}"
            );
            assert!(
                !err.contains("request past"),
                "per-argument cap comes first: {err}"
            );
        }
    }

    #[test]
    fn adversarial_malformed_lengths_are_refused() {
        for bad in [
            "-1",
            "0x10",
            "1e3",
            "5 5",
            "",
            "   ",
            "1.0",
            "18446744073709551616",
            "99999999999999999999999",
            "\u{0661}",
            "\0",
        ] {
            let err = run(vec![header(1), raw(&format!("{bad}\n")), Seg::Tripwire]).expect_err(bad);
            assert!(err.contains("is not a number"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn adversarial_lenient_length_spellings_are_accepted_and_still_bounded() {
        // Pinned current behaviour: leading zeros, a plus sign, padding and CRLF
        // all parse to the same number, so none of them can smuggle a larger
        // value past the caps.
        for spelling in [
            "00000000000000000000000000000003",
            "+3",
            "  3 ",
            "3\r",
            "\t3",
        ] {
            let args = run(vec![raw("1\n"), raw(&format!("{spelling}\n")), raw("abc")])
                .unwrap_or_else(|e| panic!("{spelling:?}: {e}"))
                .expect("a request");
            assert_eq!(args, vec!["abc".to_string()], "{spelling:?}");
        }
        let err = run(vec![header(1), raw("+16777217\n"), Seg::Tripwire]).expect_err("over cap");
        assert!(err.contains("over the"), "{err}");
    }

    #[test]
    fn adversarial_crlf_framing_keeps_body_bytes_verbatim() {
        let args = run(vec![
            raw("2\r\n4\r\n"),
            raw("a\r\nb"),
            raw("1\r\n"),
            raw("x"),
        ])
        .expect("CRLF headers")
        .expect("a request");
        assert_eq!(args, vec!["a\r\nb".to_string(), "x".to_string()]);
    }

    #[test]
    fn adversarial_header_line_is_bounded_without_a_newline() {
        let at = "0".repeat(MAX_SERVE_HEADER_BYTES);
        assert!(
            run(vec![raw(&format!("{at}\n"))])
                .expect("32 bytes is the limit")
                .is_some()
        );
        let over = "0".repeat(MAX_SERVE_HEADER_BYTES + 1);
        let err = run(vec![raw(&over), Seg::Tripwire]).expect_err("33 bytes, no newline");
        assert!(err.contains("without a newline"), "{err}");
        let err = run(vec![header(1), raw(&over), Seg::Tripwire]).expect_err("same on an argument");
        assert!(err.contains("without a newline"), "{err}");
    }

    #[test]
    fn adversarial_end_of_stream_in_each_position() {
        assert_eq!(run(vec![]).expect("clean end between requests"), None);
        let err = run(vec![raw("2")]).expect_err("EOF inside the count header");
        assert!(err.contains("part way through a header"), "{err}");
        let err = run(vec![header(2)]).expect_err("EOF before argument 0");
        assert!(err.contains("stream ended before argument 0"), "{err}");
        let err = run(vec![header(1), raw("4")]).expect_err("EOF inside an argument header");
        assert!(err.contains("part way through a header"), "{err}");
        let err = run(vec![header(2), header(1), raw("a")]).expect_err("EOF before argument 1");
        assert!(err.contains("stream ended before argument 1"), "{err}");
        let err = run(vec![header(1), header(5), raw("ab")]).expect_err("EOF inside a body");
        assert!(err.contains("stream ended after 2 of 5 bytes"), "{err}");
    }

    #[test]
    fn adversarial_invalid_utf8_is_a_clean_error_that_does_not_echo_the_bytes() {
        let err = run(vec![
            header(2),
            header(1),
            raw("a"),
            header(4),
            Seg::Bytes(vec![b'T', 0xff, 0xfe, b'K']),
        ])
        .expect_err("invalid UTF-8 body");
        assert_eq!(err, "argument 1 is not UTF-8");
        let err = run(vec![Seg::Bytes(vec![b'1', 0xff, b'\n'])]).expect_err("invalid UTF-8 header");
        assert_eq!(err, "header is not UTF-8");
    }

    #[test]
    fn adversarial_a_refusal_leaves_the_stream_mid_frame_so_serve_must_close() {
        // read_request does not resync: after a refusal the next bytes are
        // still the refused frame's body. serve() returns on any Err for this
        // reason; the test pins that the reader is not silently advanced to a
        // following request.
        let mut reader = Scripted::new(vec![
            header(1),
            header(MAX_SERVE_ARG_BYTES + 1),
            raw("1\n1\nx"),
        ]);
        read_request(&mut reader).expect_err("over the cap");
        let next = read_request(&mut reader).expect("the leftover parses as a frame");
        assert_eq!(next, Some(vec!["x".to_string()]));
    }

    #[test]
    fn adversarial_refusal_reply_keeps_the_serve_error_shape() {
        let err = run(vec![header(65)]).expect_err("count over cap");
        let mut out = Vec::new();
        write_reply(&mut out, "err", &fatal_object(&format!("serve: {err}"))).unwrap();
        let text = String::from_utf8(out).unwrap();
        let (status, rest) = text.split_once('\n').unwrap();
        let (len, body) = rest.split_once('\n').unwrap();
        assert_eq!(status, "err");
        assert_eq!(len.parse::<usize>().unwrap(), body.len());
        assert_eq!(
            body,
            "{\"ok\":false,\"error\":\"serve: argument count declares 65, over the 64 cap\"}"
        );
    }
}
