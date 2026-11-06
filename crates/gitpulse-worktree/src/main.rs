//! Standalone worktree & branch orchestrator, DevMap delta overlay compiler,
//! and lock-free multi-agent coordination CLI for GitPulse.
//!
//! Designed for parallel agentic lanes working concurrently in the same Git
//! repository family:
//! - Zero-copy DevMap shared base graph pointers (`base_store_ref.json` ->
//!   anchor `devmap.sqlite` + mmap `.csr` + `cas/ast/`)
//! - DevMap-guided `--cone` sparse checkouts (`add_sparse_worktree_zero_copy`)
//!   with selective Copy-on-Write (`clonefile` / `FICLONE`) cache cloning
//! - Ephemeral Blake3/CAS `WorktreeOverlay` + `RoaringBitmap` dirty CSR node
//!   compilation (`compile-overlay`)
//! - Lock-free/atomic `<git-common-dir>/gitpulse/lanes/` pre-edit claim bus,
//!   bounded agent-to-agent message ring, and 3-layer collision `radar`
//! - Zero-copy content-addressed `gitpulse-rustc` compilation (`build`) and
//!   cache inspection / pruning (`cache-stats`)
//! - 6-point branch integration verification (`verify`) and ODB-quarantined
//!   squash/non-squash merge pipeline (`merge`, `prune`).

use gitpulse_lib::codeintel::{
    load_worktree_overlay, load_zero_copy_base_ref, provision_worktree_devmap,
    provision_worktree_devmap_zero_copy, sync_worktree_overlay, zero_copy_base_ref_path,
};
use gitpulse_lib::engine::agent_bus::{
    list_lanes, post_message, read_messages, register_or_update_lane,
    register_or_update_lane_with_conflicts, release_lane, scan_parallel_agent_radar,
};
use gitpulse_lib::engine::cow_clone::{clone_ignored_caches, CacheSelection};
use gitpulse_lib::engine::git_cli::capture_command;
use gitpulse_lib::engine::git_reader::compute_default_base;
use gitpulse_lib::engine::worktree::{
    add_sparse_worktree_zero_copy, add_worktree, add_worktree_argv, add_worktree_with_devmap,
    expand_sparse_cone_with_devmap, list_worktrees, merge_worktree_pipeline, prune_worktree,
    reap_processes_in_dir, remove_worktree, remove_worktree_argv, resolve_worktree_family,
    resolve_worktree_path_for_branch, user_changed_paths, verify_branch_integration, DevmapMode,
    DevmapOutcome,
};
use gitpulse_lib::ipc_error::CommandError;
use gitpulse_worktree::build_cache::{
    inspect_or_prune_cache, resolve_build_cache_dir, resolve_gitpulse_rustc_bin,
    ENV_BUILD_CACHE_DIR, ENV_WORKTREE_ROOT,
};
use serde_json::{json, Value};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
gitpulse-worktree — Zero-copy worktree engine, DevMap overlay compiler & parallel agent bus

USAGE:
    gitpulse-worktree <SUBCOMMAND> [OPTIONS]

GLOBAL OPTIONS:
    --json               Emit structured JSON output on stdout
    -h, --help           Print help information
    -V, --version        Print version

SUBCOMMANDS:
    create               Create a worktree (full, --zero-copy, or DevMap-expanded --sparse)
      --repo <path>      Repository checkout path (required)
      --branch <name>    New branch name (required)
      --base <rev>       Starting revision / branch
      --target <dir>     Explicit absolute target directory
      --sparse <p1,p2>   Comma-separated paths for DevMap-guided sparse --cone checkout
      --zero-copy        Share anchor DevMap SQLite/CSR/CAS via stack_lease.json + base_store_ref.json
      --devmap-mode <m>  DevMap provisioning mode: copy, stacked, zero-copy, or none
      --no-cache         Skip Copy-on-Write ignored build cache cloning
      --exec <cmd>       Guard and run a post-create command (60s timeout)
      --agent <id>       Register the lane in <git-common-dir>/gitpulse/lanes/claims.json

    switch               Locate an existing branch worktree (or create with --create)
      --repo <path>      Repository checkout path (required)
      --branch <name>    Branch name (required)
      --create           Create worktree if missing
      --base <rev>       Base revision when creating
      --zero-copy        Provision stacked/zero-copy DevMap when creating
      --devmap-mode <m>  DevMap provisioning mode when creating: copy, stacked, zero-copy, or none

    isolate              Provision CoW caches and DevMap state into an existing worktree
      --repo <path>      Repository checkout path (required)
      --worktree <dir>   Target worktree path (required)
      --zero-copy        Use stacked/zero-copy DevMap instead of CoW SQLite clone
      --devmap-mode <m>  DevMap provisioning mode: copy, stacked, zero-copy, or none
      --include-venv     Include .venv / venv in CoW cache cloning

    compile-overlay      Compile dirty files into overlay.json + CAS ASTs + RoaringBitmap CSR nodes
      --worktree <dir>   Target worktree path (required)

    expand-cone          Expand scope paths via DevMap file-dependency graph into --cone dirs
      --repo <path>      Repository checkout path (required)
      --paths <p1,p2>    Comma-separated repo-relative file or directory paths (required)

    status               Inspect worktrees, zero-copy vs local DevMap state, overlays & agent lanes
      --repo <path>      Repository checkout path (required)

    claim                Register or update an agent lane's file & symbol intent claims
      --repo <path>      Repository checkout path (required)
      --agent <id>       Agent identifier (required)
      --branch <name>    Branch name (required)
      --worktree <dir>   Worktree path
      --sparse <d1,d2>   Sparse cone directories
      --files <f1,f2>    Claimed repo-relative files or directories
      --symbols <s1,s2>  Claimed symbol names

    release              Release a finished agent lane's claim record
      --repo <path>      Repository checkout path (required)
      --agent <id>       Agent identifier (required)

    msg-send             Post a bounded message to <git-common-dir>/gitpulse/lanes/messages.json
      --repo <path>      Repository checkout path (required)
      --from <agent>     Sender agent ID (required)
      --to <agent>       Recipient agent ID (omit for broadcast)
      --kind <kind>      Message kind, e.g. handoff, interface_change, blocker (required)
      --body <text>      Message payload up to 16 KiB (required)
      --branch <name>    Sender branch context

    msg-inbox            Read direct + broadcast coordination messages from the lane bus
      --repo <path>      Repository checkout path (required)
      --agent <id>       Filter inbox for agent ID (omit for all messages)
      --since-seq <n>    Only messages with seq > n (default: 0)

    radar                Run 3-layer collision radar (claims + 1-hop CSR semantic + ODB merge-tree)
      --repo <path>      Repository checkout path (required)

    verify               Run the 6-point branch integration verification algorithm
      --repo <path>      Repository checkout path (required)
      --branch <name>    Candidate branch to check (required)
      --target <base>    Target base branch (defaults to repo default base)

    merge                Execute ODB-quarantined worktree merge & teardown pipeline
      --repo <path>      Repository checkout path (required)
      --source <branch>  Source branch to merge (required)
      --target <base>    Target branch (defaults to repo default base)
      --no-squash        Preserve merge commit / fast-forward instead of squash
      --message <msg>    Explicit commit message
      --keep-worktree    Keep worktree and branch after merge

    prune                Prune clean, 6-point-integrated worktrees with process reaping
      --repo <path>      Repository checkout path (required)
      --branch <name>    Restrict pruning to a single branch
      --dry-run          Report prunable worktrees without deleting
      --force            Prune even if unintegrated
      --no-reap          Skip process reaping inside pruned worktrees

    build                Run cargo in a worktree with zero-copy gitpulse-rustc build caching
      --repo <path>      Repository checkout path (required)
      --worktree <dir>   Target worktree path (defaults to --repo)
      --rustc-wrapper <p> Explicit path to gitpulse-rustc binary
      -- <cargo args...> Cargo subcommand and flags (defaults to: build)

    cache-stats          Inspect or prune <git-common-dir>/gitpulse/build-cache
      --repo <path>      Repository checkout path (required)
      --prune            Evict oldest blobs and invalidated manifests above --max-bytes
      --max-bytes <n>    Maximum retained blob bytes when --prune is set (default: 4 GiB)
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliDevmapMode {
    Copy,
    Stacked,
    None,
}

