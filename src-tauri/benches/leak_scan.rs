//! The commit gate's secret scan: the staged-diff scan end to end, and the
//! engine and patch parser on their own.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench leak_scan`
//! (`-- --smoke` for 3 samples, `-- --json <path>` for the report).
//!
//! Rows:
//!
//! * `staged typical` / `staged typical (null pair)`: the same scan twice,
//!   interleaved, over a staged change of 24 files and ~1,000 added lines.
//!   This is the scan `cmd_commit` runs (two git processes: the raw listing
//!   and the patch). The two rows run identical code, so the gap between them
//!   is this host's noise floor, and any A/B read off this bench has to clear
//!   it. The ship target for this row is about 50 ms.
//! * `staged with binary`: the same change plus one staged 256 KiB binary
//!   blob, which adds the `cat-file --batch-check` / `--batch` reads.
//! * `staged with streamed 24 MiB binary`: the typical change plus one 24 MiB
//!   binary, past the batch cap, read through `git cat-file blob` into the
//!   line splitter without being held.
//! * `engine 4 MiB`:`Tally::scan_blob` over generated mixed text (code,
//!   lockfile integrity hashes, UUIDs, base64, prose) with one planted key per
//!   1,000 lines. Bytes per second through `dc_redact::find_secret`.
//! * `parse_patch 4 MiB`: the strict `-U0` patch parser alone.
//!
//! The production spawn rate is lifted, as the other git-spawning benches do:
//! these rows time the work, not the gate's token refill.

mod common;

use common::fixture::{git, trust_repo};
use common::{interleave, Case, Report, Sample};
use gitpulse_lib::leak_scan::staged::scan_staged;
use gitpulse_lib::leak_scan::{parse_patch, Tally};
use std::path::Path;

/// Deterministic text: a rotating set of line shapes a real repository has,
/// with a credential-shaped line every `plant_every` lines.
fn corpus(bytes: usize, plant_every: usize) -> String {
    let shapes = [
        "    let total = items.iter().map(|item| item.price * item.quantity).sum::<u64>();",
        "  integrity sha512-q3ZT7KXR2M5NWB8DaB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5hJ7kL9mN1pQ3sT5vW7yZ9bC1dE3fG==",
        "id = \"123e4567-e89b-12d3-a456-426614174000\" # request id",
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
        "The scanner reads every added line and names the shape it found, never the value.",
        "function a(b,c){return b.map(function(d){return d+c}).filter(Boolean).join(\",\")}",
    ];
    // Assembled, so this file carries no token a scanner would stop.
    let planted = format!(
        "export TOKEN={}_{}\n",
        "ghp", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5"
    );
    let mut out = String::with_capacity(bytes + 256);
    let mut n = 0usize;
    while out.len() < bytes {
        if n % plant_every == plant_every - 1 {
            out.push_str(&planted);
        } else {
            out.push_str(shapes[n % shapes.len()]);
            out.push('\n');
        }
        n += 1;
    }
    out
}

