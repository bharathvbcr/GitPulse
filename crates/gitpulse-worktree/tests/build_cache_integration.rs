//! End-to-end subprocess integration tests for `gitpulse-rustc` and
//! `gitpulse-worktree build` / `cache-stats`.
//!
//! Verifies with real `cargo` + `rustc` invocations (100% offline, zero mocks):
//! 1. CLI `--help`, `--version`, argument validation, and untrusted-repo fail-closed gates.
//! 2. Cross-worktree zero-copy artifact sharing via `<git-common-dir>/gitpulse/build-cache`
//!    even when sibling worktree `target/` was NOT cloned (`--no-cache`, simulating an
//!    active `.cargo-lock` on the anchor checkout).
//! 3. Stage 2 `.d` dep-info soundness: mutating an `include_str!("greeting.txt")` file
//!    without touching `src/lib.rs` invalidates Stage 2 (`delta_stage2_invalidations >= 1`)
//!    and prevents stale compilation artifacts while keeping unchanged crates cached.
//! 4. `cache-stats --prune --max-bytes` evicts blobs and invalidated manifests cleanly.

use gitpulse_lib::procguard::LockedSpawn;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn worktree_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gitpulse-worktree")
}

fn rustc_wrapper_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gitpulse-rustc")
}

fn toolchain_and_profile_env(home: &Path) -> Vec<(&'static str, PathBuf)> {
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let host_home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .expect("host HOME");
            PathBuf::from(host_home).join(".rustup")
        });
    vec![
        ("HOME", home.to_path_buf()),
        ("APPDATA", home.join("AppData")),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("CARGO_HOME", home.join(".cargo-home")),
        ("RUSTUP_HOME", rustup_home),
    ]
}

fn approve_in_isolated_home(repo: &Path, home: &Path) {
    fs::create_dir_all(home).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "approve_build_cache_fixture_helper",
            "--ignored",
        ])
        .envs(toolchain_and_profile_env(home))
        .env("GITPULSE_E2E_GRANT_REPO", repo)
        .status_locked()
        .unwrap();
    assert!(status.success(), "isolated fixture approval failed");
}

#[test]
#[ignore = "subprocess helper invoked by approve_in_isolated_home"]
fn approve_build_cache_fixture_helper() {
    let repo = std::env::var("GITPULSE_E2E_GRANT_REPO").expect("fixture repository");
    let view = gitpulse_lib::repository_trust::inspect(&repo).unwrap();
    gitpulse_lib::repository_trust::grant(&repo, &view.identity, true).unwrap();
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output_locked()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn run_cli(home: &Path, args: &[&str]) -> Output {
    Command::new(worktree_bin())
        .args(args)
        .envs(toolchain_and_profile_env(home))
        // The fixtures assert on `<worktree>/target/debug/<bin>`. An inherited
        // target dir (CI and parallel lanes set one) would send the nested
        // `cargo build` elsewhere, and the binary the test then runs would not
        // exist.
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .output_locked()
        .expect("spawn gitpulse-worktree")
}

fn run_cli_json_ok(home: &Path, args: &[&str]) -> Value {
    let out = run_cli(home, args);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "gitpulse-worktree {args:?} failed (code {:?}):\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("invalid JSON from {args:?}: {e}\nraw stdout: {stdout}")
    })
}

