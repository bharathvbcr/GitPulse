//! Benchmark, not a correctness test: the steady-state cost of one transcript
//! pass over 2 000 transcripts none of which changed since the last pass (the
//! common shape: a long-lived machine, a corpus of old sessions). Run by name,
//! `-- --ignored --exact`, from two builds interleaved; it prints the minimum
//! and median of the passes, the host load and the count of rows recorded by the
//! measured passes (zero: a build that re-read the corpus cannot win by
//! recording less).

use super::*;
use crate::procguard::LockedSpawn;

const FILES: usize = 2_000;
const PASSES: usize = 9;

fn load() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output_locked()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

#[test]
#[ignore = "benchmark: run explicitly"]
fn bench_transcript_pass_over_two_thousand_unchanged_transcripts() {
    let repo_dir = tempfile::tempdir().expect("repo");
    let repo = repo_dir
        .path()
        .canonicalize()
        .expect("canonical")
        .to_string_lossy()
        .into_owned();
    let corpus = tempfile::tempdir().expect("corpus");
    let slug = corpus.path().join("slug");
    std::fs::create_dir_all(&slug).expect("slug");
    for i in 0..FILES {
        let line = serde_json::json!({
            "type": "assistant",
            "sessionId": format!("B{i}"),
            "timestamp": "2026-09-01T12:00:00.000Z",
            "version": "2.1.241",
            "cwd": repo,
            "message": { "content": [
                { "type": "tool_use", "name": "Edit",
                  "input": { "file_path": format!("{repo}/f{i}.rs") } }
            ]}
        })
        .to_string();
        std::fs::write(slug.join(format!("B{i}.jsonl")), line + "\n").expect("transcript");
    }
    let serial = crate::test_support::env::env_serial();
    let _env =
        crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", corpus.path());

    let first = pass::ingest_family_until(&repo, &repo, &[], &mut |_| false);
    assert_eq!(first.recorded, i64::try_from(FILES).unwrap(), "{first:?}");
    let mut times = Vec::new();
    let mut recorded = 0;
    for _ in 0..PASSES {
        let started = std::time::Instant::now();
        let out = pass::ingest_family_until(&repo, &repo, &[], &mut |_| false);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        recorded += out.recorded;
    }
    times.sort_by(|x, y| x.partial_cmp(y).expect("finite"));
    println!(
        "BENCH transcripts_2k min_ms={:.2} median_ms={:.2} recorded={recorded} load={}",
        times[0],
        times[times.len() / 2],
        load()
    );
}
