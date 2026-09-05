//! Where the MCP surface spends its time.
//!
//! Run with `cargo bench --bench mcp_protocol`. Deliberately not a Criterion
//! benchmark: Criterion would be a new dependency, and the questions here are
//! coarse — "is the protocol layer free relative to the work it wraps, and did
//! a fix that was supposed to be cheap stay cheap" — which do not need
//! statistical machinery. What they do need is honesty about spread, so every
//! row reports p50 and p95 rather than a single mean that hides a stall.
//!
//! `test = false` in Cargo.toml keeps this out of `cargo test`: a wall-clock
//! measurement asserted as a test fails on a loaded machine, and a check that
//! fails for reasons unrelated to the code is worse than no check.
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

use std::time::{Duration, Instant};

use gitpulse_lib::mcp::{self, Era, JsonRpcRequest};
use serde_json::{json, Value};

/// Samples per row after warmup.
const SAMPLES: usize = 200;
const WARMUP: usize = 20;

struct Row {
    name: &'static str,
    p50: Duration,
    p95: Duration,
    worst: Duration,
    bytes: usize,
}

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

/// Time `body` `SAMPLES` times and summarise. `body` returns the serialized
/// response so the row can report payload size next to latency — a fast call
/// that emits a megabyte is not a cheap call on a stdio transport.
fn measure(name: &'static str, mut body: impl FnMut() -> usize) -> Row {
    for _ in 0..WARMUP {
        std::hint::black_box(body());
    }
    let mut timings = Vec::with_capacity(SAMPLES);
    let mut bytes = 0;
    for _ in 0..SAMPLES {
        let started = Instant::now();
        bytes = std::hint::black_box(body());
        timings.push(started.elapsed());
    }
    timings.sort_unstable();
    Row {
        name,
        p50: timings[SAMPLES / 2],
        p95: timings[SAMPLES * 95 / 100],
        worst: *timings.last().expect("samples"),
        bytes,
    }
}

/// One request through `accept` + `run`, serialized as the transport would.
fn round_trip(method: &'static str, params: Value) -> impl FnMut() -> usize {
    move || {
        let mut era = Era::Unknown;
        let response =
            mcp::process_request(request(method, params.clone()), &mut era).expect("answered");
        serde_json::to_string(&response).expect("serializes").len()
    }
}

fn render(rows: &[Row]) {
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(0).max(9);
    println!(
        "\n{:<width$}  {:>10}  {:>10}  {:>10}  {:>12}",
        "operation", "p50", "p95", "worst", "bytes"
    );
    println!("{}", "-".repeat(width + 50));
    for row in rows {
        println!(
            "{:<width$}  {:>10}  {:>10}  {:>10}  {:>12}",
            row.name,
            format!("{:.1?}", row.p50),
            format!("{:.1?}", row.p95),
            format!("{:.1?}", row.worst),
            row.bytes,
        );
    }
}

fn main() {
    // Every repository-scoped row runs against this crate's own checkout, which
    // is a real repository with real worktrees — a synthetic empty repo would
    // measure the error path rather than the work.
    let repo = std::env::var("GITPULSE_BENCH_REPO").unwrap_or_else(|_| {
        std::env::current_dir()
            .ok()
            .and_then(|d| d.parent().map(|p| p.to_string_lossy().into_owned()))
            .unwrap_or_else(|| ".".into())
    });
    println!("gitpulse-mcp protocol benchmark");
    println!("  protocol : {}", mcp::PROTOCOL_VERSION);
    println!("  repo     : {repo}");
    println!("  samples  : {SAMPLES} (after {WARMUP} warmup)");

    let mut rows = Vec::new();

    // ---- Protocol layer: no filesystem, no git. This is the overhead the
    // capability work added, and it has to stay invisible next to a git spawn.
    rows.push(measure(
        "server/discover",
        round_trip("server/discover", json!({})),
    ));
    rows.push(measure("tools/list", round_trip("tools/list", json!({}))));
    rows.push(measure(
        "resources/list",
        round_trip("resources/list", json!({})),
    ));
    rows.push(measure(
        "resources/templates/list",
        round_trip("resources/templates/list", json!({})),
    ));
    rows.push(measure(
        "prompts/list",
        round_trip("prompts/list", json!({})),
    ));
    rows.push(measure(
        "resources/read server/manifest",
        round_trip(
            "resources/read",
            json!({ "uri": "gitpulse://server/manifest" }),
        ),
    ));
    rows.push(measure(
        "completion/complete",
        round_trip(
            "completion/complete",
            json!({
                "ref": { "type": "ref/prompt", "name": "gitpulse_preflight" },
                "argument": { "name": "repo_path", "value": "" }
            }),
        ),
    ));

    // ---- Argument validation, the per-call tax the spec's "validate all tool
    // inputs" adds. Measured against the largest schema in the catalog.
    let trace = mcp::tools()
        .into_iter()
        .find(|t| t["name"] == "gitpulse_codeintel_trace")
        .expect("tool");
    let schema = trace["inputSchema"].clone();
    let good = json!({ "repo_path": "/tmp/x", "from": "a", "to": "b", "budget": 2000 });
    let bad = json!({ "repo_path": "", "from": 7, "to": "b", "budget": -1, "nope": 1 });
    rows.push(measure("validate (accepting)", || {
        std::hint::black_box(mcp::validate::validate(&good, &schema)).len()
    }));
    rows.push(measure("validate (rejecting)", || {
        std::hint::black_box(mcp::validate::validate(&bad, &schema)).len()
    }));

    // ---- A rejected call must be cheap: this is the path a confused model
    // retries in a loop, and it must never cost what the real call costs.
    rows.push(measure(
        "tools/call rejected argument",
        round_trip(
            "tools/call",
            json!({
                "name": "gitpulse_active_changes",
                "arguments": { "repo_path": "/tmp", "limit": "500" }
            }),
        ),
    ));

    // ---- Real work. These dominate; the rows above exist to show by how much.
    rows.push(measure(
        "codeintel status",
        round_trip(
            "tools/call",
            json!({
                "name": "gitpulse_status", "arguments": { "repo_path": repo.clone() }
            }),
        ),
    ));
    rows.push(measure(
        "tools/call collision_risk",
        round_trip(
            "tools/call",
            json!({
                "name": "gitpulse_collision_risk", "arguments": { "repo_path": repo.clone() }
            }),
        ),
    ));
    rows.push(measure(
        "tools/call insights",
        round_trip(
            "tools/call",
            json!({
                "name": "gitpulse_insights", "arguments": { "repo_path": repo.clone() }
            }),
        ),
    ));

    render(&rows);

    // A protocol-only call has to be negligible against a call that touches
    // git. Reporting the ratio is the point: an absolute number means nothing
    // without knowing what it is being hidden behind.
    let protocol = rows
        .iter()
        .find(|r| r.name == "tools/list")
        .map(|r| r.p50)
        .unwrap_or_default();
    let work = rows
        .iter()
        .find(|r| r.name == "tools/call insights")
        .map(|r| r.p50)
        .unwrap_or_default();
    if !protocol.is_zero() && !work.is_zero() {
        println!(
            "\ntools/list is {:.0}x cheaper than one insights call ({:.1?} vs {:.1?})",
            work.as_secs_f64() / protocol.as_secs_f64(),
            protocol,
            work
        );
    }
    println!(
        "\nNote: tools/call emits its payload twice — pretty-printed into content[0].text\n\
         and raw into structuredContent — which the spec asks for so pre-structuredContent\n\
         clients still read something. The `bytes` column counts both."
    );
}
