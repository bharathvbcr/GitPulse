//! Soundness tests for the `gitpulse-rustc` cache: every case drives the real
//! `gitpulse-rustc` binary around the real `rustc` (no cargo, no git), through a
//! small shell shim that can edit a file or flood stderr after rustc exits, to
//! model races and hostile cache contents deterministically.
//!
//! Each test names the review finding it pins (ZERO_COPY_REVIEW.md "Build cache").

#![cfg(unix)]

use gitpulse_worktree::build_cache::inspect_or_prune_cache;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const SHIM: &str = r#"#!/bin/sh
# Test shim: run the real rustc, then (only for a real compile) optionally edit
# a file or flood stderr, so races and oversize diagnostics are deterministic.
real="${GP_REAL_RUSTC:-rustc}"
case "$*" in *--crate-name*) build=1 ;; *) build=0 ;; esac
"$real" "$@"
rc=$?
if [ "$build" = 1 ] && [ "$rc" = 0 ]; then
  if [ -n "$GP_EDIT_FILE" ]; then printf '%s' "$GP_EDIT_CONTENT" > "$GP_EDIT_FILE"; fi
  if [ -n "$GP_STDERR_BYTES" ]; then head -c "$GP_STDERR_BYTES" /dev/zero | tr '\0' 'x' >&2; fi
