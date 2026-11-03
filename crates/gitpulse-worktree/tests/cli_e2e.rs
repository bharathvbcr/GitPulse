//! End-to-end subprocess integration tests for the `gitpulse-worktree` binary.
//!
//! Exercises the compiled binary over OS pipes with an isolated trust profile:
//! 1. CLI help, version, and invalid-argument exit codes (`0` vs `2`).
//! 2. Fail-closed `REPOSITORY_TRUST_REQUIRED` refusal (`exit 1`) on untrusted repos.
//! 3. DevMap-guided `--sparse` cone expansion (`packages/app` -> `packages/shared`),
//!    selective CoW `node_modules` cloning, zero-copy `base_store_ref.json` sharing
//!    (zero `devmap.sqlite` copy), `compile-overlay`, multi-agent `claim`/`msg-send`/
//!    `msg-inbox`, cross-file 1-hop CSR `radar` collision detection, `verify`, and
//!    ODB-quarantined `merge`.

use devmap_store::Store;
use gitpulse_lib::procguard::LockedSpawn;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_gitpulse-worktree")
}

fn profile_env(home: &Path) -> [(&'static str, PathBuf); 3] {
    [
        ("HOME", home.to_path_buf()),
        ("APPDATA", home.join("AppData")),
        ("XDG_CONFIG_HOME", home.join(".config")),
    ]
}

fn approve_in_isolated_home(repo: &Path, home: &Path) {
    fs::create_dir_all(home).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "approve_subprocess_fixture_helper",
            "--ignored",
        ])
        .envs(profile_env(home))
        .env("GITPULSE_E2E_GRANT_REPO", repo)
        .status_locked()
        .unwrap();
    assert!(status.success(), "isolated fixture approval failed");
}

#[test]
#[ignore = "subprocess helper invoked by approve_in_isolated_home"]
fn approve_subprocess_fixture_helper() {
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
    Command::new(bin_path())
        .args(args)
        .envs(profile_env(home))
        .output_locked()
        .expect("spawn gitpulse-worktree")
}

fn run_cli_json_ok(home: &Path, args: &[&str]) -> Value {
    let mut full_args: Vec<&str> = args.to_vec();
    if !full_args.contains(&"--json") {
        full_args.push("--json");
    }
    let out = run_cli(home, &full_args);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "gitpulse-worktree {full_args:?} failed (code {:?}):\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("invalid JSON from {full_args:?}: {e}\nraw stdout: {stdout}")
    })
}