fn toml_escape_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn init_offline_rust_workspace(anchor: &Path, vendor_root: &Path, home: &Path) {
    // 1. Offline vendored registry crate (`shared_calc`) so sibling worktrees share an
    //    identical crates-io `SourceId` and `-C metadata` completely offline.
    let shared_calc = vendor_root.join("shared_calc");
    fs::create_dir_all(shared_calc.join("src")).unwrap();
    fs::write(
        shared_calc.join("Cargo.toml"),
        "[package]\nname = \"shared_calc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        shared_calc.join("src/lib.rs"),
        "pub fn fibonacci(n: u32) -> u64 {\n    let (mut a, mut b) = (0u64, 1u64);\n    for _ in 0..n {\n        let next = a + b;\n        a = b;\n        b = next;\n    }\n    a\n}\n",
    )
    .unwrap();
    fs::write(shared_calc.join(".cargo-checksum.json"), "{\"files\":{}}\n").unwrap();

    // 2. Anchor Git repository with `.cargo/config.toml`, `crates/local_greeter` (using
    //    `include_str!("greeting.txt")` + `option_env!`), and binary `agent_app`.
    fs::create_dir_all(anchor.join(".cargo")).unwrap();
    fs::write(
        anchor.join(".cargo/config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"{}\"\n",
            toml_escape_path(vendor_root)
        ),
    )
    .unwrap();
    fs::write(anchor.join(".gitignore"), "/target\n/.devmap\n").unwrap();

    let greeter = anchor.join("crates/local_greeter");
    fs::create_dir_all(greeter.join("src")).unwrap();
    fs::write(
        greeter.join("Cargo.toml"),
        "[package]\nname = \"local_greeter\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(greeter.join("src/greeting.txt"), "hello-from-v1\n").unwrap();
    fs::write(
        greeter.join("src/lib.rs"),
        "pub fn greeting() -> &'static str {\n    if let Some(tag) = option_env!(\"AGENT_BUILD_TAG\") {\n        if tag == \"override\" {\n            return \"tagged-override\";\n        }\n    }\n    include_str!(\"greeting.txt\").trim()\n}\n",
    )
    .unwrap();

    fs::create_dir_all(anchor.join("src")).unwrap();
    fs::write(
        anchor.join("Cargo.toml"),
        "[package]\nname = \"agent_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nshared_calc = \"0.1.0\"\nlocal_greeter = { path = \"crates/local_greeter\" }\n",
    )
    .unwrap();
    fs::write(
        anchor.join("src/main.rs"),
        "fn main() {\n    println!(\"{}:{}\", local_greeter::greeting(), shared_calc::fibonacci(10));\n}\n",
    )
    .unwrap();

    // Generate `Cargo.lock` offline from the vendored directory before committing.
    let lock_out = Command::new("cargo")
        .current_dir(anchor)
        .args(["generate-lockfile", "--offline"])
        .envs(toolchain_and_profile_env(home))
        .output_locked()
        .expect("cargo generate-lockfile");
    assert!(
        lock_out.status.success(),
        "cargo generate-lockfile --offline failed: {}",
        String::from_utf8_lossy(&lock_out.stderr)
    );

    git(anchor, &["init", "-q", "-b", "main"]);
    git(anchor, &["config", "user.email", "test@gitpulse.local"]);
    git(anchor, &["config", "user.name", "GitPulse Test"]);
    git(anchor, &["add", "."]);
    git(anchor, &["commit", "-q", "-m", "initial rust workspace"]);
}

fn exe_suffix() -> &'static str {
    if cfg!(windows) {
        ".exe"
    } else {
        ""
    }
}

#[test]
fn gitpulse_rustc_cli_contracts_and_untrusted_build_gates_fail_closed() {
    let help = Command::new(rustc_wrapper_bin())
        .arg("--help")
        .output_locked()
        .expect("spawn gitpulse-rustc --help");
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("RUSTC_WRAPPER"));

    let ver = Command::new(rustc_wrapper_bin())
        .arg("--version")
        .output_locked()
        .expect("spawn gitpulse-rustc --version");
    assert_eq!(ver.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&ver.stdout).contains("gitpulse-rustc"));

    let missing_args = Command::new(rustc_wrapper_bin())
        .output_locked()
        .expect("spawn gitpulse-rustc with no args");
    assert_eq!(missing_args.status.code(), Some(2));

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let untrusted = tmp.path().join("untrusted");
    fs::create_dir_all(&untrusted).unwrap();
    git(&untrusted, &["init", "-q", "-b", "main"]);

    let untrusted_str = untrusted.to_str().unwrap();
    let refused_build = run_cli(&home, &["build", "--repo", untrusted_str, "--json"]);
    assert_eq!(refused_build.status.code(), Some(1));
    let refused_json: Value =
        serde_json::from_slice(&refused_build.stdout).expect("structured error json");
    assert_eq!(refused_json["ok"], false);

    let refused_stats = run_cli(&home, &["cache-stats", "--repo", untrusted_str, "--json"]);
    assert_eq!(refused_stats.status.code(), Some(1));
}

