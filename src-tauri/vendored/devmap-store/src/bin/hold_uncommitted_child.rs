//! Separate-process helper for `hold_uncommitted` adversarial tests.
//!
//! Must not live inside the multithreaded test process: `fork(2)` after rayon /
//! the test harness has spawned threads deadlocks on allocator locks, which is
//! how `a_writer_killed_mid_transaction…` timed out with an empty pipe under
//! `cargo test --workspace`.

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::Connection;

fn main() {
    let mut args = env::args().skip(1);
    let db = PathBuf::from(args.next().expect("db path"));
    let script = args.next().expect("script");
    let ready = args.next().unwrap_or_else(|| "ready".to_string());

    let conn = Connection::open(&db).expect("child opens store");
    conn.busy_timeout(Duration::from_millis(5000))
        .expect("busy_timeout");
    conn.execute_batch("BEGIN IMMEDIATE")
        .expect("BEGIN IMMEDIATE");
    match script.as_str() {
        "torn_generation" => {
            conn.execute(
                "INSERT INTO generations (created_at, head_sha, analysis_json) VALUES (1.0, 'torn', '{}')",
                [],
            )
            .expect("torn generation");
            // A whole torn generation as v28 writes one: the old membership of
            // the file closed, a new payload with a symbol named `torn`, and the
            // range that brings it into the new generation.
            let gen: i64 = conn
                .query_row("SELECT max(id) FROM generations", [], |row| row.get(0))
                .expect("generation id");
            conn.execute(
                "UPDATE file_rows SET valid_to = ?1 WHERE file_id = 1 AND valid_to IS NULL",
                [gen],
            )
            .expect("close membership");
            conn.execute(
                "INSERT INTO file_payloads (file_id, content_hash, language, grammar_version, analyzer_version, parse_outcome_json, engine_json, extraction_json) VALUES (1, 424242, 'python', 'torn', 'torn', '\"Clean\"', '\"TreeSitter\"', '{}')",
                [],
            )
            .expect("torn payload");
            conn.execute(
                "INSERT INTO payload_nodes (payload_id, ord_in_file, name, qualified_name, kind, span_start, span_end, is_exported) SELECT last_insert_rowid(), 0, 'torn', 'torn', 'Function', 0, 1, 0",
                [],
            )
            .expect("torn node");
            conn.execute(
                "INSERT INTO file_rows (file_id, payload_id, valid_from) SELECT 1, payload_id, ?1 FROM payload_nodes WHERE name = 'torn'",
                [gen],
            )
            .expect("torn membership");
        }
        "close_ranges" => {
            conn.execute(
                "INSERT INTO generations (created_at, head_sha, analysis_json) VALUES (9.0, 'torn', '{}')",
                [],
            )
            .expect("torn generation");
            let gen: i64 = conn
                .query_row("SELECT max(id) FROM generations", [], |row| row.get(0))
                .expect("generation id");
            conn.execute(
                "UPDATE edge_rows SET valid_to = ?1 WHERE valid_to IS NULL",
                [gen],
            )
            .expect("close edges");
            conn.execute(
                "UPDATE unresolved_rows SET valid_to = ?1 WHERE valid_to IS NULL",
                [gen],
            )
            .expect("close unresolved");
        }
        other => panic!("unknown script {other}"),
    }
    let mut out = io::stdout().lock();
    out.write_all(ready.as_bytes()).expect("announce");
    out.write_all(b"\n").expect("announce newline");
    out.flush().expect("announce flush");
    loop {
        std::thread::sleep(Duration::from_secs(600));
    }
}
