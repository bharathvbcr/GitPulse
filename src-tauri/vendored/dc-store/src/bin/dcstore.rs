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

use std::io::{BufRead, BufReader, Write};
use std::process::ExitCode;

use dc_store::{AcquireRequest, Lease, LeaseCode, ScopeWrite, Store, StoreError};

fn main() -> ExitCode {
    let args = match collect_args() {
        Ok(args) => args,
        Err(message) => {
            println!("{}", fatal_object(&message));
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
            println!("{}", fatal_object(&message));
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
fn open_for(command: &str, db: &str) -> Result<Store, Failure> {
    if command == "health" {
        Store::open_existing(db)
    } else {
        Store::open(db)
    }
    .map_err(|e| Failure::Fatal(e.to_string()))
}

fn run(args: &[String]) -> Result<String, Failure> {
    let parsed = parse(args)?;
    let db = parsed
        .db
        .ok_or_else(|| Failure::Fatal("--db is required".into()))?;
    let command = parsed
        .command
        .ok_or_else(|| Failure::Fatal("a command is required".into()))?;
    let store = open_for(&command, &db)?;

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
            "unknown command {other:?} (acquire, diagnose, release, renew, active, list, task, ready, scope-append, health, serve)"
        ))),
    }
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
    for index in 0..count {
        let len = read_bounded_len(reader, MAX_SERVE_ARG_BYTES, &format!("argument {index}"))?
            .ok_or_else(|| format!("stream ended before argument {index}"))?;
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("reading argument {index}: {e}"))?;
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
        match open_for(&command, db) {
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
        println!("{}", fatal_object("--db is required"));
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