fn seed_monorepo_with_devmap_and_csr(anchor: &Path) {
    fs::write(anchor.join(".gitignore"), "node_modules/\n.devmap/\n").unwrap();
    fs::create_dir_all(anchor.join("packages/app")).unwrap();
    fs::create_dir_all(anchor.join("packages/shared")).unwrap();
    fs::create_dir_all(anchor.join("packages/unrelated")).unwrap();

    fs::write(
        anchor.join("packages/app/main.rs"),
        "pub fn run_app() { shared_helper(); }\n",
    )
    .unwrap();
    fs::write(
        anchor.join("packages/shared/lib.rs"),
        "pub fn shared_helper() {}\n",
    )
    .unwrap();
    fs::write(
        anchor.join("packages/unrelated/huge.rs"),
        "pub fn untouched_heavy_subsystem() {}\n",
    )
    .unwrap();

    git(anchor, &["add", "."]);
    git(anchor, &["commit", "-q", "-m", "initial monorepo"]);
    let head_sha = git(anchor, &["rev-parse", "HEAD"]);

    // Ignored nested caches: only `packages/app/node_modules` should be cloned into a
    // sparse checkout scoped to `packages/app`, NEVER `packages/unrelated/node_modules`!
    fs::create_dir_all(anchor.join("packages/app/node_modules/pkg-a")).unwrap();
    fs::write(
        anchor.join("packages/app/node_modules/pkg-a/index.js"),
        "module.exports = 1;\n",
    )
    .unwrap();
    fs::create_dir_all(anchor.join("packages/unrelated/node_modules/pkg-heavy")).unwrap();
    fs::write(
        anchor.join("packages/unrelated/node_modules/pkg-heavy/index.js"),
        "module.exports = 999;\n",
    )
    .unwrap();

    let canonical_anchor = anchor.canonicalize().unwrap();
    let db_path = devmap_query::paths::store_path(&canonical_anchor);
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let store = Store::open(&db_path).expect("open devmap store");
    drop(store);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE pending_state SET repo_root = ?1 WHERE singleton = 1",
        [canonical_anchor.to_str().unwrap()],
    )
    .unwrap();
    let files = [
        ("packages/app/main.rs", "run_app"),
        ("packages/shared/lib.rs", "shared_helper"),
        ("packages/unrelated/huge.rs", "untouched_heavy_subsystem"),
    ];
    for (idx, (path, _)) in files.iter().enumerate() {
        let id = (idx + 1) as i64;
        conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, *path))
            .unwrap();
    }
    conn.execute(
        "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
         VALUES (1, 1.0, ?1, ?2, ?3)",
        (
            &head_sha,
            r#"{"total_files":3,"total_symbols":3,"total_edges":1,"dead_symbols":[],"communities":[],"status":"Ok","unresolved_calls":0}"#,
            canonical_anchor.to_str().unwrap(),
        ),
    )
    .unwrap();

    for (idx, (path, sym)) in files.iter().enumerate() {
        let id = (idx + 1) as i64;
        let bytes = fs::read(canonical_anchor.join(path)).unwrap();
        let digest = devmap_store::IncrementalDigestEngine::hash_bytes(&bytes) as i64;
        conn.execute(
            "INSERT INTO file_payloads
               (payload_id, file_id, content_hash, language,
                parse_outcome_json, engine_json, extraction_json,
                entry_wiring_json, grammar_version, analyzer_version)
             VALUES (?1, ?1, ?2, 'rust', '\"Clean\"', '\"ConfigScanner\"', '{}', '[]', 'v1', 'v1')",
            (id, digest),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_rows (file_id, payload_id, valid_from, valid_to)
             VALUES (?1, ?1, 1, NULL)",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO payload_nodes
               (payload_id, ord_in_file, name, qualified_name, kind,
                span_start, span_end, is_exported)
             VALUES (?1, 0, ?2, ?2, 'Function', 0, ?3, 1)",
            (id, *sym, bytes.len() as i64),
        )
        .unwrap();
        let fts_rowid = id << 24;
        conn.execute(
            "INSERT INTO nodes_fts (rowid, name, qualified_name, path)
             VALUES (?1, ?2, ?2, ?3)",
            (fts_rowid, *sym, *path),
        )
        .unwrap();
    }

    // Outbound edge: `packages/app/main.rs::run_app` -> `packages/shared/lib.rs::shared_helper`
    conn.execute(
        "INSERT INTO edge_rows
           (edge_id, source_file_id, target_file_id,
            source_symbol, target_symbol, edge_kind, confidence,
            valid_from, valid_to)
         VALUES (0, 1, 2, 'run_app', 'shared_helper', 'Calls', 0.95, 1, NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO build_history
           (generation_id, built_at, head_sha, files, symbols, edges,
            dead_confident, dead_ambiguous, parse_failed, languages_covered,
            build_ms, db_bytes)
         VALUES (1, 1.0, ?1, 3, 3, 1, 0, 0, 0, 1, 5, 4096)",
        [&head_sha],
    )
    .unwrap();
    conn.close().unwrap();

    // Materialize the mmap CSR graph sidecar from the SQLite store's edge index.
    let store = Store::open(&db_path).unwrap();
    let (_, edge_index) = store.generation_edges_with_id().unwrap().unwrap();
    let csr_path = Store::csr_sidecar_path(&db_path);
    edge_index.to_csr_writer().write_to_file(&csr_path).unwrap();
    assert!(csr_path.is_file(), "CSR sidecar must exist");
}