impl CliDevmapMode {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "copy" => Ok(Self::Copy),
            "stacked" | "zero-copy" | "zero_copy" => Ok(Self::Stacked),
            "none" | "off" => Ok(Self::None),
            other => Err(format!(
                "invalid --devmap-mode {other:?} (expected copy, stacked, zero-copy, or none)"
            )),
        }
    }

    fn resolve(zero_copy: bool, explicit: Option<Self>) -> Self {
        match explicit {
            Some(mode) => mode,
            None if zero_copy => Self::Stacked,
            None => Self::Copy,
        }
    }

    fn to_engine_mode(self) -> DevmapMode {
        match self {
            Self::Copy => DevmapMode::Copy,
            Self::Stacked => DevmapMode::ZeroCopy,
            Self::None => DevmapMode::None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    Command { json: bool, cmd: Subcommand },
}

#[derive(Debug, PartialEq, Eq)]
enum Subcommand {
    Create {
        repo: String,
        branch: String,
        base: Option<String>,
        target: Option<String>,
        sparse: Vec<String>,
        zero_copy: bool,
        devmap_mode: Option<CliDevmapMode>,
        share_cache: bool,
        exec: Option<String>,
        agent: Option<String>,
    },
    Switch {
        repo: String,
        branch: String,
        create: bool,
        base: Option<String>,
        zero_copy: bool,
        devmap_mode: Option<CliDevmapMode>,
    },
    Isolate {
        repo: String,
        worktree: String,
        zero_copy: bool,
        devmap_mode: Option<CliDevmapMode>,
        include_venv: bool,
    },
    CompileOverlay {
        worktree: String,
    },
    ExpandCone {
        repo: String,
        paths: Vec<String>,
    },
    Status {
        repo: String,
    },
    Claim {
        repo: String,
        agent: String,
        branch: String,
        worktree: Option<String>,
        sparse: Vec<String>,
        files: Vec<String>,
        symbols: Vec<String>,
    },
    Release {
        repo: String,
        agent: String,
    },
    MsgSend {
        repo: String,
        from: String,
        to: Option<String>,
        kind: String,
        body: String,
        branch: Option<String>,
    },
    MsgInbox {
        repo: String,
        agent: Option<String>,
        since_seq: u64,
    },
    Radar {
        repo: String,
    },
    Verify {
        repo: String,
        branch: String,
        target: Option<String>,
    },
    Merge {
        repo: String,
        source: String,
        target: Option<String>,
        squash: bool,
        message: Option<String>,
        remove_wt: bool,
    },
    Prune {
        repo: String,
        branch: Option<String>,
        dry_run: bool,
        force: bool,
        reap: bool,
    },
    Build {
        repo: String,
        worktree: Option<String>,
        rustc_wrapper: Option<String>,
        cargo_args: Vec<String>,
    },
    CacheStats {
        repo: String,
        prune: bool,
        max_bytes: Option<u64>,
    },
}

fn split_csv_into(raw: &str, out: &mut Vec<String>) -> Result<(), String> {
    let mut added = 0usize;
    for part in raw.split(',') {
        let trimmed = part.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
            added += 1;
        }
    }
    if added == 0 {
        return Err("list flag value must not be empty".to_string());
    }
    Ok(())
}

fn take_val(args: &[String], idx: &mut usize, flag: &str) -> Result<String, String> {
    *idx += 1;
    let Some(val) = args.get(*idx) else {
        return Err(format!("missing value for {flag}"));
    };
    if val.trim().is_empty() {
        return Err(format!("value for {flag} must not be empty"));
    }
    Ok(val.clone())
}

fn require_flag(opt: Option<String>, flag: &str, sub: &str) -> Result<String, String> {
    opt.ok_or_else(|| format!("{sub} requires {flag}"))
}