#[test]
fn build_shares_zero_copy_artifacts_across_worktrees_and_enforces_stage2_dep_invalidation() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let vendor = tmp.path().join("vendor");
    let anchor = tmp.path().join("anchor");

    init_offline_rust_workspace(&anchor, &vendor, &home);
    approve_in_isolated_home(&anchor, &home);

    let anchor_str = anchor.to_str().unwrap();
    let wrapper_str = rustc_wrapper_bin();

    // 1. Cold build in Worktree A (`anchor`) populates `<git-common-dir>/gitpulse/build-cache`.
    let build_a = run_cli_json_ok(
        &home,
        &[
            "build",
            "--repo",
            anchor_str,
            "--rustc-wrapper",
            wrapper_str,
            "--json",
            "--",
            "build",
            "--offline",
        ],
    );
    assert_eq!(build_a["ok"], true);
    assert!(
        build_a["delta_misses"].as_u64().unwrap_or(0) >= 2,
        "expected cold build misses for shared_calc + local_greeter, got: {build_a}"
    );
    assert_eq!(build_a["delta_hits"].as_u64(), Some(0));
    assert!(build_a["blob_count"].as_u64().unwrap_or(0) >= 2);

    let bin_a = anchor.join(format!("target/debug/agent_app{}", exe_suffix()));
    let run_a = Command::new(&bin_a).output_locked().expect("run bin_a");
    assert!(run_a.status.success());
    assert_eq!(String::from_utf8_lossy(&run_a.stdout).trim(), "hello-from-v1:55");

    // 2. Create sibling Worktree B with `--no-cache` (no `target/` cloned, simulating an
    //    active `.cargo-lock` on the anchor checkout) and build via `gitpulse-worktree build`.
    let wt_b = tmp.path().join("wt-lane-b");
    let wt_b_str = wt_b.to_str().unwrap();
    let created_b = run_cli_json_ok(
        &home,
        &[
            "create",
            "--repo",
            anchor_str,
            "--branch",
            "agent/lane-b",
            "--target",
            wt_b_str,
            "--no-cache",
            "--zero-copy",
            "--json",
        ],
    );
    assert_eq!(created_b["ok"], true);
    assert!(
        !wt_b.join("target").exists(),
        "wt_b must start with no target/ directory"
    );

    let build_b = run_cli_json_ok(
        &home,
        &[
            "build",
            "--repo",
            anchor_str,
            "--worktree",
            wt_b_str,
            "--rustc-wrapper",
            wrapper_str,
            "--json",
            "--",
            "build",
            "--offline",
        ],
    );
    assert_eq!(build_b["ok"], true);
    assert!(
        build_b["delta_hits"].as_u64().unwrap_or(0) >= 1,
        "expected cross-worktree cache hit on vendored shared_calc crate, got: {build_b}"
    );
    assert!(
        build_b["delta_bytes_saved"].as_u64().unwrap_or(0) > 0,
        "expected non-zero bytes saved from cached artifact restoration: {build_b}"
    );

    let bin_b = wt_b.join(format!("target/debug/agent_app{}", exe_suffix()));
    let run_b = Command::new(&bin_b).output_locked().expect("run bin_b");
    assert!(run_b.status.success());
    assert_eq!(String::from_utf8_lossy(&run_b.stdout).trim(), "hello-from-v1:55");

    // 3. Adversarial Stage 2 test: mutate ONLY `greeting.txt` (consumed via `include_str!`)
    //    without touching `crates/local_greeter/src/lib.rs`, remove `wt_b/target`, and rebuild.
    //    `shared_calc` must hit Stage 1+2 (`delta_hits >= 1`), while `local_greeter` must match
    //    Stage 1 and FAIL Stage 2 `.d` verification (`delta_stage2_invalidations == 1`)!
    fs::write(
        wt_b.join("crates/local_greeter/src/greeting.txt"),
        "hello-after-stage2-invalidation\n",
    )
    .unwrap();
    fs::remove_dir_all(wt_b.join("target")).unwrap();

    let build_b2 = run_cli_json_ok(
        &home,
        &[
            "build",
            "--repo",
            anchor_str,
            "--worktree",
            wt_b_str,
            "--rustc-wrapper",
            wrapper_str,
            "--json",
            "--",
            "build",
            "--offline",
        ],
    );
    assert_eq!(build_b2["ok"], true);
    assert!(
        build_b2["delta_hits"].as_u64().unwrap_or(0) >= 1,
        "unchanged shared_calc must still hit cache: {build_b2}"
    );
    assert_eq!(
        build_b2["delta_stage2_invalidations"].as_u64(),
        Some(1),
        "mutated include_str! dependency must trigger Stage 2 invalidation: {build_b2}"
    );

    let run_b2 = Command::new(&bin_b).output_locked().expect("run bin_b after stage2");
    assert!(run_b2.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run_b2.stdout).trim(),
        "hello-after-stage2-invalidation:55"
    );

    // 3b. Adversarial `# env-dep:` Stage 2 test: pass `--config env.AGENT_BUILD_TAG="override"`
    //     without changing any file on disk. Stage 1 key is unchanged (`AGENT_BUILD_TAG` is not a
    //     `CARGO_*` variable), so only Stage 2 `# env-dep:AGENT_BUILD_TAG` verification catches it!
    fs::remove_dir_all(wt_b.join("target")).unwrap();
    let build_b3 = run_cli_json_ok(
        &home,
        &[
            "build",
            "--repo",
            anchor_str,
            "--worktree",
            wt_b_str,
            "--rustc-wrapper",
            wrapper_str,
            "--json",
            "--",
            "build",
            "--offline",
            "--config",
            "env.AGENT_BUILD_TAG=\"override\"",
        ],
    );
    assert_eq!(build_b3["ok"], true);
    assert_eq!(
        build_b3["delta_stage2_invalidations"].as_u64(),
        Some(1),
        "option_env! AGENT_BUILD_TAG change must trigger Stage 2 # env-dep invalidation: {build_b3}"
    );
    let run_b3 = Command::new(&bin_b).output_locked().expect("run bin_b after env-dep");
    assert!(run_b3.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run_b3.stdout).trim(),
        "tagged-override:55"
    );

    // 3c. Concurrent parallel builds across two fresh sibling worktrees (`wt_c` and `wt_d`)
    //     hitting the shared `<git-common-dir>/gitpulse/build-cache` simultaneously.
    let wt_c = tmp.path().join("wt-lane-c");
    let wt_d = tmp.path().join("wt-lane-d");
    for (br, dir) in [("agent/lane-c", &wt_c), ("agent/lane-d", &wt_d)] {
        run_cli_json_ok(
            &home,
            &[
                "create",
                "--repo",
                anchor_str,
                "--branch",
                br,
                "--target",
                dir.to_str().unwrap(),
                "--no-cache",
                "--zero-copy",
                "--json",
            ],
        );
    }
    let (par_c, par_d) = std::thread::scope(|scope| {
        let hc = &home;
        let wc = wt_c.to_str().unwrap();
        let wd = wt_d.to_str().unwrap();
        let tc = scope.spawn(move || {
            run_cli_json_ok(
                hc,
                &[
                    "build",
                    "--repo",
                    anchor_str,
                    "--worktree",
                    wc,
                    "--rustc-wrapper",
                    wrapper_str,
                    "--json",
                    "--",
                    "build",
                    "--offline",
                ],
            )
        });
        let td = scope.spawn(move || {
            run_cli_json_ok(
                hc,
                &[
                    "build",
                    "--repo",
                    anchor_str,
                    "--worktree",
                    wd,
                    "--rustc-wrapper",
                    wrapper_str,
                    "--json",
                    "--",
                    "build",
                    "--offline",
                ],
            )
        });
        (tc.join().expect("lane c thread"), td.join().expect("lane d thread"))
    });
    assert_eq!(par_c["ok"], true);
    assert_eq!(par_d["ok"], true);
    let total_par_hits = par_c["delta_hits"].as_u64().unwrap_or(0)
        + par_d["delta_hits"].as_u64().unwrap_or(0);
    assert!(
        total_par_hits >= 2,
        "expected both concurrent lanes to hit shared_calc from cache: c={par_c}, d={par_d}"
    );

    // 4. `cache-stats` inspection + `--prune --max-bytes 1` bounded eviction.
    let stats_before = run_cli_json_ok(
        &home,
        &["cache-stats", "--repo", anchor_str, "--json"],
    );
    assert!(stats_before["report"]["blob_count"].as_u64().unwrap_or(0) >= 2);
    assert!(stats_before["report"]["manifest_count"].as_u64().unwrap_or(0) >= 2);

    let stats_pruned = run_cli_json_ok(
        &home,
        &[
            "cache-stats",
            "--repo",
            anchor_str,
            "--prune",
            "--max-bytes",
            "1",
            "--json",
        ],
    );
    assert!(
        stats_pruned["report"]["pruned_blobs"].as_u64().unwrap_or(0) >= 2,
        "expected blobs evicted under 1-byte cap: {stats_pruned}"
    );
    assert!(
        stats_pruned["report"]["pruned_manifests"].as_u64().unwrap_or(0) >= 2,
        "expected manifests referencing evicted blobs removed: {stats_pruned}"
    );
    assert!(stats_pruned["report"]["total_blob_bytes"].as_u64().unwrap_or(99) <= 1);
}