#[test]
fn cli_help_version_invalid_args_and_untrusted_refusal() {
    let home = tempfile::tempdir().unwrap();

    let help = run_cli(home.path(), &["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("compile-overlay"));

    let version = run_cli(home.path(), &["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("gitpulse-worktree "));

    let bad = run_cli(home.path(), &["--json", "create", "--repo", "/tmp/x"]);
    assert_eq!(bad.status.code(), Some(2));
    let bad_json: Value = serde_json::from_slice(&bad.stdout).unwrap();
    assert_eq!(bad_json["ok"], false);
    assert!(bad_json["error"].as_str().unwrap().contains("--branch"));

    // Untrusted repository MUST fail closed with exit 1 and REPOSITORY_TRUST_REQUIRED.
    let untrusted_dir = tempfile::tempdir().unwrap();
    git(untrusted_dir.path(), &["init", "-q", "-b", "main"]);
    let untrusted = run_cli(
        home.path(),
        &[
            "--json",
            "status",
            "--repo",
            untrusted_dir.path().to_str().unwrap(),
        ],
    );
    assert_eq!(untrusted.status.code(), Some(1));
    let err_json: Value = serde_json::from_slice(&untrusted.stdout).unwrap();
    assert_eq!(err_json["ok"], false);
    assert!(
        err_json["error"]
            .as_str()
            .unwrap()
            .contains("REPOSITORY_TRUST_REQUIRED"),
        "expected trust refusal, got: {err_json}"
    );
}

#[test]
fn cli_end_to_end_zero_copy_sparse_overlay_agent_bus_radar_and_merge() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let anchor = workspace.path().join("monorepo");
    fs::create_dir_all(&anchor).unwrap();
    git(&anchor, &["init", "-q", "-b", "main"]);
    git(&anchor, &["config", "user.name", "E2E Agent"]);
    git(&anchor, &["config", "user.email", "agent@example.com"]);

    seed_monorepo_with_devmap_and_csr(&anchor);
    approve_in_isolated_home(&anchor, home.path());

    let anchor_str = anchor.to_str().unwrap();

    // 1. `expand-cone`: asking only for `packages/app` automatically pulls in `packages/shared`!
    let cone = run_cli_json_ok(
        home.path(),
        &["expand-cone", "--repo", anchor_str, "--paths", "packages/app"],
    );
    assert_eq!(
        cone["materialized_cone_dirs"],
        serde_json::json!(["packages/app", "packages/shared"])
    );

    // 2. `create --sparse packages/app --zero-copy --agent claude-a`
    let wt_a_target = workspace.path().join("wt-lane-a");
    let created_a = run_cli_json_ok(
        home.path(),
        &[
            "create",
            "--repo",
            anchor_str,
            "--branch",
            "agent/lane-a",
            "--target",
            wt_a_target.to_str().unwrap(),
            "--sparse",
            "packages/app",
            "--zero-copy",
            "--agent",
            "claude-a",
        ],
    );
    assert_eq!(created_a["ok"], true);
    assert_eq!(created_a["zero_copy_devmap"], true);
    assert_eq!(
        created_a["sparse_cone"],
        serde_json::json!(["packages/app", "packages/shared"])
    );

    // Verify sparse file materialization + selective cache cloning + zero SQLite copy!
    assert!(wt_a_target.join("packages/app/main.rs").is_file());
    assert!(wt_a_target.join("packages/shared/lib.rs").is_file());
    assert!(
        !wt_a_target.join("packages/unrelated/huge.rs").exists(),
        "unrelated directory must NOT be materialized in sparse worktree"
    );
    assert!(
        wt_a_target
            .join("packages/app/node_modules/pkg-a/index.js")
            .is_file(),
        "in-cone node_modules must be selectively CoW-cloned"
    );
    assert!(
        !wt_a_target.join("packages/unrelated/node_modules").exists(),
        "out-of-cone node_modules must NOT be cloned"
    );
    assert!(
        !devmap_query::paths::store_path(&wt_a_target).exists(),
        "zero-copy worktree must NOT duplicate devmap.sqlite"
    );
    assert!(
        gitpulse_lib::codeintel::zero_copy_base_ref_path(wt_a_target.to_str().unwrap()).is_file(),
        "zero-copy worktree must write <512B base_store_ref.json"
    );

    // 3. Create parallel zero-copy `agent/lane-b` scoped to `packages/shared`
    let wt_b_target = workspace.path().join("wt-lane-b");
    let created_b = run_cli_json_ok(
        home.path(),
        &[
            "create",
            "--repo",
            anchor_str,
            "--branch",
            "agent/lane-b",
            "--target",
            wt_b_target.to_str().unwrap(),
            "--sparse",
            "packages/shared",
            "--zero-copy",
            "--agent",
            "codex-b",
        ],
    );
    assert_eq!(created_b["zero_copy_devmap"], true);

    // 4. Coordinate across lanes via `claim`, `msg-send`, and `msg-inbox`
    let claim_b = run_cli_json_ok(
        home.path(),
        &[
            "claim",
            "--repo",
            wt_b_target.to_str().unwrap(),
            "--agent",
            "codex-b",
            "--branch",
            "agent/lane-b",
            "--worktree",
            wt_b_target.to_str().unwrap(),
            "--files",
            "packages/shared/lib.rs",
            "--symbols",
            "shared_helper",
        ],
    );
    assert_eq!(claim_b["ok"], true);

    let sent = run_cli_json_ok(
        home.path(),
        &[
            "msg-send",
            "--repo",
            wt_b_target.to_str().unwrap(),
            "--from",
            "codex-b",
            "--to",
            "claude-a",
            "--kind",
            "interface_change",
            "--body",
            "Changing shared_helper() signature to take Context",
            "--branch",
            "agent/lane-b",
        ],
    );
    assert_eq!(sent["message"]["seq"], 1);

    let inbox_a = run_cli_json_ok(
        home.path(),
        &[
            "msg-inbox",
            "--repo",
            wt_a_target.to_str().unwrap(),
            "--agent",
            "claude-a",
        ],
    );
    assert_eq!(inbox_a["inbox"]["messages"].as_array().unwrap().len(), 1);

    // 5. Cross-file edits + `compile-overlay` + 3-layer `radar`!
    // Agent A edits `packages/app/main.rs`; Agent B edits `packages/shared/lib.rs`.
    // Zero overlapping file paths, yet 1-hop CSR graph links `run_app -> shared_helper`!
    fs::write(
        wt_a_target.join("packages/app/main.rs"),
        "pub fn run_app() { shared_helper(); /* lane-a edit */ }\n",
    )
    .unwrap();
    fs::write(
        wt_b_target.join("packages/shared/lib.rs"),
        "pub fn shared_helper() { /* lane-b edit */ }\n",
    )
    .unwrap();

    let ov_a = run_cli_json_ok(
        home.path(),
        &["compile-overlay", "--worktree", wt_a_target.to_str().unwrap()],
    );
    assert_eq!(ov_a["report"]["modified_files"], 1);
    assert_eq!(ov_a["report"]["dirty_nodes"], 1);

    let ov_b = run_cli_json_ok(
        home.path(),
        &["compile-overlay", "--worktree", wt_b_target.to_str().unwrap()],
    );
    assert_eq!(ov_b["report"]["modified_files"], 1);
    assert_eq!(ov_b["report"]["dirty_nodes"], 1);

    let radar = run_cli_json_ok(home.path(), &["radar", "--repo", anchor_str]);
    assert_eq!(radar["radar"]["csr_graph_available"], true);
    let sem = radar["radar"]["semantic_conflicts"].as_array().unwrap();
    assert!(
        sem.iter()
            .any(|c| c["relation"] == "caller_callee_1hop"
                && c["caller_callee_linked_nodes"].as_u64().unwrap_or(0) >= 1),
        "expected cross-file 1-hop caller/callee semantic conflict in radar: {radar}"
    );

    // 6. Commit in `lane-a`, `verify` (not yet integrated), `merge` into `main`, and `release`!
    git(&wt_a_target, &["add", "packages/app/main.rs"]);
    git(&wt_a_target, &["commit", "-q", "-m", "feat(app): update run_app"]);

    let pre_verify = run_cli_json_ok(
        home.path(),
        &["verify", "--repo", anchor_str, "--branch", "agent/lane-a"],
    );
    assert_eq!(pre_verify["verification"]["is_integrated"], false);

    let merged = run_cli_json_ok(
        home.path(),
        &[
            "merge",
            "--repo",
            anchor_str,
            "--source",
            "agent/lane-a",
            "--target",
            "main",
            "--message",
            "feat: squash merge agent/lane-a",
        ],
    );
    assert_eq!(merged["ok"], true);
    assert_eq!(merged["worktree_removed"], true);
    assert!(!wt_a_target.exists(), "merged worktree must be removed");
    assert!(
        fs::read_to_string(anchor.join("packages/app/main.rs"))
            .unwrap()
            .contains("lane-a edit"),
        "main checkout must receive squashed sparse-worktree commit while preserving full monorepo"
    );
    assert!(
        anchor.join("packages/unrelated/huge.rs").is_file(),
        "sparse worktree commit must preserve untouched monorepo files outside the sparse cone!"
    );

    // `merge` tore down `agent/lane-a` and its worktree, automatically releasing `claude-a`'s
    // lane claim; an explicit follow-up `release` for `claude-a` is a clean idempotent no-op,
    // while `codex-b` (still unmerged) releases with `released == true`.
    let released_a = run_cli_json_ok(
        home.path(),
        &["release", "--repo", anchor_str, "--agent", "claude-a"],
    );
    assert_eq!(released_a["released"], false);

    let released_b = run_cli_json_ok(
        home.path(),
        &["release", "--repo", anchor_str, "--agent", "codex-b"],
    );
    assert_eq!(released_b["released"], true);
}

#[test]
fn cli_refuses_every_subcommand_on_untrusted_repository() {
    let home = tempfile::tempdir().unwrap();
    let untrusted_dir = tempfile::tempdir().unwrap();
    let repo = untrusted_dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.name", "Test"]);
    git(repo, &["config", "user.email", "test@example.com"]);
    fs::write(repo.join("README.md"), "untrusted\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "init"]);

    let repo_str = repo.to_str().unwrap();
    let cases: &[&[&str]] = &[
        &["--json", "status", "--repo", repo_str],
        &["--json", "create", "--repo", repo_str, "--branch", "feat/x"],
        &["--json", "expand-cone", "--repo", repo_str, "--paths", "README.md"],
        &["--json", "compile-overlay", "--worktree", repo_str],
        &[
            "--json",
            "claim",
            "--repo",
            repo_str,
            "--agent",
            "rogue",
            "--branch",
            "agent/rogue",
            "--files",
            "README.md",
        ],
        &["--json", "release", "--repo", repo_str, "--agent", "rogue"],
        &[
            "--json",
            "msg-send",
            "--repo",
            repo_str,
            "--from",
            "rogue",
            "--kind",
            "status",
            "--body",
            "bypass",
        ],
        &["--json", "msg-inbox", "--repo", repo_str, "--agent", "rogue"],
        &["--json", "radar", "--repo", repo_str],
        &["--json", "verify", "--repo", repo_str, "--branch", "main"],
        &[
            "--json",
            "merge",
            "--repo",
            repo_str,
            "--source",
            "main",
            "--target",
            "main",
        ],
        &["--json", "prune", "--repo", repo_str],
    ];

    for args in cases {
        let out = run_cli(home.path(), args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "subcommand {args:?} must fail with exit 1 on untrusted repo; stdout={}",
            String::from_utf8_lossy(&out.stdout)
        );
        let payload: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(payload["ok"], false, "subcommand {args:?}: {payload}");
        assert!(
            payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("REPOSITORY_TRUST_REQUIRED"),
            "subcommand {args:?} bypassed trust gate: {payload}"
        );
    }
}

#[test]
fn cli_cross_process_concurrent_claims_messages_and_auto_release_on_merge() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let anchor = workspace.path().join("concurrent-repo");
    fs::create_dir_all(&anchor).unwrap();
    git(&anchor, &["init", "-q", "-b", "main"]);
    git(&anchor, &["config", "user.name", "Concurrency Agent"]);
    git(&anchor, &["config", "user.email", "concurrency@example.com"]);
    fs::write(anchor.join("lib.rs"), "pub fn base() {}\n").unwrap();
    git(&anchor, &["add", "."]);
    git(&anchor, &["commit", "-q", "-m", "init"]);
    approve_in_isolated_home(&anchor, home.path());

    let anchor_str = anchor.to_str().unwrap();

    // Spawn 12 parallel OS processes claiming distinct files simultaneously.
    let mut claim_children = Vec::new();
    for i in 0..12 {
        let agent = format!("agent-{i:02}");
        let branch = format!("agent/lane-{i:02}");
        let file = format!("src/mod_{i:02}.rs");
        let child = Command::new(bin_path())
            .args([
                "--json",
                "claim",
                "--repo",
                anchor_str,
                "--agent",
                &agent,
                "--branch",
                &branch,
                "--files",
                &file,
            ])
            .envs(profile_env(home.path()))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn_locked()
            .expect("spawn parallel claim");
        claim_children.push(child);
    }
    for child in claim_children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "parallel claim failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Spawn 12 parallel OS processes sending broadcast messages simultaneously.
    let mut msg_children = Vec::new();
    for i in 0..12 {
        let agent = format!("agent-{i:02}");
        let body = format!("hello-{i:02}");
        let child = Command::new(bin_path())
            .args([
                "--json",
                "msg-send",
                "--repo",
                anchor_str,
                "--from",
                &agent,
                "--kind",
                "status",
                "--body",
                &body,
            ])
            .envs(profile_env(home.path()))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn_locked()
            .expect("spawn parallel msg-send");
        msg_children.push(child);
    }
    for child in msg_children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "parallel msg-send failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let status = run_cli_json_ok(home.path(), &["status", "--repo", anchor_str]);
    let active_lanes = status["active_lanes"].as_array().unwrap();
    assert_eq!(
        active_lanes.len(),
        12,
        "all 12 concurrent cross-process claims must survive without lost updates: {active_lanes:?}"
    );

    let inbox = run_cli_json_ok(
        home.path(),
        &["msg-inbox", "--repo", anchor_str, "--agent", "observer"],
    );
    let messages = inbox["inbox"]["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        12,
        "all 12 concurrent cross-process messages must survive without lost updates"
    );
    let mut seqs: Vec<u64> = messages.iter().map(|m| m["seq"].as_u64().unwrap()).collect();
    seqs.sort_unstable();
    assert_eq!(
        seqs,
        (1..=12).collect::<Vec<u64>>(),
        "concurrent cross-process message seq IDs must be strictly unique 1..=12"
    );

    // Create an agent worktree with `--agent merge-worker`, commit, and `merge` without manual `release` —
    // teardown MUST automatically release `merge-worker`'s lane slot!
    let wt_target = workspace.path().join("wt-merge-worker");
    run_cli_json_ok(
        home.path(),
        &[
            "create",
            "--repo",
            anchor_str,
            "--branch",
            "agent/merge-worker",
            "--target",
            wt_target.to_str().unwrap(),
            "--agent",
            "merge-worker",
        ],
    );
    fs::write(wt_target.join("worker.rs"), "pub fn worker() {}\n").unwrap();
    git(&wt_target, &["add", "worker.rs"]);
    git(&wt_target, &["commit", "-q", "-m", "feat: worker"]);

    let merged = run_cli_json_ok(
        home.path(),
        &[
            "merge",
            "--repo",
            anchor_str,
            "--source",
            "agent/merge-worker",
            "--target",
            "main",
        ],
    );
    assert_eq!(merged["worktree_removed"], true);

    let post_merge_status = run_cli_json_ok(home.path(), &["status", "--repo", anchor_str]);
    let remaining_agents: Vec<&str> = post_merge_status["active_lanes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["agent_id"].as_str())
        .collect();
    assert!(
        !remaining_agents.contains(&"merge-worker"),
        "merge teardown must auto-release merge-worker lane slot, remaining={remaining_agents:?}"
    );
}