fn parse_cli(argv: &[String]) -> Result<Invocation, (bool, String)> {
    let (head, passthrough) = match argv.iter().position(|a| a == "--") {
        Some(sep) => (&argv[..sep], Some(&argv[sep + 1..])),
        None => (argv, None),
    };

    let mut json_mode = false;
    let mut filtered = Vec::with_capacity(head.len());
    for arg in head {
        if arg == "--json" {
            json_mode = true;
        } else {
            filtered.push(arg.clone());
        }
    }

    if filtered.is_empty() {
        return Err((
            json_mode,
            "missing subcommand (run `gitpulse-worktree --help` for usage)".to_string(),
        ));
    }

    if filtered.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(Invocation::Help);
    }
    if filtered.len() == 1 && (filtered[0] == "-V" || filtered[0] == "--version") {
        return Ok(Invocation::Version);
    }

    let sub = filtered[0].as_str();
    let rest = &filtered[1..];
    let map_err = |msg: String| (json_mode, msg);

    if passthrough.is_some() && sub != "build" {
        return Err((
            json_mode,
            format!("{sub} does not accept passthrough arguments after `--`"),
        ));
    }

    let cmd = match sub {
        "create" => {
            let mut repo = None;
            let mut branch = None;
            let mut base = None;
            let mut target = None;
            let mut sparse = Vec::new();
            let mut zero_copy = false;
            let mut devmap_mode = None;
            let mut share_cache = true;
            let mut exec = None;
            let mut agent = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--branch" => {
                        branch = Some(take_val(rest, &mut i, "--branch").map_err(map_err)?)
                    }
                    "--base" => base = Some(take_val(rest, &mut i, "--base").map_err(map_err)?),
                    "--target" => {
                        target = Some(take_val(rest, &mut i, "--target").map_err(map_err)?)
                    }
                    "--sparse" => {
                        let v = take_val(rest, &mut i, "--sparse").map_err(map_err)?;
                        split_csv_into(&v, &mut sparse).map_err(map_err)?;
                    }
                    "--zero-copy" => zero_copy = true,
                    "--devmap-mode" => {
                        let v = take_val(rest, &mut i, "--devmap-mode").map_err(map_err)?;
                        devmap_mode = Some(CliDevmapMode::parse(&v).map_err(map_err)?);
                    }
                    "--no-cache" => share_cache = false,
                    "--exec" => exec = Some(take_val(rest, &mut i, "--exec").map_err(map_err)?),
                    "--agent" => agent = Some(take_val(rest, &mut i, "--agent").map_err(map_err)?),
                    other => return Err((json_mode, format!("unknown flag for create: {other}"))),
                }
                i += 1;
            }
            Subcommand::Create {
                repo: require_flag(repo, "--repo", "create").map_err(map_err)?,
                branch: require_flag(branch, "--branch", "create").map_err(map_err)?,
                base,
                target,
                sparse,
                zero_copy,
                devmap_mode,
                share_cache,
                exec,
                agent,
            }
        }
        "switch" => {
            let mut repo = None;
            let mut branch = None;
            let mut create = false;
            let mut base = None;
            let mut zero_copy = false;
            let mut devmap_mode = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--branch" => {
                        branch = Some(take_val(rest, &mut i, "--branch").map_err(map_err)?)
                    }
                    "--create" => create = true,
                    "--base" => base = Some(take_val(rest, &mut i, "--base").map_err(map_err)?),
                    "--zero-copy" => zero_copy = true,
                    "--devmap-mode" => {
                        let v = take_val(rest, &mut i, "--devmap-mode").map_err(map_err)?;
                        devmap_mode = Some(CliDevmapMode::parse(&v).map_err(map_err)?);
                    }
                    other => return Err((json_mode, format!("unknown flag for switch: {other}"))),
                }
                i += 1;
            }
            Subcommand::Switch {
                repo: require_flag(repo, "--repo", "switch").map_err(map_err)?,
                branch: require_flag(branch, "--branch", "switch").map_err(map_err)?,
                create,
                base,
                zero_copy,
                devmap_mode,
            }
        }
        "isolate" => {
            let mut repo = None;
            let mut worktree = None;
            let mut zero_copy = false;
            let mut devmap_mode = None;
            let mut include_venv = false;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--worktree" => {
                        worktree = Some(take_val(rest, &mut i, "--worktree").map_err(map_err)?)
                    }
                    "--zero-copy" => zero_copy = true,
                    "--devmap-mode" => {
                        let v = take_val(rest, &mut i, "--devmap-mode").map_err(map_err)?;
                        devmap_mode = Some(CliDevmapMode::parse(&v).map_err(map_err)?);
                    }
                    "--include-venv" => include_venv = true,
                    other => return Err((json_mode, format!("unknown flag for isolate: {other}"))),
                }
                i += 1;
            }
            Subcommand::Isolate {
                repo: require_flag(repo, "--repo", "isolate").map_err(map_err)?,
                worktree: require_flag(worktree, "--worktree", "isolate").map_err(map_err)?,
                zero_copy,
                devmap_mode,
                include_venv,
            }
        }
        "compile-overlay" => {
            let mut worktree = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--worktree" => {
                        worktree = Some(take_val(rest, &mut i, "--worktree").map_err(map_err)?)
                    }
                    other => {
                        return Err((
                            json_mode,
                            format!("unknown flag for compile-overlay: {other}"),
                        ))
                    }
                }
                i += 1;
            }
            Subcommand::CompileOverlay {
                worktree: require_flag(worktree, "--worktree", "compile-overlay")
                    .map_err(map_err)?,
            }
        }
        "expand-cone" => {
            let mut repo = None;
            let mut paths = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--paths" | "--path" => {
                        let v = take_val(rest, &mut i, "--paths").map_err(map_err)?;
                        split_csv_into(&v, &mut paths).map_err(map_err)?;
                    }
                    other => {
                        return Err((json_mode, format!("unknown flag for expand-cone: {other}")))
                    }
                }
                i += 1;
            }
            if paths.is_empty() {
                return Err((json_mode, "expand-cone requires --paths".to_string()));
            }
            Subcommand::ExpandCone {
                repo: require_flag(repo, "--repo", "expand-cone").map_err(map_err)?,
                paths,
            }
        }
        "status" => {
            let mut repo = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    other => return Err((json_mode, format!("unknown flag for status: {other}"))),
                }
                i += 1;
            }
            Subcommand::Status {
                repo: require_flag(repo, "--repo", "status").map_err(map_err)?,
            }
        }
        "claim" => {
            let mut repo = None;
            let mut agent = None;
            let mut branch = None;
            let mut worktree = None;
            let mut sparse = Vec::new();
            let mut files = Vec::new();
            let mut symbols = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--agent" => agent = Some(take_val(rest, &mut i, "--agent").map_err(map_err)?),
                    "--branch" => {
                        branch = Some(take_val(rest, &mut i, "--branch").map_err(map_err)?)
                    }
                    "--worktree" => {
                        worktree = Some(take_val(rest, &mut i, "--worktree").map_err(map_err)?)
                    }
                    "--sparse" => {
                        let v = take_val(rest, &mut i, "--sparse").map_err(map_err)?;
                        split_csv_into(&v, &mut sparse).map_err(map_err)?;
                    }
                    "--files" => {
                        let v = take_val(rest, &mut i, "--files").map_err(map_err)?;
                        split_csv_into(&v, &mut files).map_err(map_err)?;
                    }
                    "--symbols" => {
                        let v = take_val(rest, &mut i, "--symbols").map_err(map_err)?;
                        split_csv_into(&v, &mut symbols).map_err(map_err)?;
                    }
                    other => return Err((json_mode, format!("unknown flag for claim: {other}"))),
                }
                i += 1;
            }
            Subcommand::Claim {
                repo: require_flag(repo, "--repo", "claim").map_err(map_err)?,
                agent: require_flag(agent, "--agent", "claim").map_err(map_err)?,
                branch: require_flag(branch, "--branch", "claim").map_err(map_err)?,
                worktree,
                sparse,
                files,
                symbols,
            }
        }
        "release" => {
            let mut repo = None;
            let mut agent = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--agent" => agent = Some(take_val(rest, &mut i, "--agent").map_err(map_err)?),
                    other => return Err((json_mode, format!("unknown flag for release: {other}"))),
                }
                i += 1;
            }
            Subcommand::Release {
                repo: require_flag(repo, "--repo", "release").map_err(map_err)?,
                agent: require_flag(agent, "--agent", "release").map_err(map_err)?,
            }
        }
        "msg-send" => {
            let mut repo = None;
            let mut from = None;
            let mut to = None;
            let mut kind = None;
            let mut body = None;
            let mut branch = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--from" => from = Some(take_val(rest, &mut i, "--from").map_err(map_err)?),
                    "--to" => to = Some(take_val(rest, &mut i, "--to").map_err(map_err)?),
                    "--kind" => kind = Some(take_val(rest, &mut i, "--kind").map_err(map_err)?),
                    "--body" => body = Some(take_val(rest, &mut i, "--body").map_err(map_err)?),
                    "--branch" => {
                        branch = Some(take_val(rest, &mut i, "--branch").map_err(map_err)?)
                    }
                    other => {
                        return Err((json_mode, format!("unknown flag for msg-send: {other}")))
                    }
                }
                i += 1;
            }
            Subcommand::MsgSend {
                repo: require_flag(repo, "--repo", "msg-send").map_err(map_err)?,
                from: require_flag(from, "--from", "msg-send").map_err(map_err)?,
                to,
                kind: require_flag(kind, "--kind", "msg-send").map_err(map_err)?,
                body: require_flag(body, "--body", "msg-send").map_err(map_err)?,
                branch,
            }
        }
        "msg-inbox" => {
            let mut repo = None;
            let mut agent = None;
            let mut since_seq = 0u64;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--agent" => agent = Some(take_val(rest, &mut i, "--agent").map_err(map_err)?),
                    "--since-seq" => {
                        let raw = take_val(rest, &mut i, "--since-seq").map_err(map_err)?;
                        since_seq = raw.parse::<u64>().map_err(|_| {
                            (json_mode, format!("invalid u64 for --since-seq: {raw:?}"))
                        })?;
                    }
                    other => {
                        return Err((json_mode, format!("unknown flag for msg-inbox: {other}")))
                    }
                }
                i += 1;
            }
            Subcommand::MsgInbox {
                repo: require_flag(repo, "--repo", "msg-inbox").map_err(map_err)?,
                agent,
                since_seq,
            }
        }
        "radar" => {
            let mut repo = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    other => return Err((json_mode, format!("unknown flag for radar: {other}"))),
                }
                i += 1;
            }
            Subcommand::Radar {
                repo: require_flag(repo, "--repo", "radar").map_err(map_err)?,
            }
        }
        "verify" => {
            let mut repo = None;
            let mut branch = None;
            let mut target = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--branch" => {
                        branch = Some(take_val(rest, &mut i, "--branch").map_err(map_err)?)
                    }
                    "--target" => {
                        target = Some(take_val(rest, &mut i, "--target").map_err(map_err)?)
                    }
                    other => return Err((json_mode, format!("unknown flag for verify: {other}"))),
                }
                i += 1;
            }
            Subcommand::Verify {
                repo: require_flag(repo, "--repo", "verify").map_err(map_err)?,
                branch: require_flag(branch, "--branch", "verify").map_err(map_err)?,
                target,
            }
        }
        "merge" => {
            let mut repo = None;
            let mut source = None;
            let mut target = None;
            let mut squash = true;
            let mut message = None;
            let mut remove_wt = true;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--source" => {
                        source = Some(take_val(rest, &mut i, "--source").map_err(map_err)?)
                    }
                    "--target" => {
                        target = Some(take_val(rest, &mut i, "--target").map_err(map_err)?)
                    }
                    "--no-squash" => squash = false,
                    "--message" => {
                        message = Some(take_val(rest, &mut i, "--message").map_err(map_err)?)
                    }
                    "--keep-worktree" => remove_wt = false,
                    other => return Err((json_mode, format!("unknown flag for merge: {other}"))),
                }
                i += 1;
            }
            Subcommand::Merge {
                repo: require_flag(repo, "--repo", "merge").map_err(map_err)?,
                source: require_flag(source, "--source", "merge").map_err(map_err)?,
                target,
                squash,
                message,
                remove_wt,
            }
        }
        "prune" => {
            let mut repo = None;
            let mut branch = None;
            let mut dry_run = false;
            let mut force = false;
            let mut reap = true;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--branch" => {
                        branch = Some(take_val(rest, &mut i, "--branch").map_err(map_err)?)
                    }
                    "--dry-run" => dry_run = true,
                    "--force" => force = true,
                    "--no-reap" => reap = false,
                    other => return Err((json_mode, format!("unknown flag for prune: {other}"))),
                }
                i += 1;
            }
            Subcommand::Prune {
                repo: require_flag(repo, "--repo", "prune").map_err(map_err)?,
                branch,
                dry_run,
                force,
                reap,
            }
        }
        "build" => {
            let mut repo = None;
            let mut worktree = None;
            let mut rustc_wrapper = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--worktree" => {
                        worktree = Some(take_val(rest, &mut i, "--worktree").map_err(map_err)?)
                    }
                    "--rustc-wrapper" => {
                        rustc_wrapper =
                            Some(take_val(rest, &mut i, "--rustc-wrapper").map_err(map_err)?)
                    }
                    other => return Err((json_mode, format!("unknown flag for build: {other}"))),
                }
                i += 1;
            }
            let cargo_args = match passthrough {
                Some([]) => {
                    return Err((
                        json_mode,
                        "build requires at least one cargo argument after `--`".to_string(),
                    ))
                }
                Some(args) => args.to_vec(),
                None => vec!["build".to_string()],
            };
            Subcommand::Build {
                repo: require_flag(repo, "--repo", "build").map_err(map_err)?,
                worktree,
                rustc_wrapper,
                cargo_args,
            }
        }
        "cache-stats" => {
            let mut repo = None;
            let mut prune = false;
            let mut max_bytes = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--repo" => repo = Some(take_val(rest, &mut i, "--repo").map_err(map_err)?),
                    "--prune" => prune = true,
                    "--max-bytes" => {
                        let raw = take_val(rest, &mut i, "--max-bytes").map_err(map_err)?;
                        let parsed = raw.parse::<u64>().map_err(|_| {
                            (json_mode, format!("invalid u64 for --max-bytes: {raw:?}"))
                        })?;
                        max_bytes = Some(parsed);
                    }
                    other => {
                        return Err((json_mode, format!("unknown flag for cache-stats: {other}")))
                    }
                }
                i += 1;
            }
            Subcommand::CacheStats {
                repo: require_flag(repo, "--repo", "cache-stats").map_err(map_err)?,
                prune,
                max_bytes,
            }
        }
        other => {
            return Err((
                json_mode,
                format!("unknown subcommand {other:?} (run `gitpulse-worktree --help` for usage)"),
            ))
        }
    };

    Ok(Invocation::Command {
        json: json_mode,
        cmd,
    })
}