#[test]
fn build_caches_build_rs_include_out_dir_and_proc_macro_while_invalidating_runtime_out_dir_and_rewriting_symlinks()
{
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let vendor = tmp.path().join("vendor");
    let real_anchor = tmp.path().join("real-anchor");
    fs::create_dir_all(&real_anchor).unwrap();

    // 1. Vendored crate `shared_codegen` with a `build.rs` + `include!(concat!(env!("OUT_DIR"), "/gen.rs"))`
    //    (representative of serde/syn/libc/anyhow/prost/etc.).
    let codegen = vendor.join("shared_codegen");
    fs::create_dir_all(codegen.join("src")).unwrap();
    fs::write(
        codegen.join("Cargo.toml"),
        "[package]\nname = \"shared_codegen\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        codegen.join("build.rs"),
        "fn main() {\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(std::path::Path::new(&out).join(\"gen.rs\"), \"pub const MAGIC_NUMBER: u64 = 1337;\\n\").unwrap();\n}\n",
    )
    .unwrap();
    fs::write(
        codegen.join("src/lib.rs"),
        "// Generated in OUT_DIR by build.rs\ninclude!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\npub fn magic() -> u64 { MAGIC_NUMBER }\n",
    )
    .unwrap();
    fs::write(codegen.join(".cargo-checksum.json"), "{\"files\":{}}\n").unwrap();

    // 2. Vendored `proc-macro` crate `shared_macro`.
    let pmacro = vendor.join("shared_macro");
    fs::create_dir_all(pmacro.join("src")).unwrap();
    fs::write(
        pmacro.join("Cargo.toml"),
        "[package]\nname = \"shared_macro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n",
    )
    .unwrap();
    fs::write(
        pmacro.join("src/lib.rs"),
        "use proc_macro::TokenStream;\n#[proc_macro]\npub fn answer_tokens(_input: TokenStream) -> TokenStream {\n    \"42u64\".parse().unwrap()\n}\n",
    )
    .unwrap();
    fs::write(pmacro.join(".cargo-checksum.json"), "{\"files\":{}}\n").unwrap();

    // 3. Vendored crate `runtime_outdir` that embeds `env!("OUT_DIR")` into a runtime string literal!
    //    Soundness check: this crate MUST NOT reuse Worktree A's `.rlib` in Worktree B!
    let rt_outdir = vendor.join("runtime_outdir");
    fs::create_dir_all(rt_outdir.join("src")).unwrap();
    fs::write(
        rt_outdir.join("Cargo.toml"),
        "[package]\nname = \"runtime_outdir\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        rt_outdir.join("build.rs"),
        "fn main() {\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    std::fs::write(std::path::Path::new(&out).join(\"runtime.txt\"), \"live-asset\").unwrap();\n}\n",
    )
    .unwrap();
    fs::write(
        rt_outdir.join("src/lib.rs"),
        "pub fn baked_out_dir() -> &'static str {\n    env!(\"OUT_DIR\")\n}\n",
    )
    .unwrap();
    fs::write(rt_outdir.join(".cargo-checksum.json"), "{\"files\":{}}\n").unwrap();

    // 4. Anchor workspace consuming all three vendored crates.
    fs::create_dir_all(real_anchor.join(".cargo")).unwrap();
    fs::write(
        real_anchor.join(".cargo/config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"{}\"\n",
            toml_escape_path(&vendor)
        ),
    )
    .unwrap();
    fs::write(real_anchor.join(".gitignore"), "/target\n/.devmap\n").unwrap();
    fs::create_dir_all(real_anchor.join("src")).unwrap();
    fs::write(
        real_anchor.join("Cargo.toml"),
        "[package]\nname = \"combo_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nshared_codegen = \"0.1.0\"\nshared_macro = \"0.1.0\"\nruntime_outdir = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        real_anchor.join("src/main.rs"),
        "fn main() {\n    let sum = shared_codegen::magic() + shared_macro::answer_tokens!();\n    let asset = std::fs::read_to_string(std::path::Path::new(runtime_outdir::baked_out_dir()).join(\"runtime.txt\")).unwrap();\n    println!(\"{sum}:{asset}\");\n}\n",
    )
    .unwrap();

    let lock_out = Command::new("cargo")
        .current_dir(&real_anchor)
        .args(["generate-lockfile", "--offline"])
        .envs(toolchain_and_profile_env(&home))
        .output_locked()
        .expect("cargo generate-lockfile");
    assert!(lock_out.status.success());

    git(&real_anchor, &["init", "-q", "-b", "main"]);
    git(&real_anchor, &["config", "user.email", "test@gitpulse.local"]);
    git(&real_anchor, &["config", "user.name", "GitPulse Test"]);
    git(&real_anchor, &["add", "."]);
    git(&real_anchor, &["commit", "-q", "-m", "init combo workspace"]);

    approve_in_isolated_home(&real_anchor, &home);

    #[cfg(unix)]
    let anchor = {
        let sym = tmp.path().join("sym-anchor");
        std::os::unix::fs::symlink(&real_anchor, &sym).unwrap();
        sym
    };
    #[cfg(not(unix))]
    let anchor = real_anchor.clone();

    let anchor_str = anchor.to_str().unwrap();
    let wrapper_str = rustc_wrapper_bin();

    // Cold build in Worktree A (`sym-anchor`), then DELETE `real_anchor/target` completely
    // so Worktree B cannot accidentally rely on any leftover file in Worktree A's `target/`.
    let build_a = run_cli_json_ok(
        &home,
        &[
            "build",
            "--repo",
            anchor_str,
            "--rustc-wrapper",
            wrapper_str,
            "--json",
            "--",
            "build",
            "--offline",
        ],
    );
    assert_eq!(build_a["ok"], true);
    let bin_a = anchor.join(format!("target/debug/combo_app{}", exe_suffix()));
    let run_a = Command::new(&bin_a).output_locked().expect("run bin_a");
    assert!(run_a.status.success());
    assert_eq!(String::from_utf8_lossy(&run_a.stdout).trim(), "1379:live-asset");

    fs::remove_dir_all(real_anchor.join("target")).unwrap();

    // Create Worktree B (`real-wt-b` accessed via symlink `sym-wt-b`) with `--no-cache`.
    let real_wt_b = tmp.path().join("real-wt-b");
    run_cli_json_ok(
        &home,
        &[
            "create",
            "--repo",
            anchor_str,
            "--branch",
            "agent/combo-b",
            "--target",
            real_wt_b.to_str().unwrap(),
            "--no-cache",
            "--zero-copy",
            "--json",
        ],
    );
    #[cfg(unix)]
    let wt_b = {
        let sym = tmp.path().join("sym-wt-b");
        std::os::unix::fs::symlink(&real_wt_b, &sym).unwrap();
        sym
    };
    #[cfg(not(unix))]
    let wt_b = real_wt_b.clone();

    let build_b = run_cli_json_ok(
        &home,
        &[
            "build",
            "--repo",
            anchor_str,
            "--worktree",
            wt_b.to_str().unwrap(),
            "--rustc-wrapper",
            wrapper_str,
            "--json",
            "--",
            "build",
            "--offline",
        ],
    );
    assert_eq!(build_b["ok"], true);
    // BOTH `shared_codegen` (build.rs + include!(concat!(env!("OUT_DIR"), ...))) AND
    // `shared_macro` (proc-macro) must hit Stage 1+2 across worktrees (`delta_hits == 2`),
    // while ONLY `runtime_outdir` (which embeds `env!("OUT_DIR")` at runtime!) invalidates Stage 2 (`== 1`).
    assert_eq!(
        build_b["delta_hits"].as_u64(),
        Some(2),
        "expected both shared_codegen (OUT_DIR include!) and shared_macro (proc-macro) to hit cache: {build_b}"
    );
    assert_eq!(
        build_b["delta_stage2_invalidations"].as_u64(),
        Some(1),
        "expected ONLY runtime_outdir to invalidate Stage 2, not shared_codegen: {build_b}"
    );

    // Because `real_anchor/target` was deleted above, `combo_app` in `wt_b` proves:
    // 1. `shared_macro` proc-macro dylib loaded and expanded cleanly from zero-copy cache.
    // 2. `runtime_outdir::baked_out_dir()` points to `wt_b`'s live OUT_DIR, NOT deleted `anchor`'s!
    let bin_b = wt_b.join(format!("target/debug/combo_app{}", exe_suffix()));
    let run_b = Command::new(&bin_b).output_locked().expect("run bin_b");
    assert!(
        run_b.status.success(),
        "combo_app failed (stale OUT_DIR baked into binary?): stderr={}",
        String::from_utf8_lossy(&run_b.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run_b.stdout).trim(), "1379:live-asset");

    // Verify no `.d` file in `wt_b/target/debug/deps` leaks either `sym-anchor` or `real-anchor`!
    for entry in fs::read_dir(wt_b.join("target/debug/deps")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) == Some("d") {
            let text = fs::read_to_string(&path).unwrap();
            assert!(
                !text.contains("sym-anchor") && !text.contains("real-anchor"),
                "leaked Worktree A path into Worktree B dep-info {}:\n{text}",
                path.display()
            );
        }
    }
}

