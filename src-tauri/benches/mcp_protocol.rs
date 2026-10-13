//! Where the MCP surface spends its time.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench mcp_protocol`
//! (add `-- --smoke` for 3 samples per row, `-- --json out.json` for the
//! machine-readable report). Deliberately not a Criterion benchmark: Criterion
//! would be a new dependency, and the questions here are coarse — "is the
//! protocol layer free relative to the work it wraps, and did a fix that was
//! supposed to be cheap stay cheap". It runs on the shared harness in
//! `benches/common`: interleaved rounds, min/p50/p95 per row, work counters
//! and the host-load snapshot with every run.
//!
//! `test = false` in Cargo.toml keeps this out of `cargo test`: a wall-clock
//! measurement asserted as a test fails on a loaded machine, and a check that
//! fails for reasons unrelated to the code is worse than no check.
//!
//! # What the repository rows measure
//!
//! * They run on a **generated fixture repository** with linked worktrees,
//!   built once under the hermetic temp `HOME`, so two machines and two days
//!   measure the same bytes. `GITPULSE_BENCH_REPO=<path>` points them at a
//!   real checkout instead, explicitly; the run then says so in its report.
//! * `main` lifts the production spawn rate first
//!   (`run_process_with_unlimited_spawn_rate`, the switch the stress tests
//!   use). Without it the 220 git-spawning samples spent the production gate's
//!   192-spawn burst and then refilled at 8/s, so the rows timed the rate
//!   limiter or a deferred, degraded snapshot rather than the work.
//! * **Every sampled response is checked.** A tool result with `isError`, or
//!   any object in its payload with `"ok": false` (a facet that did not run),
//!   aborts the run naming the tool and the facet path. A degraded answer is
//!   never timed as if it were the work. Facets that report unavailability
//!   without an `ok` flag (`codeintel.available`, `ledger.error`,
//!   `agents.live`) are printed, not gated: on a fixture with no code-graph
//!   store they are expected, and `agents.live` reads the host's real process
//!   table whatever `HOME` is.
//!
//! The two rows that exist because a measurement found a defect:
//!
//! * `codeintel::status` used to call `latest_edges(0.0)` and throw the rows
//!   away through `.len()`, materialising the whole edge table to learn one
//!   integer — 46 MB resident on a 13-file repository, on a call that runs on
//!   every `gitpulse_status` and every `gitpulse_insights`.
//! * `tools/call` serialises its payload twice, once pretty-printed into
//!   `content[0].text` and once raw into `structuredContent`. That is what the
//!   spec asks for, so the row exists to keep the cost visible rather than to
//!   argue with it.

mod common;

use common::fixture::{build_repo, count_git_processes, Spec};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::mcp::{self, Era, JsonRpcRequest};
use serde_json::{json, Value};

/// Rounds for the cheap protocol rows and for the git-spawning rows.
const PROTOCOL_ROUNDS: usize = 200;
const WORK_ROUNDS: usize = 30;

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": mcp::PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": { "name": "gitpulse-bench", "version": "0" },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn request(method: &str, mut params: Value) -> JsonRpcRequest {
    params["_meta"] = modern_meta();
    JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: method.into(),
        params,
    }
}

/// One request through `accept` + `run`, returning the response as a value and
/// the byte length it serializes to on the transport.
fn call(method: &str, params: &Value) -> (Value, usize) {
    let mut era = Era::Unknown;
    let response =
        mcp::process_request(request(method, params.clone()), &mut era).expect("answered");
    let text = serde_json::to_string(&response).expect("serializes");
    let bytes = text.len();
    (
        serde_json::from_str(&text).expect("a response is JSON"),
        bytes,
    )
}

/// A `tools/call` that must be the work, not a refusal: aborts naming the tool
/// and the failing facet otherwise.
fn checked_tool_call(tool: &str, repo: &str) -> Sample {
    let params = json!({ "name": tool, "arguments": { "repo_path": repo } });
    let (response, bytes) = call("tools/call", &params);
    let result = &response["result"];
    if !response["error"].is_null() || result.is_null() {
        panic!(
            "{tool}: JSON-RPC error instead of a result: {}",
            response["error"]
        );
    }
    if result["isError"] == true {
        panic!(
            "{tool}: the response is isError, so this run would time a refusal, not the work: {}",
            result["content"][0]["text"]
        );
    }
    let payload = &result["structuredContent"];
    let passed = common::require_no_failed_facets(tool, payload);
    Sample::default()
        .counter("facets_ok", passed)
        .metric("bytes", bytes as f64)
}

fn protocol_case(name: &'static str, method: &'static str, params: Value) -> Case<'static> {
    Case::new(name, move || {
        let (response, bytes) = call(method, &params);
        std::hint::black_box(&response);
        Sample::default().metric("bytes", bytes as f64)
    })
}