fn execute_subcommand(cmd: Subcommand) -> Result<Value, String> {
    match cmd {
        Subcommand::Create {
            repo,
            branch,
            base,
            target,
            sparse,
            zero_copy,
            devmap_mode,
            share_cache,
            exec,
            agent,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let target_buf = resolve_worktree_path_for_branch(&repo, &branch, target.as_deref())
                .map_err(CommandError::into_message)?;
            let target_str = target_buf
                .to_str()
                .ok_or_else(|| "target worktree path is not valid UTF-8".to_string())?;
            let argv = add_worktree_argv(target_str, Some(&branch), base.as_deref(), false);
            let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            gitpulse_lib::harness::guard_command(&repo, &argv_refs)?;
            let effective_mode = CliDevmapMode::resolve(zero_copy, devmap_mode);

            let (worktree_path, devmap_seeded, zero_copy_devmap, sparse_cone) = if !sparse
                .is_empty()
            {
                // Every git write the sparse path runs after `worktree add`
                // passes the same policy gate as the add itself.
                let mut gate = |args: &[&str]| -> Result<(), CommandError> {
                    let argv: Vec<&str> =
                        std::iter::once("git").chain(args.iter().copied()).collect();
                    gitpulse_lib::harness::guard_command(&repo, &argv)
                        .map(|_| ())
                        .map_err(CommandError::plain)
                };
                let created = add_sparse_worktree_zero_copy(
                    &repo,
                    target_str,
                    &branch,
                    base.as_deref(),
                    &sparse,
                    share_cache,
                    &mut gate,
                )
                .map_err(CommandError::into_message)?;
                (
                    created.path,
                    created.zero_copy_devmap,
                    created.zero_copy_devmap,
                    Some(created.materialized_cone_dirs),
                )
            } else if effective_mode == CliDevmapMode::Stacked {
                let path = add_worktree(&repo, target_str, Some(&branch), base.as_deref(), false)
                    .map_err(CommandError::into_message)?;
                let mut zc_seeded = false;
                if let Ok(family) = resolve_worktree_family(&repo, &path) {
                    let selection = CacheSelection::default();
                    let (_, dev_res) = std::thread::scope(|scope| {
                        let c = scope.spawn(|| {
                            if share_cache {
                                let _ = clone_ignored_caches(
                                    &family.anchor,
                                    &family.worktree,
                                    &selection,
                                );
                            }
                        });
                        let d = scope.spawn(|| {
                            provision_worktree_devmap_zero_copy(&family.anchor, &family.worktree)
                        });
                        let _ = c.join();
                        ((), d.join().ok().and_then(Result::ok))
                    });
                    if let Some(p) = dev_res {
                        zc_seeded = p.seeded && p.zero_copy_base_ref;
                    }
                }
                (path, zc_seeded, zc_seeded, None)
            } else {
                // The store is seeded whether or not build caches are shared:
                // a worktree without one is cold-built by DevMap's own hook
                // (`add_worktree_extended` seeds none, so `--share-cache`
                // used to leave every such worktree unseeded).
                let selection = CacheSelection::default();
                let engine_mode = effective_mode.to_engine_mode();
                let created = add_worktree_with_devmap(
                    &repo,
                    target_str,
                    Some(&branch),
                    base.as_deref(),
                    false,
                    share_cache.then_some(&selection),
                    true,
                    engine_mode,
                )
                .map_err(CommandError::into_message)?;
                let seeded = matches!(created.devmap, DevmapOutcome::Provisioned { .. });
                if seeded && effective_mode == CliDevmapMode::Copy {
                    // This process exits when the command returns, and a
                    // background build would die with it.
                    gitpulse_lib::devmap::live::refresh_seeded_worktree(
                        &created.path,
                        gitpulse_lib::devmap::live::SeededRefresh::Inline,
                    );
                }
                (created.path, seeded, false, None)
            };

            let lane_claim = if let Some(ref agent_id) = agent {
                let cone_vec = sparse_cone.clone().unwrap_or_default();
                Some(
                    register_or_update_lane(
                        &repo,
                        agent_id,
                        &branch,
                        Some(&worktree_path),
                        &cone_vec,
                        &sparse,
                        &[],
                    )
                    .map_err(CommandError::into_message)?,
                )
            } else {
                None
            };

            let mut cmd_out = None;
            if let Some(cmd_str) = exec {
                #[cfg(unix)]
                let (prog, prog_args): (&str, [&str; 2]) = ("sh", ["-c", cmd_str.as_str()]);
                #[cfg(windows)]
                let (prog, prog_args): (&str, [&str; 2]) = ("cmd", ["/C", cmd_str.as_str()]);
                let mut guard_argv = vec![prog];
                guard_argv.extend_from_slice(&prog_args);
                gitpulse_lib::harness::guard_command(&worktree_path, &guard_argv)?;
                let output = capture_command(
                    prog,
                    &prog_args,
                    Some(Path::new(&worktree_path)),
                    Duration::from_secs(60),
                    &[],
                )
                .map_err(CommandError::into_message)?;
                let mut combined = output.stdout_text();
                if !output.success {
                    let stderr = output.stderr_text();
                    if !stderr.is_empty() {
                        if !combined.is_empty() && !combined.ends_with('\n') {
                            combined.push('\n');
                        }
                        combined.push_str(&stderr);
                    }
                    return Err(format!(
                        "exec command exited with status {}: {}",
                        output.status_code,
                        combined.trim()
                    ));
                }
                cmd_out = Some(combined);
            }

            Ok(json!({
                "ok": true,
                "branch": branch,
                "worktree_path": worktree_path,
                "cache_shared": share_cache,
                "devmap_seeded": devmap_seeded,
                "zero_copy_devmap": zero_copy_devmap,
                "sparse_cone": sparse_cone,
                "lane_claim": lane_claim,
                "command_output": cmd_out,
            }))
        }
        Subcommand::Switch {
            repo,
            branch,
            create,
            base,
            zero_copy,
            devmap_mode,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let worktrees = list_worktrees(&repo).map_err(CommandError::into_message)?;
            if let Some(wt) = worktrees
                .iter()
                .find(|w| w.branch.as_deref() == Some(branch.as_str()))
            {
                let wt_path = Path::new(&wt.path);
                let devmap_seeded = devmap_query::paths::store_path(wt_path).is_file()
                    || zero_copy_base_ref_path(&wt.path).is_file()
                    || devmap_query::paths::stack_lease_path(wt_path).is_file();
                return Ok(json!({
                    "ok": true,
                    "branch": branch,
                    "worktree_path": wt.path,
                    "created": false,
                    "devmap_seeded": devmap_seeded,
                }));
            }
            if create {
                let target_buf = resolve_worktree_path_for_branch(&repo, &branch, None)
                    .map_err(CommandError::into_message)?;
                let target_str = target_buf
                    .to_str()
                    .ok_or_else(|| "invalid target path".to_string())?;
                let argv = add_worktree_argv(target_str, Some(&branch), base.as_deref(), false);
                let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                gitpulse_lib::harness::guard_command(&repo, &argv_refs)?;
                let selection = CacheSelection::default();
                let effective_mode = CliDevmapMode::resolve(zero_copy, devmap_mode);
                let created = add_worktree_with_devmap(
                    &repo,
                    target_str,
                    Some(&branch),
                    base.as_deref(),
                    false,
                    Some(&selection),
                    true,
                    effective_mode.to_engine_mode(),
                )
                .map_err(CommandError::into_message)?;
                let devmap_seeded = matches!(created.devmap, DevmapOutcome::Provisioned { .. });
                if devmap_seeded && effective_mode == CliDevmapMode::Copy {
                    gitpulse_lib::devmap::live::refresh_seeded_worktree(
                        &created.path,
                        gitpulse_lib::devmap::live::SeededRefresh::Inline,
                    );
                }
                return Ok(json!({
                    "ok": true,
                    "branch": branch,
                    "worktree_path": created.path,
                    "created": true,
                    "devmap_seeded": devmap_seeded,
                }));
            }
            Err(format!("worktree for branch {branch:?} does not exist"))
        }
        Subcommand::Isolate {
            repo,
            worktree,
            zero_copy,
            devmap_mode,
            include_venv,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let family =
                resolve_worktree_family(&repo, &worktree).map_err(CommandError::into_message)?;
            gitpulse_lib::harness::guard_command(&repo, &["git", "worktree", "list"])?;
            let selection = CacheSelection {
                include_virtualenvs: include_venv,
                ..Default::default()
            };
            let effective_mode = CliDevmapMode::resolve(zero_copy, devmap_mode);
            let anchor = &family.anchor;
            let target = &family.worktree;
            let (cache_res, devmap_res) = std::thread::scope(|scope| {
                let c = scope.spawn(|| clone_ignored_caches(anchor, target, &selection));
                let d = scope.spawn(|| match effective_mode {
                    CliDevmapMode::Stacked => {
                        provision_worktree_devmap_zero_copy(anchor, target).map(Some)
                    }
                    CliDevmapMode::Copy => provision_worktree_devmap(anchor, target).map(Some),
                    CliDevmapMode::None => Ok(None),
                });
                let cr = c
                    .join()
                    .unwrap_or_else(|_| Err(CommandError::plain("cache clone worker panicked")));
                let dr = d.join().unwrap_or_else(|_| {
                    Err(CommandError::plain("devmap provision worker panicked"))
                });
                (cr, dr)
            });
            let report = cache_res.map_err(CommandError::into_message)?;
            let caches_cloned: Vec<String> = report
                .copied
                .iter()
                .filter_map(|r| {
                    let dst = Path::new(&r.destination);
                    dst.strip_prefix(target)
                        .ok()
                        .and_then(|rel| rel.to_str())
                        .or_else(|| dst.file_name().and_then(|n| n.to_str()))
                        .map(str::to_owned)
                })
                .collect();
            let (devmap_seeded, devmap_mode_label) = match devmap_res {
                Ok(None) => (false, "none".to_string()),
                Ok(Some(p)) => {
                    let mode = if !p.seeded {
                        p.reason.unwrap_or_else(|| "not_seeded".to_string())
                    } else if p.zero_copy_base_ref {
                        "zero_copy_base_ref".to_string()
                    } else if p.used_online_backup {
                        "online_backup".to_string()
                    } else if p.is_cow {
                        "cow_clone".to_string()
                    } else {
                        "byte_copy".to_string()
                    };
                    (p.seeded, mode)
                }
                Err(e) => (false, format!("failed: {}", e.message())),
            };
            if devmap_seeded && effective_mode == CliDevmapMode::Copy {
                gitpulse_lib::devmap::live::refresh_seeded_worktree(
                    &target.to_string_lossy(),
                    gitpulse_lib::devmap::live::SeededRefresh::Inline,
                );
            }
            let overlay_dirty = if effective_mode == CliDevmapMode::None {
                0
            } else {
                target
                    .to_str()
                    .and_then(|s| sync_worktree_overlay(s).ok())
                    .map(|r| r.modified_files + r.tombstoned_files)
                    .unwrap_or(0)
            };
            Ok(json!({
                "ok": true,
                "worktree_path": worktree,
                "caches_cloned": caches_cloned,
                "cache_error": report.error(),
                "devmap_seeded": devmap_seeded,
                "devmap_mode": devmap_mode_label,
                "overlay_dirty_files": overlay_dirty,
            }))
        }
        Subcommand::CompileOverlay { worktree } => {
            gitpulse_lib::repository_trust::require(Path::new(&worktree))
                .map_err(CommandError::into_message)?;
            let report = sync_worktree_overlay(&worktree).map_err(CommandError::into_message)?;
            let zero_copy_ref = load_zero_copy_base_ref(&worktree).ok().flatten();
            Ok(json!({
                "ok": true,
                "worktree_path": worktree,
                "zero_copy_base_ref": zero_copy_ref,
                "report": report,
            }))
        }
        Subcommand::ExpandCone { repo, paths } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let family =
                resolve_worktree_family(&repo, &repo).map_err(CommandError::into_message)?;
            let cone_dirs = expand_sparse_cone_with_devmap(&family.anchor, &paths)
                .map_err(CommandError::into_message)?;
            Ok(json!({
                "ok": true,
                "anchor": family.anchor,
                "requested_paths": paths,
                "materialized_cone_dirs": cone_dirs,
            }))
        }
        Subcommand::Status { repo } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let worktrees = list_worktrees(&repo).map_err(CommandError::into_message)?;
            let lanes = list_lanes(&repo).map_err(CommandError::into_message)?;
            let mut wt_rows = Vec::with_capacity(worktrees.len());
            for wt in &worktrees {
                let wt_path = Path::new(&wt.path);
                let has_local_store = devmap_query::paths::store_path(wt_path).is_file();
                let zc_ref = load_zero_copy_base_ref(&wt.path).ok().flatten();
                let has_stack_lease = devmap_query::paths::stack_lease_path(wt_path).is_file();
                let devmap_mode = if zc_ref.is_some() || has_stack_lease {
                    "zero_copy_base_ref"
                } else if has_local_store {
                    "local_store"
                } else {
                    "none"
                };
                let overlay_summary = load_worktree_overlay(&wt.path).ok().flatten().map(|ov| {
                    json!({
                        "generation_id": ov.generation,
                        "modified_files": ov.modified_files.len(),
                        "tombstoned_files": ov.tombstoned_files.len(),
                        "dirty_csr_nodes": ov.dirty_nodes.len(),
                    })
                });
                let user_dirty = if wt.is_bare {
                    None
                } else {
                    user_changed_paths(&wt.path)
                        .ok()
                        .map(|(paths, _)| paths.len())
                };
                wt_rows.push(json!({
                    "path": wt.path,
                    "branch": wt.branch,
                    "head": wt.head,
                    "is_main": wt.is_main,
                    "devmap_mode": devmap_mode,
                    "zero_copy_anchor": zc_ref.map(|r| r.anchor_repo_root),
                    "overlay": overlay_summary,
                    "user_dirty_files": user_dirty,
                }));
            }
            Ok(json!({
                "ok": true,
                "worktrees": wt_rows,
                "active_lanes": lanes,
            }))
        }
        Subcommand::Claim {
            repo,
            agent,
            branch,
            worktree,
            sparse,
            files,
            symbols,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let (claim, claim_conflicts) = register_or_update_lane_with_conflicts(
                &repo,
                &agent,
                &branch,
                worktree.as_deref(),
                &sparse,
                &files,
                &symbols,
            )
            .map_err(CommandError::into_message)?;
            Ok(json!({
                "ok": true,
                "claim": claim,
                "claim_conflicts": claim_conflicts,
            }))
        }
        Subcommand::Release { repo, agent } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let released = release_lane(&repo, &agent).map_err(CommandError::into_message)?;
            Ok(json!({
                "ok": true,
                "agent_id": agent,
                "released": released,
            }))
        }
        Subcommand::MsgSend {
            repo,
            from,
            to,
            kind,
            body,
            branch,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let message =
                post_message(&repo, &from, to.as_deref(), &kind, &body, branch.as_deref())
                    .map_err(CommandError::into_message)?;
            Ok(json!({
                "ok": true,
                "message": message,
            }))
        }
        Subcommand::MsgInbox {
            repo,
            agent,
            since_seq,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let inbox = read_messages(&repo, agent.as_deref(), since_seq)
                .map_err(CommandError::into_message)?;
            Ok(json!({
                "ok": true,
                "inbox": inbox,
            }))
        }
        Subcommand::Radar { repo } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let radar = scan_parallel_agent_radar(&repo).map_err(CommandError::into_message)?;
            Ok(json!({
                "ok": true,
                "radar": radar,
            }))
        }
        Subcommand::Verify {
            repo,
            branch,
            target,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let family =
                resolve_worktree_family(&repo, &repo).map_err(CommandError::into_message)?;
            let target_branch = match target {
                Some(t) => t,
                None => compute_default_base(&family.anchor)
                    .checked()
                    .ok()
                    .and_then(|d| d.base.map(|(n, _)| n))
                    .unwrap_or_else(|| "main".to_string()),
            };
            let result = verify_branch_integration(&family.anchor, &branch, &target_branch)
                .map_err(CommandError::into_message)?;
            Ok(json!({
                "ok": true,
                "verification": result,
            }))
        }
        Subcommand::Merge {
            repo,
            source,
            target,
            squash,
            message,
            remove_wt,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let res = merge_worktree_pipeline(
                &repo,
                &source,
                target.as_deref(),
                squash,
                message.as_deref(),
                remove_wt,
            )
            .map_err(CommandError::into_message)?;
            Ok(serde_json::to_value(res).map_err(|e| e.to_string())?)
        }
        Subcommand::Prune {
            repo,
            branch,
            dry_run,
            force,
            reap,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let family =
                resolve_worktree_family(&repo, &repo).map_err(CommandError::into_message)?;
            let anchor = family.anchor;
            let default_base = compute_default_base(&anchor)
                .checked()
                .ok()
                .and_then(|d| d.base.map(|(n, _)| n))
                .unwrap_or_else(|| "main".to_string());
            let worktrees = list_worktrees(&repo).map_err(CommandError::into_message)?;
            let mut pruned = Vec::new();
            let mut reaped_pids = Vec::new();

            for wt in worktrees {
                if wt.is_main {
                    continue;
                }
                if let Some(ref only_branch) = branch {
                    if wt.branch.as_deref() != Some(only_branch.as_str()) {
                        continue;
                    }
                }
                let mut prunable = force;
                if !prunable {
                    let is_clean =
                        user_changed_paths(&wt.path).is_ok_and(|(dirty, _)| dirty.is_empty());
                    if is_clean {
                        if let Some(br) = wt.branch.as_deref() {
                            if let Ok(verif) = verify_branch_integration(&anchor, br, &default_base)
                            {
                                prunable = verif.is_integrated;
                            }
                        }
                    }
                }
                if prunable {
                    let remove_argv = remove_worktree_argv(&wt.path, force);
                    let remove_refs: Vec<&str> = remove_argv.iter().map(String::as_str).collect();
                    gitpulse_lib::harness::guard_command(&repo, &remove_refs)?;
                    if reap {
                        let r = reap_processes_in_dir(Path::new(&wt.path), dry_run)
                            .map_err(CommandError::into_message)?;
                        reaped_pids.extend(r.pids_terminated);
                    }
                    if dry_run {
                        pruned.push(wt.path);
                    } else {
                        remove_worktree(&repo, &wt.path, force)
                            .map_err(CommandError::into_message)?;
                        pruned.push(wt.path);
                    }
                }
            }
            if !dry_run {
                let _ = prune_worktree(&repo);
            }
            Ok(json!({
                "ok": true,
                "dry_run": dry_run,
                "pruned_worktrees": pruned,
                "reaped_pids": reaped_pids,
            }))
        }
        Subcommand::Build {
            repo,
            worktree,
            rustc_wrapper,
            cargo_args,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let wt_lookup = worktree.as_deref().unwrap_or(&repo);
            let family =
                resolve_worktree_family(&repo, wt_lookup).map_err(CommandError::into_message)?;
            let wt_str = family
                .worktree
                .to_str()
                .ok_or_else(|| "worktree path is not valid UTF-8".to_string())?;

            let mut guard_argv = Vec::with_capacity(cargo_args.len() + 1);
            guard_argv.push("cargo");
            for arg in &cargo_args {
                guard_argv.push(arg.as_str());
            }
            gitpulse_lib::harness::guard_command(wt_str, &guard_argv)?;

            let cache_dir = resolve_build_cache_dir(&family.worktree)?;
            let cache_dir_str = cache_dir
                .to_str()
                .ok_or_else(|| "build cache path is not valid UTF-8".to_string())?;

            let wrapper_path = match rustc_wrapper {
                Some(explicit) => PathBuf::from(explicit),
                None => resolve_gitpulse_rustc_bin()?,
            };
            if !wrapper_path.is_file() {
                return Err(format!(
                    "gitpulse-rustc wrapper does not exist at {}",
                    wrapper_path.display()
                ));
            }
            let wrapper_str = wrapper_path
                .to_str()
                .ok_or_else(|| "rustc wrapper path is not valid UTF-8".to_string())?;

            let before = inspect_or_prune_cache(&cache_dir, false, None)?;

            let cargo_arg_refs: Vec<&str> = cargo_args.iter().map(String::as_str).collect();
            let mut env_pairs: Vec<(&str, String)> = vec![
                ("RUSTC_WRAPPER", wrapper_str.to_string()),
                (ENV_BUILD_CACHE_DIR, cache_dir_str.to_string()),
                (ENV_WORKTREE_ROOT, wt_str.to_string()),
                ("CARGO_INCREMENTAL", "0".to_string()),
            ];
            for inherited_key in [
                "RUSTUP_HOME",
                "CARGO_HOME",
                "RUSTUP_TOOLCHAIN",
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_JOBS",
                "CARGO_TERM_COLOR",
                "CARGO_NET_OFFLINE",
                "RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
                "CC",
                "CXX",
                "AR",
                "SDKROOT",
                "DEVELOPER_DIR",
                "MACOSX_DEPLOYMENT_TARGET",
                "PKG_CONFIG_PATH",
                "LIBCLANG_PATH",
            ] {
                if let Ok(val) = std::env::var(inherited_key) {
                    if !val.trim().is_empty() {
                        env_pairs.push((inherited_key, val));
                    }
                }
            }
            let extra_env: Vec<(&str, &str)> =
                env_pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();

            let output = capture_command(
                "cargo",
                &cargo_arg_refs,
                Some(&family.worktree),
                Duration::from_secs(900),
                &extra_env,
            )
            .map_err(CommandError::into_message)?;

            let after = inspect_or_prune_cache(&cache_dir, false, None)?;
            let stdout = output.stdout_text();
            let stderr = output.stderr_text();
            if !output.success {
                let mut combined = stdout;
                if !stderr.is_empty() {
                    if !combined.is_empty() && !combined.ends_with('\n') {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr);
                }
                return Err(format!(
                    "cargo {:?} exited with status {}: {}",
                    cargo_args,
                    output.status_code,
                    combined.trim()
                ));
            }

            Ok(json!({
                "ok": true,
                "worktree_path": wt_str,
                "cache_dir": cache_dir_str,
                "rustc_wrapper": wrapper_str,
                "cargo_args": cargo_args,
                "delta_hits": after.counters.hits.saturating_sub(before.counters.hits),
                "delta_zero_copy_hits": after.counters.zero_copy_hits.saturating_sub(before.counters.zero_copy_hits),
                "delta_misses": after.counters.misses.saturating_sub(before.counters.misses),
                "delta_stage2_invalidations": after.counters.stage2_invalidations.saturating_sub(before.counters.stage2_invalidations),
                "delta_bytes_saved": after.counters.bytes_saved.saturating_sub(before.counters.bytes_saved),
                "cache_counters": after.counters,
                "blob_count": after.blob_count,
                "total_blob_bytes": after.total_blob_bytes,
                "stdout": stdout,
                "stderr": stderr,
            }))
        }
        Subcommand::CacheStats {
            repo,
            prune,
            max_bytes,
        } => {
            gitpulse_lib::repository_trust::require(Path::new(&repo))
                .map_err(CommandError::into_message)?;
            let family =
                resolve_worktree_family(&repo, &repo).map_err(CommandError::into_message)?;
            let cache_dir = resolve_build_cache_dir(&family.worktree)?;
            let report = inspect_or_prune_cache(&cache_dir, prune, max_bytes)?;
            Ok(json!({
                "ok": true,
                "report": report,
            }))
        }
    }
}

