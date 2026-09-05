//! Adversarial stress against the real `gitpulse-mcp` process.
//!
//! `mcp_stdio.rs` proves the happy path over the wire. This file exists for
//! everything a hostile or broken client does instead, because each case below
//! is a measured failure of the previous `for line in stdin.lock().lines()`
//! loop, not a hypothetical:
//!
//! * one non-UTF-8 byte ended the loop and the process exited **0**;
//! * 600 MiB of newline-free input grew the process to 616 MiB resident;
//! * a request that failed struct validation was answered `id: null`, so the
//!   client's pending entry for the real id never resolved;
//! * one slow call blocked every other request for as long as it ran.
//!
//! The invariants asserted throughout: **stdout carries nothing but one
//! JSON-RPC message per line**, **every request carrying an id is answered
//! exactly once**, and **no input kills the server**.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

fn mcp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gitpulse-mcp")
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "gitpulse-stress", "version": "0" },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn request(id: Value, method: &str, mut params: Value) -> String {
    if !params.is_object() {
        params = json!({});
    }
    params
        .as_object_mut()
        .expect("object")
        .insert("_meta".into(), modern_meta());
    format!(
        "{}\n",
        serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))
        .expect("request")
    )
}

/// A live server, with its log directory kept alive for the process's lifetime.
///
/// stdout is drained by a thread started with the process, not read at the end.
/// A pipe holds about 64 KiB: a test that writes its whole input before reading
/// anything deadlocks as soon as the responses exceed that — the server blocks
/// writing, so it stops reading stdin, so the test blocks writing. That is a
/// flaw in the harness rather than the server, and it hides real failures
/// behind a hang.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    drain: Option<std::thread::JoinHandle<Vec<String>>>,
    _log_dir: tempfile::TempDir,
}

impl Server {
    /// Spawn a server and drain its stdout on a background thread.
    fn start(env: &[(&str, &str)]) -> Self {
        let (mut server, stdout) = Self::spawn(env);
        server.drain = Some(std::thread::spawn(move || {
            BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
                .filter(|line| !line.is_empty())
                .collect()
        }));
        server
    }

    /// Spawn a server whose stdout the caller reads itself.
    ///
    /// Only for the cases that must observe response *timing*, or close the
    /// read end while the server is still running — neither is expressible
    /// through the drain. Everything else uses [`Server::start`], because a
    /// test that forgets to read stdout concurrently deadlocks rather than
    /// failing.
    fn start_undrained(env: &[(&str, &str)]) -> (Self, std::process::ChildStdout) {
        Self::spawn(env)
    }

    fn spawn(env: &[(&str, &str)]) -> (Self, std::process::ChildStdout) {
        let log_dir = tempfile::tempdir().expect("log dir");
        let mut command = Command::new(mcp_bin());
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GITPULSE_LOG_DIR", log_dir.path());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", mcp_bin()));
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        (
            Self {
                child,
                stdin: Some(stdin),
                drain: None,
                _log_dir: log_dir,
            },
            stdout,
        )
    }

    fn write(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(bytes).expect("write");
        stdin.flush().expect("flush");
    }

    /// Close stdin — the client's documented shutdown signal — and collect every
    /// line the server wrote, asserting each is one complete JSON value.
    fn finish(mut self) -> (Vec<Value>, Option<i32>) {
        drop(self.stdin.take());
        let raw = self
            .drain
            .take()
            .expect("stdout drain")
            .join()
            .expect("stdout drain thread");
        let status = self.child.wait().expect("the server did not exit").code();
        let lines = raw
            .into_iter()
            .map(|line| {
                let value: Value = serde_json::from_str(&line).unwrap_or_else(|e| {
                    panic!("stdout carried a line that is not JSON-RPC ({e}): {line:?}")
                });
                assert_eq!(value["jsonrpc"], "2.0", "not a JSON-RPC message: {line}");
                value
            })
            .collect();
        (lines, status)
    }
}

/// Drive one exchange: write `input` verbatim, close stdin, collect responses.
fn exchange(input: &[u8]) -> Vec<Value> {
    let mut server = Server::start(&[]);
    server.write(input);
    let (responses, status) = server.finish();
    assert_eq!(
        status,
        Some(0),
        "closing stdin is a clean shutdown and must exit 0"
    );
    responses
}

fn by_id(responses: &[Value], id: &Value) -> Vec<Value> {
    responses
        .iter()
        .filter(|r| &r["id"] == id)
        .cloned()
        .collect()
}

