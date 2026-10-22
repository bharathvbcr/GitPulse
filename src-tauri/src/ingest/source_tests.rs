//! Transcript sources: what moving Claude behind the source trait must leave
//! byte-identical, and what the Codex source must record.

use super::*;

fn with_roots<T>(claude: &std::path::Path, f: impl FnOnce() -> T) -> T {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", claude);
    f()
}

fn claude_line(
    cwd: &str,
    session: &str,
    second: u32,
    tool: &str,
    input: serde_json::Value,
) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": session,
        "timestamp": format!("2026-09-01T12:00:{second:02}.000Z"),
        "version": "2.1.241",
        "cwd": cwd,
        "gitBranch": "main",
        "message": { "content": [
            { "type": "text", "text": "working" },
            { "type": "tool_use", "name": tool, "input": input }
        ]}
    })
    .to_string()
}

/// FNV-1a over `text`: a digest that does not move with the toolchain.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Every row a Claude corpus produces, as one canonical text: each field the
/// ledger keeps except the ids and the ingest-time stamp, with the repository's
/// own path made constant, in sorted order.
fn claude_rows_digest() -> (String, u64) {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let slug = corpus.path().join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    let body = [
        claude_line(
            repo,
            "S1",
            1,
            "Edit",
            serde_json::json!({ "file_path": format!("{repo}/src/a.rs") }),
        ),
        claude_line(
            repo,
            "S1",
            2,
            "Bash",
            serde_json::json!({ "command": "cargo test" }),
        ),
        claude_line(
            repo,
            "S1",
            3,
            "Read",
            serde_json::json!({ "file_path": format!("{repo}/src/a.rs") }),
        ),
        claude_line(
            "/elsewhere",
            "S1",
            4,
            "Bash",
            serde_json::json!({ "command": "ls" }),
        ),
        "{\"type\":\"assistant\", this is not json}".to_string(),
        claude_line(
            repo,
            "S1",
            5,
            "Write",
            serde_json::json!({ "file_path": format!("{repo}/src/b.rs") }),
        ),
    ]
    .join("\n");
    std::fs::write(slug.join("S1.jsonl"), body + "\n").unwrap();
    std::fs::write(
        slug.join("S2.jsonl"),
        claude_line(
            repo,
            "S2",
            9,
            "NotebookEdit",
            serde_json::json!({ "file_path": format!("{repo}/n.ipynb") }),
        ) + "\n",
    )
    .unwrap();

    let out = with_roots(corpus.path(), || ingest_transcripts(repo));
    let mut rows: Vec<String> = ledger::tail(repo, 0, 100)
        .unwrap()
        .iter()
        .map(|e| {
            format!(
                "{}|{:?}|{:?}|{}|{:?}|{}|{:?}|{:?}|{:?}",
                e.action,
                e.object,
                e.session_id,
                e.actor_kind,
                e.actor_id,
                e.outcome,
                e.worktree_path,
                e.task_id,
                e.detail_json
            )
            .replace(repo, "<repo>")
        })
        .collect();
    rows.sort();
    let canonical = format!(
        "recorded={} transcripts={} skipped={} listed={} complete={:?} error={:?}\n{}",
        out.recorded,
        out.transcripts,
        out.skipped_lines,
        out.transcripts_listed,
        out.walk_complete,
        out.error,
        rows.join("\n")
    );
    let digest = fnv1a(&canonical);
    (canonical, digest)
}

/// Pinned on the code before the source trait existed: the trait is a
/// refactor of the Claude path, so every row and every counter it produces
/// must be the one the old code produced.
const CLAUDE_DIGEST: u64 = 15_870_773_247_515_364_499;

#[test]
fn claude_rows_are_byte_identical_to_what_they_were_before_sources() {
    let (canonical, digest) = claude_rows_digest();
    assert_eq!(
        digest, CLAUDE_DIGEST,
        "digest moved; rows were:\n{canonical}"
    );
}