fn main() {
    // First statement: the production spawn gate (192-spawn burst, then 8/s)
    // would otherwise be what the git-spawning rows time.
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();

    let mut report = Report::new("mcp_protocol");
    let protocol_rounds = report.opts.samples(PROTOCOL_ROUNDS);
    let work_rounds = report.opts.samples(WORK_ROUNDS);

    // ---- The repository the work rows run on.
    let temp = tempfile::tempdir().expect("temp dir");
    let explicit = std::env::var("GITPULSE_BENCH_REPO").ok();
    let spec = Spec {
        files: 400,
        dirs: 20,
        extra_commits: 30,
        touch: 4,
        linked_worktrees: 3,
    };
    let (repo, _fixture) = match &explicit {
        Some(path) => {
            common::fixture::trust_repo(std::path::Path::new(path));
            report.note(format!(
                "repository rows run on GITPULSE_BENCH_REPO={path} (explicit; not comparable across machines)"
            ));
            (path.clone(), None)
        }
        None => {
            let built = build_repo(temp.path(), &spec);
            report.note(format!(
                "repository rows run on a generated fixture: {} files in {} dirs, {} commits, {} linked worktrees, hermetic HOME",
                spec.files,
                spec.dirs,
                spec.extra_commits + 1,
                spec.linked_worktrees
            ));
            (built.path().to_string(), Some(built))
        }
    };
    report.note(format!("protocol: {}", mcp::PROTOCOL_VERSION));

    // ---- Protocol layer: no filesystem, no git. This is the overhead the
    // capability work added, and it has to stay invisible next to a git spawn.
    let trace = mcp::tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_codeintel_trace")
        .expect("tool");
    let schema = trace["inputSchema"].clone();
    let good = json!({ "repo_path": "/tmp/x", "from": "a", "to": "b", "budget": 2000 });
    let bad = json!({ "repo_path": "", "from": 7, "to": "b", "budget": -1, "nope": 1 });
    let cheap = vec![
        protocol_case("server/discover", "server/discover", json!({})),
        protocol_case("tools/list", "tools/list", json!({})),
        protocol_case("resources/list", "resources/list", json!({})),
        protocol_case(
            "resources/templates/list",
            "resources/templates/list",
            json!({}),
        ),
        protocol_case("prompts/list", "prompts/list", json!({})),
        protocol_case(
            "resources/read server/manifest",
            "resources/read",
            json!({ "uri": "gitpulse://server/manifest" }),
        ),
        protocol_case(
            "completion/complete",
            "completion/complete",
            json!({
                "ref": { "type": "ref/prompt", "name": "gitpulse_preflight" },
                "argument": { "name": "repo_path", "value": "" }
            }),
        ),
        // Argument validation, the per-call tax the spec's "validate all tool
        // inputs" adds, against the largest schema in the catalog.
        Case::new("validate (accepting)", || {
            Sample::default().counter(
                "violations",
                std::hint::black_box(mcp::validate::validate(&good, &schema)).len() as u64,
            )
        }),
        Case::new("validate (rejecting)", || {
            Sample::default().counter(
                "violations",
                std::hint::black_box(mcp::validate::validate(&bad, &schema)).len() as u64,
            )
        }),
        // A rejected call must be cheap: this is the path a confused model
        // retries in a loop, and it must never cost what the real call costs.
        protocol_case(
            "tools/call rejected argument",
            "tools/call",
            json!({
                "name": "gitpulse_active_changes",
                "arguments": { "repo_path": "/tmp", "limit": "500" }
            }),
        ),
    ];
    let warmup = report.opts.warmup(20);
    let mut rows = interleave(cheap, protocol_rounds, warmup);

    // ---- Real work. These dominate; the rows above exist to show by how much.
    let work = vec![
        Case::new("codeintel status", || {
            checked_tool_call("gitpulse_status", &repo)
        }),
        Case::new("tools/call collision_risk", || {
            checked_tool_call("gitpulse_collision_risk", &repo)
        }),
        Case::new("tools/call insights", || {
            checked_tool_call("gitpulse_insights", &repo)
        }),
    ];
    rows.extend(interleave(work, work_rounds, report.opts.warmup(3)));

    // The spawn gate is lifted, so its report is not the evidence; git's own
    // trace2 is. One insights call, counted in its own untimed pass.
    let (_, processes) = count_git_processes(|| checked_tool_call("gitpulse_insights", &repo));
    if let Some(row) = rows.iter_mut().find(|r| r.name == "tools/call insights") {
        row.counters.insert("git_processes", processes);
    }
    report.note(format!(
        "one insights call starts {processes} git processes (git trace2, counted in a separate untimed pass)"
    ));

    // A protocol-only call has to be negligible against a call that touches
    // git. Reporting the ratio is the point: an absolute number means nothing
    // without knowing what it is being hidden behind.
    let min_of = |name: &str| rows.iter().find(|r| r.name == name).map(|r| r.min());
    if let (Some(protocol), Some(work)) = (min_of("tools/list"), min_of("tools/call insights")) {
        if !protocol.is_zero() && !work.is_zero() {
            report.note(format!(
                "tools/list is {:.0}x cheaper than one insights call (min {:.1?} vs {:.1?})",
                work.as_secs_f64() / protocol.as_secs_f64(),
                protocol,
                work
            ));
        }
    }
    report.note(
        "tools/call emits its payload twice (pretty-printed content[0].text and raw \
         structuredContent) as the spec asks; the `bytes` metric counts both"
            .to_string(),
    );
    report.extend(rows);
    report.finish();
}