/// A patch of `files` sections, each one hunk of `lines` added lines.
fn patch(files: usize, lines: usize) -> String {
    let body = corpus(lines * 90, usize::MAX);
    let added: Vec<&str> = body.lines().take(lines).collect();
    let mut out = String::new();
    for f in 0..files {
        out.push_str(&format!(
            "diff --git a/src/f{f}.rs b/src/f{f}.rs\nindex 1111111..2222222 100644\n--- a/src/f{f}.rs\n+++ b/src/f{f}.rs\n@@ -0,0 +1,{} @@\n",
            added.len()
        ));
        for line in &added {
            out.push('+');
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// A committed repository with `files` files staged for the next commit,
/// each about `lines` lines long, clean of credentials.
fn staged_repo(root: &Path, files: usize, lines: usize) -> String {
    std::fs::create_dir_all(root).unwrap();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "bench\n").unwrap();
    git(root, &["add", "README.md"]);
    git(root, &["commit", "-q", "-m", "init"]);
    let body = corpus(lines * 90, usize::MAX);
    let text: String = body.lines().take(lines).map(|l| format!("{l}\n")).collect();
    for f in 0..files {
        let path = root.join(format!("src/m{f}/file.rs"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &text).unwrap();
    }
    git(root, &["add", "-A"]);
    trust_repo(root);
    root.to_str().expect("UTF-8 fixture path").to_string()
}

fn staged_case<'a>(name: &str, repo: &'a str) -> Case<'a> {
    Case::new(name, move || {
        let scan = scan_staged(Path::new(repo), &[]).expect("the scan reaches a verdict");
        assert!(
            scan.tally.is_clean(),
            "the fixture is clean: {:?}",
            scan.tally.hits
        );
        Sample::default()
            .counter("inputs", scan.tally.inputs_scanned)
            .counter("lines", scan.tally.lines_scanned)
            .counter("bytes", scan.tally.bytes_scanned)
    })
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let mut report = Report::new("leak_scan");
    let opts = &report.opts;
    let temp = tempfile::tempdir().expect("temp dir");

    // ---- staged scans, with a same-code null pair
    let typical = staged_repo(&temp.path().join("typical"), 24, 42);
    let with_binary = staged_repo(&temp.path().join("binary"), 24, 42);
    let mut blob = vec![0u8; 256 * 1024];
    for (i, b) in blob.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    std::fs::write(Path::new(&with_binary).join("asset.bin"), &blob).unwrap();
    git(Path::new(&with_binary), &["add", "asset.bin"]);
    // Past the batch cap, so it goes through the streamed read.
    let with_streamed = staged_repo(&temp.path().join("streamed"), 24, 42);
    let mut large = Vec::with_capacity(24 << 20);
    while large.len() < 24 << 20 {
        large.extend_from_slice(&blob);
    }
    std::fs::write(Path::new(&with_streamed).join("video.bin"), &large).unwrap();
    git(Path::new(&with_streamed), &["add", "video.bin"]);
    let staged_rows = interleave(
        vec![
            staged_case("staged typical", &typical),
            staged_case("staged typical (null pair)", &typical),
            staged_case("staged with binary", &with_binary),
            staged_case("staged with streamed 24 MiB binary", &with_streamed),
        ],
        opts.samples(41),
        opts.warmup(3),
    );

    // ---- engine and parser alone
    let text = corpus(4 << 20, 1000);
    let diff = patch(64, 700);
    let pure_rows = interleave(
        vec![
            Case::new("engine 4 MiB", || {
                let started = std::time::Instant::now();
                let mut tally = Tally::default();
                tally.scan_blob("corpus.txt", text.as_bytes());
                let elapsed = started.elapsed();
                Sample::default()
                    .counter("lines", tally.lines_scanned)
                    .counter("hits", tally.hits_total)
                    .metric("mb_per_s", text.len() as f64 / 1e6 / elapsed.as_secs_f64())
                    .elapsed(elapsed)
            }),
            Case::new("parse_patch 4 MiB", || {
                let sections = parse_patch(diff.as_bytes()).expect("the generated patch parses");
                let added: usize = sections.iter().map(|s| s.added.len()).sum();
                Sample::default()
                    .counter("sections", sections.len() as u64)
                    .counter("added_lines", added as u64)
                    .counter("bytes", diff.len() as u64)
            }),
        ],
        opts.samples(15),
        opts.warmup(2),
    );

    report.note(
        "staged rows: a generated repository with 24 staged files of 42 lines each (~1,000 added \
         lines), one with an extra staged 256 KiB binary, one with an extra 24 MiB binary that is \
         streamed; scan_staged spawns git (raw listing, patch, cat-file for binaries); production \
         spawn rate lifted. Engine rows: \
         generated 4 MiB mixed text with a planted key every 1,000 lines, and a 64-file, \
         700-line-per-file -U0 patch"
            .to_string(),
    );
    report.extend(staged_rows);
    report.extend(pure_rows);
    report.finish();
}