fn emit_value(json_mode: bool, value: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let encoded = if json_mode {
        serde_json::to_string(value).unwrap_or_else(|_| "{\"ok\":false}".to_string())
    } else {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{\"ok\":false}".to_string())
    };
    out.write_all(encoded.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

fn emit_error(json_mode: bool, error: &str) {
    if json_mode {
        let payload = json!({
            "ok": false,
            "error": error,
        });
        let _ = emit_value(true, &payload);
    } else {
        let mut err = io::stderr().lock();
        let _ = writeln!(err, "gitpulse-worktree: {error}");
        let _ = err.flush();
    }
}

fn main() {
    gitpulse_lib::engine::git_cli::run_process_as_background();
    gitpulse_lib::engine::git_cli::run_process_awaiting_background_budget();
    gitpulse_lib::engine::git_cli::run_process_with_shared_spawn_budget();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (json_mode, cmd) = match parse_cli(&argv) {
        Ok(Invocation::Help) => {
            let mut out = io::stdout().lock();
            let ok = out
                .write_all(USAGE.as_bytes())
                .and_then(|()| out.flush())
                .is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        Ok(Invocation::Version) => {
            let mut out = io::stdout().lock();
            let line = format!("gitpulse-worktree {VERSION}\n");
            let ok = out
                .write_all(line.as_bytes())
                .and_then(|()| out.flush())
                .is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        Ok(Invocation::Command { json, cmd }) => (json, cmd),
        Err((json_err, msg)) => {
            emit_error(json_err, &msg);
            std::process::exit(2);
        }
    };

    gitpulse_lib::logging::init();
    gitpulse_lib::logging::install_panic_hook();
    log::info!(
        target: "setup",
        "{}",
        gitpulse_lib::limits::raise_open_file_limit().describe()
    );
    let signals = gitpulse_lib::procguard::install_signal_handlers();
    if signals.is_armed() {
        log::info!(target: "setup", "{}", signals.describe());
    } else {
        log::warn!(target: "setup", "{}", signals.describe());
    }

    let exit_code = match execute_subcommand(cmd) {
        Ok(val) => {
            if emit_value(json_mode, &val).is_ok() {
                0
            } else {
                1
            }
        }
        Err(reason) => {
            emit_error(json_mode, &reason);
            1
        }
    };

    let sweep = gitpulse_lib::procguard::reap_all(gitpulse_lib::procguard::CHILD_GRACE);
    if sweep.is_complete() {
        log::info!(target: "shutdown", "{}", sweep.describe());
    } else {
        log::warn!(target: "shutdown", "{}", sweep.describe());
    }
    gitpulse_lib::procguard::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_str(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parse_cli_handles_help_version_and_sparse_zero_copy_create() {
        assert_eq!(parse_cli(&vec_str(&["--help"])), Ok(Invocation::Help));
        assert_eq!(parse_cli(&vec_str(&["create", "-h"])), Ok(Invocation::Help));
        assert_eq!(parse_cli(&vec_str(&["--version"])), Ok(Invocation::Version));

        let parsed = parse_cli(&vec_str(&[
            "create",
            "--repo",
            "/tmp/repo",
            "--branch",
            "agent/lane-1",
            "--sparse",
            "packages/api, packages/core",
            "--zero-copy",
            "--agent",
            "claude-1",
            "--json",
        ]))
        .expect("valid create args");

        assert_eq!(
            parsed,
            Invocation::Command {
                json: true,
                cmd: Subcommand::Create {
                    repo: "/tmp/repo".to_string(),
                    branch: "agent/lane-1".to_string(),
                    base: None,
                    target: None,
                    sparse: vec!["packages/api".to_string(), "packages/core".to_string()],
                    zero_copy: true,
                    devmap_mode: None,
                    share_cache: true,
                    exec: None,
                    agent: Some("claude-1".to_string()),
                }
            }
        );
    }

    #[test]
    fn parse_cli_accepts_devmap_mode_across_create_switch_and_isolate() {
        let create_stacked = parse_cli(&vec_str(&[
            "create",
            "--repo",
            "/tmp/repo",
            "--branch",
            "agent/stacked",
            "--devmap-mode",
            "stacked",
        ]))
        .expect("create --devmap-mode stacked");
        assert_eq!(
            create_stacked,
            Invocation::Command {
                json: false,
                cmd: Subcommand::Create {
                    repo: "/tmp/repo".to_string(),
                    branch: "agent/stacked".to_string(),
                    base: None,
                    target: None,
                    sparse: Vec::new(),
                    zero_copy: false,
                    devmap_mode: Some(CliDevmapMode::Stacked),
                    share_cache: true,
                    exec: None,
                    agent: None,
                }
            }
        );

        let switch_none = parse_cli(&vec_str(&[
            "switch",
            "--repo",
            "/tmp/repo",
            "--branch",
            "agent/none",
            "--create",
            "--devmap-mode",
            "none",
        ]))
        .expect("switch --devmap-mode none");
        assert_eq!(
            switch_none,
            Invocation::Command {
                json: false,
                cmd: Subcommand::Switch {
                    repo: "/tmp/repo".to_string(),
                    branch: "agent/none".to_string(),
                    create: true,
                    base: None,
                    zero_copy: false,
                    devmap_mode: Some(CliDevmapMode::None),
                }
            }
        );

        let isolate_copy = parse_cli(&vec_str(&[
            "isolate",
            "--repo",
            "/tmp/repo",
            "--worktree",
            "/tmp/wt",
            "--devmap-mode",
            "copy",
        ]))
        .expect("isolate --devmap-mode copy");
        assert_eq!(
            isolate_copy,
            Invocation::Command {
                json: false,
                cmd: Subcommand::Isolate {
                    repo: "/tmp/repo".to_string(),
                    worktree: "/tmp/wt".to_string(),
                    zero_copy: false,
                    devmap_mode: Some(CliDevmapMode::Copy),
                    include_venv: false,
                }
            }
        );

        let bad_mode = parse_cli(&vec_str(&[
            "create",
            "--repo",
            "/tmp/repo",
            "--branch",
            "x",
            "--devmap-mode",
            "bogus",
        ]))
        .expect_err("invalid --devmap-mode must error");
        assert!(bad_mode.1.contains("invalid --devmap-mode"));
    }

    #[test]
    fn parse_cli_rejects_missing_required_flags_empty_lists_and_bad_numbers() {
        let err_empty = parse_cli(&[]).expect_err("empty argv must fail");
        assert!(!err_empty.0);
        assert!(err_empty.1.contains("missing subcommand"));

        let err_missing_branch = parse_cli(&vec_str(&["--json", "create", "--repo", "/tmp/repo"]))
            .expect_err("missing --branch must fail");
        assert!(err_missing_branch.0);
        assert!(err_missing_branch.1.contains("--branch"));

        let err_empty_csv = parse_cli(&vec_str(&[
            "expand-cone",
            "--repo",
            "/tmp/repo",
            "--paths",
            " , , ",
        ]))
        .expect_err("blank csv must fail");
        assert!(err_empty_csv.1.contains("must not be empty"));

        let err_bad_seq = parse_cli(&vec_str(&[
            "msg-inbox",
            "--repo",
            "/tmp/repo",
            "--since-seq",
            "not-a-number",
        ]))
        .expect_err("invalid u64 must fail");
        assert!(err_bad_seq.1.contains("--since-seq"));

        let err_unknown = parse_cli(&vec_str(&["radar", "--repo", "/tmp/r", "--bogus"]))
            .expect_err("unknown flag must fail");
        assert!(err_unknown.1.contains("unknown flag"));
    }

    #[test]
    fn parse_cli_handles_build_passthrough_and_cache_stats() {
        let default_build = parse_cli(&vec_str(&["build", "--repo", "/tmp/repo", "--json"]))
            .expect("default build");
        assert_eq!(
            default_build,
            Invocation::Command {
                json: true,
                cmd: Subcommand::Build {
                    repo: "/tmp/repo".to_string(),
                    worktree: None,
                    rustc_wrapper: None,
                    cargo_args: vec!["build".to_string()],
                }
            }
        );

        let custom_build = parse_cli(&vec_str(&[
            "build",
            "--repo",
            "/tmp/repo",
            "--worktree",
            "/tmp/wt-b",
            "--json",
            "--",
            "check",
            "--offline",
            "--message-format=json",
        ]))
        .expect("custom build passthrough");
        assert_eq!(
            custom_build,
            Invocation::Command {
                json: true,
                cmd: Subcommand::Build {
                    repo: "/tmp/repo".to_string(),
                    worktree: Some("/tmp/wt-b".to_string()),
                    rustc_wrapper: None,
                    cargo_args: vec![
                        "check".to_string(),
                        "--offline".to_string(),
                        "--message-format=json".to_string(),
                    ],
                }
            }
        );

        let err_empty_pass = parse_cli(&vec_str(&["build", "--repo", "/tmp/repo", "--"]))
            .expect_err("empty passthrough must fail");
        assert!(err_empty_pass.1.contains("after `--`"));

        let err_non_build_pass = parse_cli(&vec_str(&["status", "--repo", "/tmp/repo", "--", "x"]))
            .expect_err("passthrough on status must fail");
        assert!(err_non_build_pass.1.contains("does not accept passthrough"));

        let stats = parse_cli(&vec_str(&[
            "cache-stats",
            "--repo",
            "/tmp/repo",
            "--prune",
            "--max-bytes",
            "1048576",
            "--json",
        ]))
        .expect("cache-stats");
        assert_eq!(
            stats,
            Invocation::Command {
                json: true,
                cmd: Subcommand::CacheStats {
                    repo: "/tmp/repo".to_string(),
                    prune: true,
                    max_bytes: Some(1_048_576),
                }
            }
        );
    }
}