fn error_code(response: &Value) -> i64 {
    response["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("not an error response: {response}"))
}

// ---------------------------------------------------------------------------
// Framing faults: one bad message must never end the session
// ---------------------------------------------------------------------------

#[test]
fn one_invalid_utf8_byte_does_not_end_the_session() {
    // The measured regression: `lines()` yields Err on a non-UTF-8 byte, the
    // loop treated it as EOF, and the third request was never seen.
    let mut input = request(json!(1), "tools/list", json!({})).into_bytes();
    input.extend_from_slice(&[0xff, 0xfe, b'\n']);
    input.extend_from_slice(request(json!(3), "server/discover", json!({})).as_bytes());

    let responses = exchange(&input);
    assert_eq!(by_id(&responses, &json!(1)).len(), 1, "{responses:?}");
    assert_eq!(
        by_id(&responses, &json!(3)).len(),
        1,
        "the request behind the bad byte was discarded: {responses:?}"
    );
    let parse_errors: Vec<_> = responses
        .iter()
        .filter(|r| r["error"]["code"] == json!(-32700))
        .collect();
    assert_eq!(parse_errors.len(), 1, "the bad frame was not reported");
}

#[test]
fn an_oversized_frame_is_refused_without_ending_the_session() {
    // 8 MiB against the 4 MiB frame cap, with a real request behind it.
    let mut input = vec![b'x'; 8 * 1024 * 1024];
    input.push(b'\n');
    input.extend_from_slice(request(json!(2), "tools/list", json!({})).as_bytes());

    let responses = exchange(&input);
    assert_eq!(
        by_id(&responses, &json!(2)).len(),
        1,
        "the request behind the oversized frame was lost: {responses:?}"
    );
    assert!(
        responses.iter().any(|r| r["error"]["code"] == json!(-32700)
            && r["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("frame cap"))),
        "the oversized frame was not reported: {responses:?}"
    );
}

#[test]
fn input_with_no_newline_at_all_does_not_grow_the_server_without_bound() {
    // The failure this guards is memory, which a test cannot assert portably.
    // What it can assert is the observable consequence of the cap: the server
    // stops accumulating, reports the frame, and answers the request that
    // follows it. Without the cap there is no report and no answer — the
    // process is still buffering.
    let mut input = vec![b'x'; 6 * 1024 * 1024];
    input.push(b'\n');
    input.extend_from_slice(request(json!("after"), "tools/list", json!({})).as_bytes());
    let responses = exchange(&input);
    assert_eq!(by_id(&responses, &json!("after")).len(), 1, "{responses:?}");
}

#[test]
fn blank_lines_and_whitespace_are_skipped_rather_than_answered() {
    let mut input = b"\n\n   \n\t\n".to_vec();
    input.extend_from_slice(request(json!(9), "tools/list", json!({})).as_bytes());
    input.extend_from_slice(b"\n\n");
    let responses = exchange(&input);
    assert_eq!(
        responses.len(),
        1,
        "blank lines produced answers: {responses:?}"
    );
    assert_eq!(responses[0]["id"], json!(9));
}

#[test]
fn a_windows_client_sending_crlf_is_understood() {
    let line = request(json!(4), "tools/list", json!({}));
    let input = line.trim_end().to_string() + "\r\n";
    let responses = exchange(input.as_bytes());
    assert_eq!(by_id(&responses, &json!(4)).len(), 1, "{responses:?}");
}

// ---------------------------------------------------------------------------
// Malformed requests: the id must survive, and the code must be honest
// ---------------------------------------------------------------------------

#[test]
fn a_request_that_fails_struct_validation_keeps_its_id_and_is_invalid_request() {
    // The measured regression: this was answered `id: null, code: -32700`, so
    // the client's pending entry for id 42 hung until its own timeout, and
    // well-formed JSON was reported as a *parse* error.
    let input = b"{\"id\":42,\"method\":\"tools/list\",\"params\":{}}\n";
    let responses = exchange(input);
    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_eq!(responses[0]["id"], json!(42), "the id was lost");
    assert_eq!(error_code(&responses[0]), -32600);
}

#[test]
fn unparseable_json_is_a_parse_error_and_the_session_survives() {
    let mut input = b"{not json at all\n".to_vec();
    input.extend_from_slice(request(json!(5), "tools/list", json!({})).as_bytes());
    let responses = exchange(&input);
    assert_eq!(error_code(&responses[0]), -32700);
    assert_eq!(by_id(&responses, &json!(5)).len(), 1);
}

#[test]
fn a_json_rpc_batch_is_refused_with_an_explanation_not_silence() {
    // MCP has no batching. Answering `id: null` would leave every id in the
    // batch unresolved with nothing to act on.
    let responses = exchange(b"[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}]\n");
    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_eq!(error_code(&responses[0]), -32600);
    assert!(responses[0]["error"]["message"]
        .as_str()
        .is_some_and(|m| m.contains("batching")));
}

#[test]
fn a_wrong_protocol_version_field_is_an_invalid_request() {
    let responses =
        exchange(b"{\"jsonrpc\":\"1.0\",\"id\":7,\"method\":\"tools/list\",\"params\":{}}\n");
    assert_eq!(responses[0]["id"], json!(7));
    assert_eq!(error_code(&responses[0]), -32600);
}

#[test]
fn a_null_id_is_refused_rather_than_silently_dropped() {
    // JSON-RPC in MCP forbids a null id. Dropping it left the client waiting
    // on a response that was never going to come.
    let responses =
        exchange(b"{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"tools/list\",\"params\":{}}\n");
    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_eq!(error_code(&responses[0]), -32600);
}

#[test]
fn a_malformed_notification_is_answered_with_silence() {
    // No id means a notification, and JSON-RPC forbids responding to one even
    // when it is malformed.
    let mut input = b"{\"method\":\"tools/list\"}\n".to_vec();
    input.extend_from_slice(request(json!(8), "tools/list", json!({})).as_bytes());
    let responses = exchange(&input);
    assert_eq!(
        responses.len(),
        1,
        "a notification was answered: {responses:?}"
    );
    assert_eq!(responses[0]["id"], json!(8));
}

// ---------------------------------------------------------------------------
// Availability: a slow call must not take the server with it
// ---------------------------------------------------------------------------

#[test]
fn a_request_past_its_budget_is_answered_and_the_server_keeps_serving() {
    // A 1 ms budget makes any real tool call overrun deterministically. What
    // matters is not the timeout itself but what follows it: the client gets an
    // answer, and the next request is served rather than queued behind work
    // that is still running.
    let mut server = Server::start(&[("GITPULSE_MCP_CALL_TIMEOUT_MS", "1")]);
    server.write(
        request(
            json!("slow"),
            "tools/call",
            json!({ "name": "gitpulse_insights", "arguments": { "repo_path": "/tmp" } }),
        )
        .as_bytes(),
    );
    std::thread::sleep(Duration::from_millis(300));
    server.write(request(json!("fast"), "tools/list", json!({})).as_bytes());

    let (responses, status) = server.finish();
    assert_eq!(status, Some(0));
    let slow = by_id(&responses, &json!("slow"));
    assert_eq!(
        slow.len(),
        1,
        "the slow call was answered {} times",
        slow.len()
    );
    assert_eq!(
        by_id(&responses, &json!("fast")).len(),
        1,
        "the fast call was never answered: {responses:?}"
    );
}

#[test]
fn a_slow_call_does_not_delay_the_answer_to_a_fast_one() {
    // Head-of-line blocking, measured: with a sequential loop a trivial
    // `tools/list` queued behind a stalled tool call waited for the whole of it.
    // With a 400 ms budget on the slow call, the fast answer must arrive well
    // before that budget expires.
    let (mut server, stdout) = Server::start_undrained(&[("GITPULSE_MCP_CALL_TIMEOUT_MS", "400")]);
    let mut reader = BufReader::new(stdout);

    server.write(
        request(
            json!("slow"),
            "tools/call",
            json!({ "name": "gitpulse_insights", "arguments": { "repo_path": "/tmp" } }),
        )
        .as_bytes(),
    );
    server.write(request(json!("fast"), "tools/list", json!({})).as_bytes());

    let started = Instant::now();
    let mut first = String::new();
    reader.read_line(&mut first).expect("a first response");
    let elapsed = started.elapsed();
    let first: Value = serde_json::from_str(first.trim()).expect("JSON");

    drop(server.stdin.take());
    let _ = server.child.wait();

    assert_eq!(
        first["id"],
        json!("fast"),
        "the fast request did not overtake the slow one: {first}"
    );
    assert!(
        elapsed < Duration::from_millis(400),
        "the fast answer waited {elapsed:?} — the loop is still serialized"
    );
}

#[test]
fn every_request_is_answered_exactly_once_under_a_pipelined_burst() {
    // 200 requests written without waiting for any answer. Two invariants:
    // nothing is lost, and nothing is answered twice — a duplicate would break
    // any client that correlates by id.
    let mut server = Server::start(&[]);
    let mut expected = Vec::new();
    let mut input = Vec::new();
    for id in 0..200u32 {
        let method = match id % 4 {
            0 => "tools/list",
            1 => "resources/list",
            2 => "prompts/list",
            _ => "server/discover",
        };
        input.extend_from_slice(request(json!(id), method, json!({})).as_bytes());
        expected.push(json!(id));
    }
    server.write(&input);
    let (responses, status) = server.finish();
    assert_eq!(status, Some(0));

    for id in &expected {
        let found = by_id(&responses, id);
        assert_eq!(found.len(), 1, "id {id} was answered {} times", found.len());
        assert!(found[0]["result"].is_object(), "id {id}: {:?}", found[0]);
    }
    assert_eq!(responses.len(), expected.len());
}

#[test]
fn the_server_exits_when_its_client_stops_reading() {
    // The measured regression: with the write error discarded, a server whose
    // client had gone kept executing requests and spawning git for nobody.
    let (mut server, stdout) = Server::start_undrained(&[]);
    // Read one response so the pipe is definitely established, then drop the
    // read end while leaving stdin open.
    let mut reader = BufReader::new(stdout);
    server.write(request(json!(1), "tools/list", json!({})).as_bytes());
    let mut first = String::new();
    reader.read_line(&mut first).expect("first response");
    drop(reader);

    // Keep feeding it. A server that ignores broken writes never stops.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut exited = None;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = server.child.try_wait() {
            exited = Some(status);
            break;
        }
        if server
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(request(json!(2), "tools/list", json!({})).as_bytes())
            .is_err()
        {
            // The child is gone and the OS tore the pipe down.
            exited = server.child.wait().ok();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = server.child.kill();
    assert!(
        exited.is_some(),
        "the server kept running after its client closed the read end"
    );
}

// ---------------------------------------------------------------------------
// Fuzzing: no input may kill the server or corrupt the stream
// ---------------------------------------------------------------------------

/// A tiny deterministic PRNG. A seeded generator keeps a failure reproducible,
/// which a random one would not.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn no_mutated_request_can_kill_the_server_or_put_non_json_on_stdout() {
    // Structure-aware mutation: start from a valid request and corrupt one
    // field at a time, so the fuzzer spends its budget on inputs that reach the
    // dispatch rather than on garbage the parser rejects immediately.
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let fields: &[&str] = &["jsonrpc", "id", "method", "params"];
    let poisons: &[Value] = &[
        json!(null),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(""),
        json!("   "),
        json!("../../etc/passwd"),
        json!(true),
        json!([]),
        json!({}),
        json!("𝕏\u{0}\u{1}"),
        json!("x".repeat(9000)),
        json!(u64::MAX),
        json!(i64::MIN),
    ];
    let methods: &[&str] = &[
        "tools/list",
        "tools/call",
        "resources/read",
        "resources/list",
        "resources/templates/list",
        "prompts/get",
        "prompts/list",
        "completion/complete",
        "server/discover",
        "initialize",
        "ping",
        "notifications/cancelled",
        "../../escape",
    ];

    // Ids start high so that no poison value below can collide with one: an
    // `id: 0` produced by poisoning must not look like a duplicate answer to
    // the legitimate request numbered 0.
    const ID_BASE: u64 = 1_000_000;
    let mut input = Vec::new();
    let mut ids = Vec::new();
    for i in ID_BASE..ID_BASE + 400 {
        let mut message = json!({
            "jsonrpc": "2.0",
            "id": i,
            "method": methods[rng.below(methods.len())],
            "params": {
                "_meta": modern_meta(),
                "name": "gitpulse_insights",
                "arguments": { "repo_path": "/tmp" },
                "uri": "gitpulse://insights/tmp",
                "cursor": "not-a-cursor",
            }
        });
        // Poison one to three fields.
        for _ in 0..=rng.below(3) {
            let field = fields[rng.below(fields.len())];
            let poison = poisons[rng.below(poisons.len())].clone();
            message[field] = poison;
        }
        if message["id"] == json!(i) {
            ids.push(json!(i));
        }
        input.extend_from_slice(serde_json::to_string(&message).expect("json").as_bytes());
        input.push(b'\n');
    }

    let mut server = Server::start(&[("GITPULSE_MCP_CALL_TIMEOUT_MS", "5000")]);
    server.write(&input);
    // A final well-formed request proves the server survived everything above.
    server.write(request(json!("sentinel"), "tools/list", json!({})).as_bytes());
    let (responses, status) = server.finish();

    assert_eq!(
        status,
        Some(0),
        "the fuzz corpus took the server down (exit {status:?})"
    );
    assert_eq!(
        by_id(&responses, &json!("sentinel")).len(),
        1,
        "the server stopped answering before the sentinel"
    );
    // `Server::finish` already asserted every stdout line is one JSON-RPC
    // message; this pins the other half of the contract.
    for response in &responses {
        assert!(
            response.get("result").is_some() ^ response.get("error").is_some(),
            "a response carried both or neither: {response}"
        );
    }
    for id in &ids {
        assert!(
            by_id(&responses, id).len() <= 1,
            "id {id} was answered more than once"
        );
    }
}

#[test]
fn no_random_byte_stream_can_kill_the_server() {
    // Not structure-aware: raw bytes, including invalid UTF-8, NULs and
    // newlines in arbitrary places. The bar is only that the server survives
    // and still speaks JSON-RPC afterwards.
    let mut rng = Rng(0xD00D_F00D_0000_0007);
    let mut input = Vec::with_capacity(200_000);
    for _ in 0..200_000 {
        let byte = (rng.next() % 256) as u8;
        input.push(byte);
    }
    input.push(b'\n');

    let mut server = Server::start(&[]);
    server.write(&input);
    server.write(request(json!("sentinel"), "server/discover", json!({})).as_bytes());
    let (responses, status) = server.finish();

    assert_eq!(status, Some(0), "random bytes took the server down");
    assert_eq!(
        by_id(&responses, &json!("sentinel")).len(),
        1,
        "the server stopped answering after random input"
    );
}

#[test]
fn a_deeply_nested_params_object_is_refused_rather_than_overflowing_the_stack() {
    // serde_json has a recursion limit; the point is that hitting it is a parse
    // error on one message, not a stack overflow that kills the process.
    let depth = 5_000;
    let mut nested =
        String::from("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":");
    nested.push_str(&"[".repeat(depth));
    nested.push_str(&"]".repeat(depth));
    nested.push_str("}\n");

    let mut input = nested.into_bytes();
    input.extend_from_slice(request(json!("after"), "tools/list", json!({})).as_bytes());
    let responses = exchange(&input);
    assert_eq!(
        by_id(&responses, &json!("after")).len(),
        1,
        "deep nesting ended the session: {responses:?}"
    );
}

// ---------------------------------------------------------------------------
// Stream discipline
// ---------------------------------------------------------------------------

#[test]
fn stdout_carries_one_message_per_line_and_nothing_else() {
    // The spec is explicit: messages are newline-delimited and MUST NOT contain
    // embedded newlines, and the server MUST NOT write anything to stdout that
    // is not a valid MCP message. Tool payloads are pretty-printed JSON full of
    // newlines, so this is a live risk, not a theoretical one.
    let (mut server, stdout) = Server::start_undrained(&[]);
    for (i, (method, params)) in [
        ("server/discover", json!({})),
        ("tools/list", json!({})),
        (
            "resources/read",
            json!({ "uri": "gitpulse://server/manifest" }),
        ),
        (
            "tools/call",
            json!({ "name": "gitpulse_insights", "arguments": { "repo_path": "/tmp" } }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        server.write(request(json!(i as u32), method, params).as_bytes());
    }
    drop(server.stdin.take());
    let mut raw = Vec::new();
    let mut stdout = stdout;
    stdout.read_to_end(&mut raw).expect("read stdout");
    let _ = server.child.wait();

    let text = String::from_utf8(raw).expect("stdout is UTF-8");
    let mut seen = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        serde_json::from_str::<Value>(trimmed)
            .unwrap_or_else(|e| panic!("stdout line is not one JSON value ({e}): {trimmed:?}"));
        seen += 1;
    }
    assert_eq!(seen, 4, "expected one line per request, got {seen}");
}

#[test]
fn the_stderr_log_never_leaks_into_stdout() {
    // `logging::init` writes stderr and a log file. A single stray byte on
    // stdout makes every MCP client fail to parse the first response, so this
    // pins the separation with the log level turned all the way up.
    let mut server = Server::start(&[("RUST_LOG", "trace"), ("GITPULSE_LOG_LEVEL", "trace")]);
    server.write(request(json!(1), "server/discover", json!({})).as_bytes());
    let (responses, status) = server.finish();
    assert_eq!(status, Some(0));
    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_eq!(responses[0]["id"], json!(1));
}
