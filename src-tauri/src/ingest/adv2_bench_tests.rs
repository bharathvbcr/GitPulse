//! Claude-only steady-state pass benchmark (2000 unchanged files), identical in
//! the base and the lane. Run by name from each build, interleaved.

use super::*;

#[test]
#[ignore = "benchmark: run explicitly"]
fn adv2_bench_claude_only_unchanged_pass() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let claude = tempfile::tempdir().unwrap();
    let slug = claude.path().join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    for i in 0..2000 {
        let line = serde_json::json!({
            "type": "assistant", "sessionId": format!("B{i}"),
            "timestamp": "2026-09-01T12:00:01.000Z", "version": "2.1.241", "cwd": repo,
            "message": { "content": [
                { "type": "tool_use", "name": "Edit", "input": { "file_path": format!("{repo}/f{i}.rs") } }
            ]}
        })
        .to_string();
        std::fs::write(slug.join(format!("B{i}.jsonl")), line + "\n").unwrap();
    }
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("GITPULSE_TRANSCRIPT_ROOT", claude.path())
        .remove("GITPULSE_CODEX_TRANSCRIPT_ROOT")
        .remove("CODEX_HOME");
    let first = pass::ingest_family_until(&repo, &repo, &[], &mut |_| false);
    let mut times = Vec::new();
    for _ in 0..9 {
        let started = std::time::Instant::now();
        let out = pass::ingest_family_until(&repo, &repo, &[], &mut |_| false);
        assert_eq!(out.recorded, 0);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "ADV2BENCH first_recorded={} min_ms={:.2} median_ms={:.2}",
        first.recorded, times[0], times[4]
    );
}