#[test]
fn gitpulse_rustc_expands_response_files_and_claim_cli_isolates_caller_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache_dir = tmp.path().join("rsp-cache");
    let wt_a = tmp.path().join("rsp-wt-a");
    let wt_b = tmp.path().join("rsp-wt-b");
    fs::create_dir_all(wt_a.join("src")).unwrap();
    fs::create_dir_all(wt_b.join("src")).unwrap();
    fs::create_dir_all(wt_a.join("out")).unwrap();
    fs::create_dir_all(wt_b.join("out")).unwrap();
    fs::write(wt_a.join("src/lib.rs"), "pub fn rsp_val() -> u32 { 77 }\n").unwrap();
    fs::write(wt_b.join("src/lib.rs"), "pub fn rsp_val() -> u32 { 77 }\n").unwrap();

    // Invoke `gitpulse-rustc` with `@args.rsp` response file in `wt_a` then `wt_b`.
    let rsp_a = wt_a.join("args.rsp");
    fs::write(
        &rsp_a,
        format!(
            "--crate-name\nrsp_demo\n--crate-type\nrlib\n--edition=2021\n--emit=dep-info,metadata,link\n-C\nextra-filename=-9999\n--out-dir\n{}\nsrc/lib.rs\n",
            wt_a.join("out").display()
        ),
    )
    .unwrap();
    let status_a = Command::new(rustc_wrapper_bin())
        .current_dir(&wt_a)
        .arg("rustc")
        .arg(format!("@{}", rsp_a.display()))
        .env("GITPULSE_BUILD_CACHE_DIR", &cache_dir)
        .env("GITPULSE_WORKTREE_ROOT", &wt_a)
        .status_locked()
        .unwrap();
    assert!(status_a.success());

    let rsp_b = wt_b.join("args.rsp");
    fs::write(
        &rsp_b,
        format!(
            "--crate-name\nrsp_demo\n--crate-type\nrlib\n--edition=2021\n--emit=dep-info,metadata,link\n-C\nextra-filename=-9999\n--out-dir\n{}\nsrc/lib.rs\n",
            wt_b.join("out").display()
        ),
    )
    .unwrap();
    let status_b = Command::new(rustc_wrapper_bin())
        .current_dir(&wt_b)
        .arg("rustc")
        .arg(format!("@{}", rsp_b.display()))
        .env("GITPULSE_BUILD_CACHE_DIR", &cache_dir)
        .env("GITPULSE_WORKTREE_ROOT", &wt_b)
        .status_locked()
        .unwrap();
    assert!(status_b.success());

    let stats: Value = serde_json::from_slice(&fs::read(cache_dir.join("stats.json")).unwrap()).unwrap();
    assert_eq!(
        stats["hits"].as_u64(),
        Some(1),
        "expected @args.rsp invocation in wt_b to hit cache instead of passthrough: {stats}"
    );

    // Also verify `gitpulse-worktree claim` returns ONLY conflicts involving the claiming agent.
    let repo = tmp.path().join("claim-repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@gitpulse.local"]);
    git(&repo, &["config", "user.name", "GitPulse Test"]);
    fs::write(repo.join("README.md"), "hi\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    approve_in_isolated_home(&repo, &home);

    let repo_str = repo.to_str().unwrap();
    run_cli_json_ok(
        &home,
        &["claim", "--repo", repo_str, "--agent", "lane-a", "--branch", "a", "--files", "src/shared.rs", "--json"],
    );
    let claim_b = run_cli_json_ok(
        &home,
        &["claim", "--repo", repo_str, "--agent", "lane-b", "--branch", "b", "--files", "src/shared.rs", "--json"],
    );
    assert_eq!(claim_b["claim_conflicts"].as_array().unwrap().len(), 1);

    // Unrelated `lane-c` claims disjoint `src/isolated.rs`: must NOT see `lane-a` vs `lane-b` conflict!
    let claim_c = run_cli_json_ok(
        &home,
        &["claim", "--repo", repo_str, "--agent", "lane-c", "--branch", "c", "--files", "src/isolated.rs", "--json"],
    );
    assert_eq!(
        claim_c["claim_conflicts"].as_array().unwrap().len(),
        0,
        "lane-c claimed disjoint file and must not receive unrelated lane-a/lane-b conflicts: {claim_c}"
    );
}