fi
exit $rc
"#;

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    wt: PathBuf,
    cache: PathBuf,
    shim: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let wt = root.join("wt");
        fs::create_dir_all(wt.join("src")).unwrap();
        let cache = root.join("cache");
        let shim = root.join("rustc-shim.sh");
        fs::write(&shim, SHIM).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _tmp: tmp,
            root,
            wt,
            cache,
            shim,
        }
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.wt.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    /// Runs `gitpulse-rustc <shim> --crate-name demo ... src/lib.rs` into `<root>/<out>`.
    fn compile(&self, out: &str, extra: &[&str], env: &[(&str, &str)]) -> Output {
        let out_dir = self.root.join(out);
        fs::create_dir_all(&out_dir).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gitpulse-rustc"));
        cmd.current_dir(&self.wt)
            .arg(&self.shim)
            .args(["--crate-name", "demo", "--crate-type", "lib"])
            .args(["--edition", "2021", "--emit=dep-info,link"])
            .args(["-C", "extra-filename=-abc"])
            .arg("--out-dir")
            .arg(&out_dir)
            .args(extra)
            .arg("src/lib.rs")
            .env("GITPULSE_BUILD_CACHE_DIR", &self.cache)
            .env("GITPULSE_WORKTREE_ROOT", &self.wt);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn gitpulse-rustc")
    }

    fn compile_ok(&self, out: &str, extra: &[&str], env: &[(&str, &str)]) -> Output {
        let o = self.compile(out, extra, env);
        assert!(
            o.status.success(),
            "compile into {out} failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        o
    }

    fn rlib(&self, out: &str) -> PathBuf {
        self.root.join(out).join("libdemo-abc.rlib")
    }

    fn stats(&self) -> Value {
        serde_json::from_slice(&fs::read(self.cache.join("stats.json")).unwrap()).unwrap()
    }

    fn counter(&self, name: &str) -> u64 {
        self.stats()[name].as_u64().unwrap_or_else(|| {
            panic!(
                "counter `{name}` missing or not a number in {}",
                self.stats()
            )
        })
    }

    fn manifest_path(&self) -> PathBuf {
        let mut found = Vec::new();
        for shard in fs::read_dir(self.cache.join("direct")).unwrap() {
            for e in fs::read_dir(shard.unwrap().path()).unwrap() {
                found.push(e.unwrap().path());
            }
        }
        assert_eq!(found.len(), 1, "expected exactly one manifest: {found:?}");
        found.pop().unwrap()
    }

    fn tamper_manifest(&self, f: impl FnOnce(&mut Value)) {
        let p = self.manifest_path();
        let mut v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        f(&mut v);
        fs::write(p, serde_json::to_vec(&v).unwrap()).unwrap();
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    haystack.windows(n.len()).any(|w| w == n)
}

/// Gives mtimes a clear gap before the first compile so the "edited during compile"
/// fence never trips on a freshly written fixture.
fn settle() {
    std::thread::sleep(Duration::from_millis(60));
}

// ---- C1 -------------------------------------------------------------------

#[test]
fn c1_edit_during_compile_is_not_stored_as_a_hit_for_the_new_content() {
    let fx = Fixture::new();
    fx.write(
        "src/lib.rs",
        "pub const G: &str = include_str!(\"greeting.txt\");\n",
    );
    fx.write("src/greeting.txt", "hello-v1");
    settle();

    // The shim rewrites greeting.txt AFTER rustc has read it and BEFORE the cache
    // hashes dependencies: rustc built v1, the file on disk is now v2.
    let greeting = fx.wt.join("src/greeting.txt");
    fx.compile_ok(
        "out1",
        &[],
        &[
            ("GP_EDIT_FILE", greeting.to_str().unwrap()),
            ("GP_EDIT_CONTENT", "hello-v2"),
        ],
    );
    assert!(
        contains(&fs::read(fx.rlib("out1")).unwrap(), "hello-v1"),
        "fixture sanity: the first build embeds the pre-edit content"
    );

    // Next build sees greeting.txt = v2 and an unchanged lib.rs (same stage-1 key).
    fx.compile_ok("out2", &[], &[]);
    let served = fs::read(fx.rlib("out2")).unwrap();
    assert!(
        !contains(&served, "hello-v1"),
        "stale rlib built from the pre-edit file was served for the post-edit file"
    );
    assert!(contains(&served, "hello-v2"));
    assert_eq!(fx.counter("hits"), 0, "the raced compile must not be a hit");
    assert!(
        fx.counter("store_failures") >= 1,
        "refusing to store must be counted, not look like a normal miss"
    );
}

#[test]
fn c1_control_unedited_compile_is_stored_and_hit() {
    let fx = Fixture::new();
    fx.write(
        "src/lib.rs",
        "pub const G: &str = include_str!(\"greeting.txt\");\n",
    );
    fx.write("src/greeting.txt", "hello-v1");
    settle();
    fx.compile_ok("out1", &[], &[]);
    fx.compile_ok("out2", &[], &[]);
    assert_eq!(fx.counter("hits"), 1);
    assert_eq!(fx.counter("misses"), 1);
    assert_eq!(fx.counter("store_failures"), 0);
}

// ---- C2 -------------------------------------------------------------------

fn populated() -> Fixture {
    let fx = Fixture::new();
    fx.write("src/lib.rs", "pub fn v() -> u32 { 1 }\n");
    settle();
    fx.compile_ok("out1", &[], &[]);
    fx
}

#[test]
fn c2_manifest_artifact_name_cannot_escape_the_out_dir() {
    for (label, evil) in [
        ("dotdot", "../escaped-artifact.rlib".to_string()),
        ("nested", "sub/dir.rlib".to_string()),
    ] {
        let fx = populated();
        fx.tamper_manifest(|m| m["artifacts"][0]["file_name"] = Value::String(evil.clone()));
        let out = fx.compile("out2", &[], &[]);
        assert!(out.status.success(), "{label}: must fall back to a compile");
        assert!(
            !fx.root.join("escaped-artifact.rlib").exists(),
            "{label}: restore wrote outside the out dir"
        );
        assert!(
            !fx.root.join("out2/sub").exists(),
            "{label}: nested dir created"
        );
        assert!(fx.rlib("out2").is_file(), "{label}: real rlib must exist");
        assert!(fx.counter("restore_failures") >= 1, "{label}: not counted");
    }
}

#[test]
fn c2_manifest_absolute_artifact_name_cannot_escape_the_out_dir() {
    let fx = populated();
    let target = fx.root.join("abs-escape.rlib");
    fx.tamper_manifest(|m| {
        m["artifacts"][0]["file_name"] = Value::String(target.to_string_lossy().into_owned())
    });
    assert!(fx.compile("out2", &[], &[]).status.success());
    assert!(
        !target.exists(),
        "absolute file_name replaced the join base"
    );
}

#[test]
fn c2_manifest_dep_file_name_cannot_escape_the_out_dir() {
    let fx = populated();
    fx.tamper_manifest(|m| m["dep_file_name"] = Value::String("../escaped.d".into()));
    assert!(fx.compile("out2", &[], &[]).status.success());
    assert!(
        !fx.root.join("escaped.d").exists(),
        "dep-info restore wrote outside the out dir"
    );
}

#[test]
fn c2_blob_digest_cannot_reach_outside_the_blob_store() {
    let fx = populated();
    // blob_path_for("../planted-blob") == <cache>/blobs/../../planted-blob == <root>/planted-blob
    fs::write(fx.root.join("planted-blob"), "PLANTED-OUTSIDE-STORE").unwrap();
    fx.tamper_manifest(|m| {
        m["artifacts"][0]["blob_blake3"] = Value::String("../planted-blob".into())
    });
    assert!(fx.compile("out2", &[], &[]).status.success());
    let served = fs::read(fx.rlib("out2")).unwrap();
    assert!(
        !contains(&served, "PLANTED-OUTSIDE-STORE"),
        "a path-shaped digest served a file from outside the blob store"
    );
}

#[test]
fn c2_blob_with_tampered_content_is_a_miss_not_a_hit() {
    let fx = populated();
    let blob = walk_files(&fx.cache.join("blobs")).pop().expect("a blob");
    fs::write(&blob, "TAMPERED-BLOB-CONTENT").unwrap();
    let out = fx.compile("out2", &[], &[]);
    assert!(out.status.success());
    let served = fs::read(fx.rlib("out2")).unwrap();
    assert!(
        !contains(&served, "TAMPERED-BLOB-CONTENT"),
        "a blob whose content does not match its digest was served"
    );
    assert_eq!(fx.counter("hits"), 0);
    assert!(fx.counter("restore_failures") >= 1);
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk_files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

// ---- C3 -------------------------------------------------------------------

/// Builds `libfoo.a` (a staticlib from rustc itself, so no C toolchain is needed).
fn build_static_lib(dir: &Path, answer: u32) {
    fs::create_dir_all(dir).unwrap();
    let src = dir.join("foo.rs");
    fs::write(
        &src,
        format!("#[no_mangle]\npub extern \"C\" fn gp_native() -> i32 {{ {answer} }}\n"),
    )
    .unwrap();
    let st = Command::new("rustc")
        .args([
            "--crate-name",
            "foo",
            "--crate-type",
            "staticlib",
            "--edition",
            "2021",
        ])
        .arg("-o")
        .arg(dir.join("libfoo.a"))
        .arg(&src)
        .status()
        .expect("spawn rustc");
    assert!(st.success());
}

#[test]
fn c3_changed_native_static_library_invalidates_the_key() {
    let fx = Fixture::new();
    fx.write(
        "src/lib.rs",
        "extern \"C\" { fn gp_native() -> i32; }\npub fn f() -> i32 { unsafe { gp_native() } }\n",
    );
    let native = fx.root.join("native");
    let search = format!("native={}", native.display());
    let args = ["-L", search.as_str(), "-l", "static=foo"];

    build_static_lib(&native, 7);
    settle();
    fx.compile_ok("out1", &args, &[]);
    let first = fs::read(fx.rlib("out1")).unwrap();

    // Swift-bridge shaped change: the library changes, no Rust source does.
    build_static_lib(&native, 8);
    settle();
    fx.compile_ok("out2", &args, &[]);
    assert_eq!(
        fx.counter("hits"),
        0,
        "a rebuilt native static library was served from the stale entry"
    );
    assert_ne!(
        first,
        fs::read(fx.rlib("out2")).unwrap(),
        "the rlib bundles the static library, so it must differ"
    );

    // Same library bytes at a different path still share (path-independent key).
    let moved = fx.root.join("native-moved");
    fs::create_dir_all(&moved).unwrap();
    fs::copy(native.join("libfoo.a"), moved.join("libfoo.a")).unwrap();
    let moved_search = format!("native={}", moved.display());
    fx.compile_ok(
        "out3",
        &["-L", moved_search.as_str(), "-l", "static=foo"],
        &[],
    );
    assert_eq!(
        fx.counter("hits"),
        1,
        "identical library content must still hit"
    );
}

#[test]
fn c3_unresolvable_static_library_is_an_uncacheable_passthrough() {
    let fx = Fixture::new();
    fx.write("src/lib.rs", "pub fn v() -> u32 { 1 }\n");
    settle();
    // rustc itself fails to find it; the point is the cache must not key it by name.
    let out = fx.compile("out1", &["-l", "static=does_not_exist_anywhere"], &[]);
    assert!(!out.status.success());
    assert_eq!(fx.counter("passthroughs"), 1);
    assert_eq!(fx.counter("misses"), 0);
    assert!(!fx.cache.join("direct").exists(), "nothing may be stored");
}

// ---- C4 -------------------------------------------------------------------

#[test]
fn c4_a_manifest_the_reader_would_refuse_is_not_written_and_is_counted() {
    let fx = Fixture::new();
    fx.write("src/lib.rs", "pub fn v() -> u32 { 1 }\n");
    settle();
    // 1.5 MB of stderr becomes a >4 MiB JSON integer array in the manifest.
    let out = fx.compile_ok("out1", &[], &[("GP_STDERR_BYTES", "1500000")]);
    assert!(
        out.stderr.len() >= 1_500_000,
        "stderr must still be forwarded"
    );
    assert!(fx.rlib("out1").is_file());
    assert!(
        fx.counter("store_failures") >= 1,
        "a failed store must be counted, not reported as a stored miss"
    );
    let manifests = if fx.cache.join("direct").exists() {
        walk_files(&fx.cache.join("direct"))
    } else {
        Vec::new()
    };
    for m in manifests {
        assert!(
            fs::metadata(&m).unwrap().len() <= 4 * 1024 * 1024,
            "a manifest larger than the read cap would be a permanent silent miss"
        );
    }
}

#[test]
fn c4_unreadable_existing_manifest_is_counted_and_replaced() {
    let fx = populated();
    fs::write(fx.manifest_path(), "{ not json").unwrap();
    fx.compile_ok("out2", &[], &[]);
    assert!(
        fx.counter("restore_failures") >= 1,
        "a corrupt manifest read error was swallowed as a plain miss"
    );
    // The rebuild repairs the entry.
    fx.compile_ok("out3", &[], &[]);
    assert_eq!(fx.counter("hits"), 1);
}

#[test]
fn c4_corrupt_stats_file_is_an_error_not_zero_counters() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("stats.json"), "{ definitely not json").unwrap();
    let report = inspect_or_prune_cache(tmp.path(), false, None);
    assert!(
        report.is_err(),
        "corrupt stats.json was reported as an empty, all-zero cache: {report:?}"
    );
}
