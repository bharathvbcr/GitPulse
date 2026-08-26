use crate::analyzer::{DiffChurn, LanguageDetector, LanguageInfo, LocCounter};
use crate::engine::git_cli::{self, git, git_text, sandbox_join_canonical, validate_repo};
use crate::engine::git_writer::validate_ref_name;
use crate::graph::lane_solver::RawCommitNode;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

const MAX_BRANCH_STAT_TARGETS: usize = 96;

/// Sidebar tag ceiling: newest-first via `--sort=-creatordate`, then cut here
/// so a 10k-tag monorepo cannot ship megabytes over IPC. TagInfo carries no
/// truncation flag (wire shape is frontend-owned); older tags beyond the cap
/// are silently omitted until one lands.
const TAG_LIST_CAP: usize = 400;

/// Commit graph ref decoration tag ceiling: newest-first, capped so massive tag histories do not bloat the graph payload.
pub const REFS_TAG_CAP: usize = 200;

/// Bound for the process-wide churn memo. Entries are content-addressed by
/// oids so they cannot go stale; the cap only bounds memory.
const CHURN_CACHE_CAPACITY: usize = 8192;

/// Hard ceiling for reading one working-tree file from disk.
///
/// `get_file_blob` loads whole files into memory (and base64-expands binaries
/// ~1.33x before they cross the IPC boundary), so an unbounded read lets a
/// multi-GB working-tree file OOM the app. Git-object reads go through
/// `git`'s output cap; this guards the direct `fs::read` path.
const MAX_WORKING_TREE_BYTES: u64 = 64 * 1024 * 1024;

/// Hard ceiling on [`GitReader::list_repo_files`]'s payload.
///
/// The file explorer ships every visible path over IPC in one `Vec<String>`;
/// without a bound, a monorepo with millions of entries would serialize an
/// unbounded allocation into the frontend on every open. Past the cap the
/// call fails loudly instead of truncating silently — a partial tree rendered
/// as complete would read as fact.
const MAX_REPO_FILES: usize = 200_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    pub is_current: bool,
    pub is_remote: bool,
    pub remote_name: Option<String>,
    pub tip_commit_id: String,
    pub ahead_count: usize,
    pub behind_count: usize,
    pub upstream: Option<String>,
    pub is_default: bool,
    pub is_gone: bool,
    pub last_commit_timestamp: i64,
    pub last_author: String,
    pub last_summary: String,
    pub commits_ahead_of_base: usize,
    pub commits_behind_base: usize,
    pub additions: usize,
    pub deletions: usize,
    pub files_changed: usize,
    pub compared_to: Option<String>,
}

/// Wire shape for one branch's churn numbers from [`GitReader::branch_stats`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchStatsUpdate {
    pub name: String,
    pub tip_commit_id: String,
    pub is_remote: bool,
    pub remote_name: Option<String>,
    pub additions: usize,
    pub deletions: usize,
    pub files_changed: usize,
    pub commits_ahead_of_base: usize,
    pub commits_behind_base: usize,
}

/// Progressive churn refresh for the branch list. `list_branches` ships fast
/// with ahead/behind from the batched for-each-ref atom and zero churn; this
/// report backfills churn per branch and is the only caller that pays for
/// subprocess-heavy shortstat walks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchStatsReport {
    pub compared_to: String,
    pub updates: Vec<BranchStatsUpdate>,
    /// Freshly computed this call (cache misses).
    pub computed: usize,
    /// Served from the oid-keyed churn cache.
    pub cached: usize,
    /// True when more unique uncached tips existed than MAX_BRANCH_STAT_TARGETS;
    /// callers re-invoke to drain the rest (the oid-keyed churn cache is the
    /// implicit cursor: already-computed tips hit the cache, so each re-call
    /// advances to the next tranche). Already-cached tips ride along so a
    /// warm refresh hydrates every eligible branch in one payload.
    pub capped: bool,
    /// Eligible branches whose churn walk FAILED this call (not capped —
    /// attempted, errored). A failed branch silently missing from `updates`
    /// must stay countable: "checked and failed" is not "not yet reached".
    pub compute_failures: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagInfo {
    pub name: String,
    pub commit_id: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStatus {
    pub path: String,
    pub old_path: Option<String>,
    pub status_code: String,
    pub is_staged: bool,
    pub is_conflicted: bool,
    pub additions: usize,
    pub deletions: usize,
    /// Why this row's additions/deletions may understate reality (its numstat
    /// record could not be parsed). Absent from the JSON entirely while empty,
    /// so existing consumers see no shape change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlameLine {
    pub line_no: usize,
    pub commit_id: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitFileChange {
    pub path: String,
    pub status_code: String,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitDetails {
    pub id: String,
    pub parent_ids: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: String,
    pub summary: String,
    pub body: String,
    pub gpg_status: String,
    pub co_authors: Vec<String>,
    pub changed_files: Vec<CommitFileChange>,
    pub total_additions: usize,
    pub total_deletions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflogEntry {
    pub index: usize,
    pub commit_id: String,
    pub selector: String,
    pub action: String,
    pub message: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoLanguageStat {
    pub language: String,
    pub color_hex: String,
    pub category: String,
    pub code_lines: usize,
    pub file_count: usize,
    pub percentage: f64,
}

/// Whole-scan budget for language statistics. Per-file caps (1 MiB reads) do
/// not bound the total: 10k files x 1 MiB is a legitimate multi-gigabyte
/// synchronous read inside one IPC call. The deadline stops the walk and the
/// report says so instead of presenting a capped sample as complete coverage.
pub const LANG_STATS_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageStatsReport {
    pub stats: Vec<RepoLanguageStat>,
    /// True when the scan stopped early: deadline hit, or fewer files counted
    /// than candidates selected.
    pub truncated: bool,
    /// Files actually read and counted.
    pub scanned_files: usize,
    /// Candidate files selected before scanning began.
    pub candidate_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileBlob {
    pub path: String,
    pub is_binary: bool,
    pub is_image: bool,
    pub mime: String,
    pub text: Option<String>,
    pub base64: Option<String>,
}

pub struct GitReader;

impl GitReader {
    /// Hard ceiling for one history walk. Callers paginate past this instead
    /// of lifting it, so no single request can ask git for an unbounded log.
    pub const MAX_HISTORY_COMMITS: usize = 100_000;

    pub fn list_branches(repo_path: &str) -> Result<Vec<BranchInfo>, String> {
        let repo = validate_repo(repo_path)?;
        // The default branch follows the primary remote's HEAD, and the
        // primary remote is not always named "origin" (upstream, gitlab,
        // company forks); guessing origin here mislabels the churn base.
        let remote = resolve_default_remote(&repo);
        let head_ref = remote_head_ref(&remote);
        let origin_head = git_text(&repo, &["symbolic-ref", "--quiet", head_ref.as_str()]).ok();
        // Resolving the default base BEFORE listing lets ahead-behind vs that
        // base ride the same for-each-ref process, so this call never blocks
        // on per-branch churn subprocesses.
        let default_base = resolve_default_base_on(&repo, &remote, origin_head.as_deref());

        let mut format = String::from(BRANCH_LIST_FORMAT);
        if let Some((_, base_oid)) = default_base.as_ref() {
            format.push_str("%00%(ahead-behind:");
            format.push_str(base_oid);
            format.push(')');
        }
        let format_arg = format!("--format={format}");
        let listed = git_text(
            &repo,
            &[
                "for-each-ref",
                format_arg.as_str(),
                "refs/heads/",
                "refs/remotes/",
            ],
        );
        // Older git (<2.42) rejects the ahead-behind atom outright: retry once
        // without it so listing still works. Ahead/behind then stay zero and
        // cmd_branch_stats fills them progressively.
        let (stdout, has_ahead_behind) = match listed {
            Ok(stdout) => (stdout, default_base.is_some()),
            Err(err) if default_base.is_some() => {
                let base_format_arg = format!("--format={BRANCH_LIST_FORMAT}");
                let retried = git_text(
                    &repo,
                    &[
                        "for-each-ref",
                        base_format_arg.as_str(),
                        "refs/heads/",
                        "refs/remotes/",
                    ],
                )
                .map_err(|retry_err| {
                    format!("{err}; retry without ahead-behind also failed: {retry_err}")
                })?;
                (retried, false)
            }
            Err(err) => return Err(err),
        };
        let default_short = default_base.map(|(name, _)| name);

        let mut branches = Vec::new();
        // One line per ref; refnames and subjects cannot contain newlines or
        // NULs, so NUL-separated fields stay aligned.
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\0').collect();
            if parts.len() < 3 {
                continue;
            }
            let is_current = parts[0].trim() == "*";
            let refname = parts[1];
            if refname.ends_with("/HEAD") {
                continue;
            }
            let tip = parts[2].to_string();
            let track = parts.get(3).copied().unwrap_or("");
            let (ahead_count, behind_count) = git_cli::parse_ahead_behind(track);

            let is_remote = refname.starts_with("refs/remotes/");
            let (name, remote_name) = if is_remote {
                let rest = refname.trim_start_matches("refs/remotes/");
                let remote = rest.split('/').next().map(String::from);
                (rest.to_string(), remote)
            } else {
                (refname.trim_start_matches("refs/heads/").to_string(), None)
            };

            let upstream = parts
                .get(4)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(String::from);
            let last_commit_timestamp = parts
                .get(5)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            let last_author = parts.get(6).unwrap_or(&"").trim().to_string();
            let last_summary = parts.get(7).unwrap_or(&"").trim().to_string();

            let (commits_ahead_of_base, commits_behind_base) = if has_ahead_behind {
                parse_ahead_behind_field(parts.get(8).copied().unwrap_or(""))
            } else {
                (0, 0)
            };

            branches.push(BranchInfo {
                name,
                is_current,
                is_remote,
                remote_name,
                tip_commit_id: tip,
                ahead_count,
                behind_count,
                upstream,
                is_default: false,
                is_gone: git_cli::upstream_is_gone(track),
                last_commit_timestamp,
                last_author,
                last_summary,
                commits_ahead_of_base,
                commits_behind_base,
                additions: 0,
                deletions: 0,
                files_changed: 0,
                compared_to: has_ahead_behind.then(|| default_short.clone()).flatten(),
            });
        }

        let local_names: Vec<String> = branches
            .iter()
            .filter(|b| !b.is_remote)
            .map(|b| b.name.clone())
            .collect();
        let default_branch = pick_default_branch(&local_names, origin_head.as_deref(), &remote);
        for branch in &mut branches {
            if !branch.is_remote && branch.name == default_branch {
                branch.is_default = true;
            }
        }

        Ok(branches)
    }

    /// Computes churn for eligible branches against the resolved default
    /// branch.
    ///
    /// This is the expensive half of branch loading: up to
    /// MAX_BRANCH_STAT_TARGETS branches x 2 git processes. It runs after (not
    /// during) list_branches so the list renders immediately, and memoizes
    /// through the oid-keyed churn cache so repeat calls are cheap.
    pub fn branch_stats(repo_path: &str) -> Result<BranchStatsReport, String> {
        let repo = validate_repo(repo_path)?;
        let repo_key = repo.to_string_lossy().into_owned();
        let remote = resolve_default_remote(&repo);
        let head_ref = remote_head_ref(&remote);
        let origin_head = git_text(&repo, &["symbolic-ref", "--quiet", head_ref.as_str()]).ok();

        // Cheap listing only: refnames and tips, no history walks here.
        // Refnames and object ids cannot contain NULs, so %00 fields stay
        // aligned where \x01 could be split by hostile ref-adjacent content.
        let stdout = git_text(
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)",
                "refs/heads/",
                "refs/remotes/",
            ],
        )?;

        let mut local_names: Vec<String> = Vec::new();
        let mut targets: Vec<BranchStatTarget> = Vec::new();
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\0').collect();
            if parts.len() < 2 || parts[1].is_empty() {
                continue;
            }
            let refname = parts[0];
            if refname.ends_with("/HEAD") {
                continue;
            }
            let is_remote = refname.starts_with("refs/remotes/");
            let (name, remote_name) = if is_remote {
                let rest = refname.trim_start_matches("refs/remotes/");
                let remote = rest.split('/').next().map(String::from);
                (rest.to_string(), remote)
            } else {
                (refname.trim_start_matches("refs/heads/").to_string(), None)
            };
            if !is_remote {
                local_names.push(name.clone());
            }
            targets.push(BranchStatTarget {
                name,
                tip_commit_id: parts[1].to_string(),
                is_remote,
                remote_name,
            });
        }

        let default_branch = pick_default_branch(&local_names, origin_head.as_deref(), &remote);
        // Same resolution rules as list_branches' pre-listing probe; when that
        // finds nothing (no origin/HEAD, no conventional name), fall back to
        // pick_default_branch's choice now that the locals are known.
        let resolved_default = resolve_default_base_on(&repo, &remote, origin_head.as_deref())
            .or_else(|| {
                let refname = format!("refs/heads/{default_branch}^{{commit}}");
                git_text(&repo, &["rev-parse", "--verify", "--quiet", &refname])
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && validate_oid(s).is_ok())
                    .map(|oid| (default_branch.clone(), oid))
            });
        let Some((compared_to, base_oid)) = resolved_default else {
            return Ok(BranchStatsReport {
                compared_to: default_branch.clone(),
                updates: Vec::new(),
                computed: 0,
                cached: 0,
                capped: false,
                compute_failures: 0,
            });
        };

        // Eligibility identical to the enrichment this command replaced:
        // skip the default branch and remote branches shadowed by a local twin.
        let local_set: HashSet<&str> = local_names.iter().map(String::as_str).collect();
        let mut eligible: Vec<BranchStatTarget> = Vec::new();
        for target in targets {
            if !target.is_remote && target.name == default_branch {
                continue;
            }
            if target.name == compared_to {
                continue;
            }
            if target.is_remote {
                let local =
                    strip_remote_prefix(target.name.as_str(), target.remote_name.as_deref());
                if local_set.contains(local) {
                    continue;
                }
            }
            eligible.push(target);
        }

        let (updates, computed, cached, capped, compute_failures) =
            compute_eligible_churn(&repo, &repo_key, &base_oid, eligible);

        Ok(BranchStatsReport {
            compared_to,
            updates,
            computed,
            cached,
            capped,
            compute_failures,
        })
    }

    pub fn head_id(repo_path: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        Ok(git_text(&repo, &["rev-parse", "HEAD"])?.trim().to_string())
    }

    pub fn read_commit_history(
        repo_path: &str,
        max_count: usize,
        revision: Option<&str>,
    ) -> Result<Vec<RawCommitNode>, String> {
        Self::read_commit_history_paged(repo_path, 0, max_count, revision)
    }

    /// Reads one page of history, oldest-first paging via `--skip`.
    ///
    /// A single walk is capped at [`MAX_HISTORY_COMMITS`] (plus one probe row
    /// for the caller's has_more check — see [`Self::page_count_limit`]);
    /// callers that need deeper history paginate rather than raise the cap, so
    /// a request can never ask git for an unbounded log on a monorepo-scale
    /// repository.
    pub fn read_commit_history_paged(
        repo_path: &str,
        skip: usize,
        max_count: usize,
        revision: Option<&str>,
    ) -> Result<Vec<RawCommitNode>, String> {
        let repo = validate_repo(repo_path)?;
        let count = Self::page_count_limit(max_count).to_string();
        let skipped = skip.min(Self::MAX_HISTORY_COMMITS).to_string();
        let count_arg = format!("-n{}", count);
        let skip_arg = format!("--skip={}", skipped);
        let mut args = vec![
            "log",
            count_arg.as_str(),
            skip_arg.as_str(),
            "--topo-order",
            // Subjects may legally contain any byte except NUL, so the RECORD
            // terminator is %x00 — a hostile subject can no longer split the
            // stream into bogus records. Fields use %x01, which a subject,
            // author name or email may legally contain too: the risky
            // variable-length fields therefore sit at the END of the record
            // and parsing works from the RIGHT (see [`parse_history_record`]),
            // so such a byte can only truncate the one field it landed in —
            // never shift id, parents or timestamp.
            "--format=format:%H%x01%P%x01%ct%x01%s%x01%an%x01%ae%x00",
        ];
        if let Some(rev) = revision {
            validate_ref_name(rev)?;
            args.push(rev);
        } else {
            args.push("--all");
        }
        let stdout = git_text(&repo, &args)?;

        // Each record is exactly six NUL-delimited fields plus its terminator.
        // `format:` separates entries with a bare newline AFTER our %x00, so
        let mut commits = Vec::new();
        for record in stdout.split('\x00') {
            let record = record.trim_matches('\n');
            if record.is_empty() {
                continue;
            }
            if let Some(node) = parse_history_record(record) {
                commits.push(node);
            }
        }
        Ok(commits)
    }

    /// `-n` bound handed to git for one history page: the caller's cap plus
    /// one overshoot slot.
    ///
    /// The extra slot exists only for the caller's has_more probe:
    /// cmd_get_commit_graph fetches cap+1 rows and drops the overflow before
    /// lanes are solved. Clamping back down to the ceiling here swallowed that
    /// probe at exactly MAX_HISTORY_COMMITS, so a repository at the ceiling
    /// reported has_more=false forever while silently truncating its history —
    /// truncation presented as fact. Only the count widens; `skip` keeps its
    /// own clamp because paging past the window is a separate concern from
    /// probing one row past it.
    pub(crate) fn page_count_limit(max_count: usize) -> usize {
        max_count.clamp(1, Self::MAX_HISTORY_COMMITS.saturating_add(1))
    }

    /// Walks the first-parent chain from `tip_oid` NEWEST-first, stopping at
    /// (and including) the first commit whose id is in `stop_at`, and returns
    /// the visited ids OLDEST-first so consecutive pairs are first-parent
    /// edges. `None` when the walk exhausted `max_commits` without reaching a
    /// stop id — the caller must treat that as "no discoverable base", never
    /// as "rooted at the default branch".
    ///
    /// This repairs the stack hierarchy on long-lived repositories where the
    /// global `--all` history window ends before a branch's fork point: one
    /// bounded subprocess per unresolved branch instead of lifting the global
    /// cap for every request.
    pub fn first_parent_chain(
        repo_path: &str,
        tip_oid: &str,
        stop_at: &HashSet<String>,
        max_commits: usize,
    ) -> Result<Option<Vec<String>>, String> {
        if stop_at.contains(tip_oid) {
            // The tip itself is another branch's tip; there is nothing to walk.
            return Ok(Some(vec![tip_oid.to_string()]));
        }
        let repo = validate_repo(repo_path)?;
        let count = max_commits.clamp(1, Self::MAX_HISTORY_COMMITS).to_string();
        let count_arg = format!("--max-count={}", count);
        let stdout = git_text(
            &repo,
            &["rev-list", "--first-parent", count_arg.as_str(), tip_oid],
        )?;
        let mut walked: Vec<String> = Vec::new();
        for line in stdout.lines() {
            let oid = line.trim();
            if oid.is_empty() {
                continue;
            }
            walked.push(oid.to_string());
            if stop_at.contains(oid) {
                walked.reverse();
                return Ok(Some(walked));
            }
        }
        Ok(None)
    }

    /// Lists commits (newest-first oids) that touched `file_path`, walking
    /// the same revisions the graph walk does: all refs when `revision` is
    /// None, otherwise the user-selected revision.
    ///
    /// Revision parity is not optional polish: the graph retains only rows in
    /// this allow-set, so a HEAD-only walk here silently dropped every
    /// other-branch commit touching the path while `--all` kept their lanes
    /// alive — whole branches vanished from the graph the moment a path
    /// filter was applied.
    pub fn commits_touching_path(
        repo_path: &str,
        file_path: &str,
        max_count: usize,
        revision: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let repo = validate_repo(repo_path)?;
        // Canonical join resolves existing prefixes through symlinks and keeps
        // not-yet-tracked leaves lexical, so a symlinked directory cannot
        // redirect the query outside the repository.
        sandbox_join_canonical(&repo, file_path)?;
        let count = max_count.clamp(1, 100_000).to_string();
        let spec = literal_pathspec(file_path);
        let count_arg = format!("-n{}", count);
        let mut args = vec![
            "-c",
            "core.quotepath=off",
            "log",
            count_arg.as_str(),
            "--format=%H",
        ];
        if let Some(rev) = revision {
            validate_ref_name(rev)?;
            args.push(rev);
        } else {
            args.push("--all");
        }
        args.push("--");
        args.push(spec.as_str());
        let stdout = git_text(&repo, &args)?;
        Ok(stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    pub fn get_status(repo_path: &str) -> Result<Vec<FileStatus>, String> {
        let repo = validate_repo(repo_path)?;
        // -z disables path quoting outright; core.quotepath=off is kept as
        // belt-and-braces so a future non-z invocation still gets raw UTF-8.
        let stdout = git_text(
            &repo,
            &["-c", "core.quotepath=off", "status", "--porcelain=v1", "-z"],
        )?;
        // numstat failures are surfaced, not laundered into "zero churn": a
        // broken diff must fail the report rather than fabricate numbers, and
        // records that decode but carry unparseable counts ride their row as
        // an explicit warning instead of reading as fact.
        let numstat_work =
            parse_numstat_with_issues(&git_text(&repo, &["diff", "--numstat", "-z"])?);
        let numstat_index =
            parse_numstat_with_issues(&git_text(&repo, &["diff", "--cached", "--numstat", "-z"])?);

        let mut statuses = Vec::new();
        for record in parse_status_records(stdout.as_bytes()) {
            let RawStatusRecord {
                index_status,
                work_status,
                path,
                old_path,
            } = record;
            let is_conflicted = index_status == 'U'
                || work_status == 'U'
                || (index_status == 'A' && work_status == 'A')
                || (index_status == 'D' && work_status == 'D');
            let is_staged = index_status != ' ' && index_status != '?';
            // Only the side this row's numbers actually come from can make
            // the row lie about them.
            let active = if is_staged {
                &numstat_index
            } else {
                &numstat_work
            };
            let (additions, deletions) = active.churn.get(&path).copied().unwrap_or((0, 0));
            let warnings: Vec<String> = active
                .issues
                .iter()
                .filter(|(warned_path, _)| *warned_path == path)
                .map(|(_, reason)| reason.clone())
                .collect();

            statuses.push(FileStatus {
                path,
                old_path,
                status_code: format!("{}{}", index_status, work_status),
                is_staged,
                is_conflicted,
                additions,
                deletions,
                warnings,
            });
        }
        Ok(statuses)
    }

    pub fn get_file_blame(repo_path: &str, file_path: &str) -> Result<Vec<BlameLine>, String> {
        let repo = validate_repo(repo_path)?;
        // Canonical join resolves existing prefixes through symlinks so a
        // symlinked directory cannot redirect the read outside the repository;
        // not-yet-tracked leaves stay lexical.
        sandbox_join_canonical(&repo, file_path)?;
        // NOTE: no :(literal) magic here. `git blame` treats its <file>
        // argument as a literal path, NOT a pathspec (globs do not widen:
        // "weird?.txt" with no such literal file fails outright), and it
        // rejects pathspec magic entirely ("fatal: no such path
        // ':(literal)...' in HEAD"), so prefixing would break every call.
        let stdout = git_text(&repo, &["blame", "--line-porcelain", "--", file_path])?;
        Ok(parse_blame_porcelain(&stdout))
    }

    /// Lists every path the file explorer may show: tracked files plus
    /// untracked-but-not-ignored working-tree files.
    ///
    /// `--cached --others --exclude-standard` is deliberately the same
    /// enumeration [`GitReader::get_repo_language_stats`] uses: `--cached`
    /// covers the index; `--others --exclude-standard` adds untracked files
    /// while excluding exactly what standard ignore rules (.gitignore,
    /// .git/info/exclude, core.excludesFile) ignore — so build output never
    /// appears and a freshly saved source file does. `-z` with
    /// `core.quotepath=off` delivers raw UTF-8 paths NUL-separated, the same
    /// belt-and-braces pairing as [`GitReader::get_status`], so spaces,
    /// quotes, glob characters, and non-ASCII names survive verbatim.
    pub fn list_repo_files(repo_path: &str) -> Result<Vec<String>, String> {
        let repo = validate_repo(repo_path)?;
        let stdout = git_text(
            &repo,
            &[
                "-c",
                "core.quotepath=off",
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;
        let files = parse_ls_files_entries(&stdout);
        if files.len() > MAX_REPO_FILES {
            return Err(format!(
                "File explorer unavailable: this repository contains more than {MAX_REPO_FILES} files"
            ));
        }
        Ok(files)
    }

    pub fn get_file_diff(
        repo_path: &str,
        file_path: &str,
        is_staged: bool,
        ignore_whitespace: bool,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        // Canonical join resolves existing prefixes through symlinks and keeps
        // not-yet-tracked leaves lexical (see get_file_blame).
        sandbox_join_canonical(&repo, file_path)?;
        if !is_staged {
            // `git diff` emits nothing for untracked paths, which rendered a
            // blank diff pane for brand-new files. Synthesize git-shaped
            // new-file output from the worktree bytes instead.
            if let Some(synthesized) = untracked_new_file_diff(&repo, file_path)? {
                return Ok(synthesized);
            }
        }
        // :(literal) stops `*?[` in a filename from widening the pathspec.
        let spec = literal_pathspec(file_path);
        let mut args = vec!["-c", "core.quotepath=off", "diff"];
        if is_staged {
            args.push("--cached");
        }
        if ignore_whitespace {
            args.push("-w");
        }
        args.push("--");
        args.push(spec.as_str());
        git_text(&repo, &args)
    }

    pub fn get_commit_diff(repo_path: &str, commit_id: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        validate_oid(commit_id)?;
        // --diff-merges=first-parent renders merge commits as ordinary hunks
        // against the first parent instead of the combined --cc format whose
        // @@@ headers the frontend cannot number (git >= 2.31).
        git_text(
            &repo,
            &[
                "-c",
                "core.quotepath=off",
                "show",
                "--unified=3",
                "--diff-merges=first-parent",
                "--format=",
                commit_id,
            ],
        )
    }

    pub fn get_commit_file_diff(
        repo_path: &str,
        commit_id: &str,
        file_path: &str,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        validate_oid(commit_id)?;
        sandbox_join_canonical(&repo, file_path)?;
        let spec = literal_pathspec(file_path);
        git_text(
            &repo,
            &[
                "-c",
                "core.quotepath=off",
                "show",
                "--unified=3",
                "--format=",
                commit_id,
                "--",
                &spec,
            ],
        )
    }

    pub fn get_range_diff(repo_path: &str, from: &str, to: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        validate_ref_name(from)?;
        validate_ref_name(to)?;
        let spec = format!("{}...{}", from, to);
        git_text(
            &repo,
            &["-c", "core.quotepath=off", "diff", "--unified=3", &spec],
        )
    }

    pub fn get_commit_files(
        repo_path: &str,
        commit_id: &str,
    ) -> Result<Vec<CommitFileChange>, String> {
        let repo = validate_repo(repo_path)?;
        validate_oid(commit_id)?;
        let stdout = git_text(
            &repo,
            &["show", "--pretty=format:", "--numstat", "-z", commit_id],
        )?;
        Ok(parse_numstat_files(&stdout))
    }

    pub fn get_file_content(
        repo_path: &str,
        file_path: &str,
        commit_id: Option<&str>,
    ) -> Result<String, String> {
        let blob = Self::get_file_blob(repo_path, file_path, commit_id)?;
        if let Some(text) = blob.text {
            Ok(text)
        } else {
            Err("File is binary".into())
        }
    }

    pub fn get_file_blob(
        repo_path: &str,
        file_path: &str,
        commit_id: Option<&str>,
    ) -> Result<FileBlob, String> {
        let repo = validate_repo(repo_path)?;
        let dest = sandbox_join_canonical(&repo, file_path)?;
        let bytes = if let Some(id) = commit_id {
            crate::engine::git_writer::validate_oid_or_revision(id)?;
            if id.contains(':') {
                return Err("Invalid revision".into());
            }
            let spec = format!("{}:{}", id, file_path);
            git(&repo, &["show", &spec])?
        } else if dest.exists() {
            check_working_tree_size(&dest, MAX_WORKING_TREE_BYTES)?;
            std::fs::read(&dest).map_err(|e| format!("Failed to read from disk: {}", e))?
        } else {
            git(&repo, &["show", &format!(":{}", file_path)])?
        };

        let lang = LanguageDetector::detect_from_bytes(file_path, &bytes);
        let is_image = lang.name == "Image";
        let is_binary = is_image || bytes.contains(&0);
        let mime = if is_image {
            mime_from_path(file_path)
        } else {
            "application/octet-stream".into()
        };

        Ok(FileBlob {
            path: file_path.to_string(),
            is_binary,
            is_image,
            mime,
            text: if is_binary {
                None
            } else {
                Some(String::from_utf8_lossy(&bytes).into_owned())
            },
            base64: if is_binary {
                Some(b64_encode(&bytes))
            } else {
                None
            },
        })
    }

    pub fn get_commit_details(repo_path: &str, commit_id: &str) -> Result<CommitDetails, String> {
        let repo = validate_repo(repo_path)?;
        validate_oid(commit_id)?;
        let stdout = git_text(
            &repo,
            &[
                "show",
                "-s",
                "--format=%H%x00%P%x00%an%x00%ae%x00%ad%x00%cn%x00%ce%x00%cd%x00%G?%x00%s%x00%b",
                commit_id,
            ],
        )?;
        let fields: Vec<&str> = stdout.split('\x00').collect();
        if fields.len() < 11 {
            return Err("Failed to parse commit metadata format".into());
        }
        let body = fields[10].trim().to_string();
        // Surface file-list failures instead of reporting a commit with
        // silently-empty changes.
        let changed_files = Self::get_commit_files(repo_path, commit_id)?;
        let total_additions = changed_files.iter().map(|f| f.additions).sum();
        let total_deletions = changed_files.iter().map(|f| f.deletions).sum();
        Ok(CommitDetails {
            id: fields[0].to_string(),
            parent_ids: if fields[1].is_empty() {
                Vec::new()
            } else {
                fields[1].split_whitespace().map(String::from).collect()
            },
            author_name: fields[2].to_string(),
            author_email: fields[3].to_string(),
            author_date: fields[4].to_string(),
            committer_name: fields[5].to_string(),
            committer_email: fields[6].to_string(),
            committer_date: fields[7].to_string(),
            gpg_status: fields[8].to_string(),
            summary: fields[9].to_string(),
            co_authors: parse_co_authors(&body),
            body,
            changed_files,
            total_additions,
            total_deletions,
        })
    }

    pub fn list_tags(repo_path: &str) -> Result<Vec<TagInfo>, String> {
        let repo = validate_repo(repo_path)?;
        let stdout = git_text(
            &repo,
            &[
                "tag",
                "-l",
                "--sort=-creatordate",
                "--format=%(refname:short)%00%(objectname)%00%(contents:subject)",
            ],
        )?;
        let mut tags = Vec::new();
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\x00').collect();
            if parts.len() >= 2 {
                tags.push(TagInfo {
                    name: parts[0].to_string(),
                    commit_id: parts[1].to_string(),
                    message: parts
                        .get(2)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                });
            }
        }
        tags.truncate(TAG_LIST_CAP);
        Ok(tags)
    }

    pub fn get_reflog(repo_path: &str, max_entries: usize) -> Result<Vec<ReflogEntry>, String> {
        let repo = validate_repo(repo_path)?;
        let count = max_entries.clamp(1, 10_000).to_string();
        let stdout = git_text(
            &repo,
            &[
                "reflog",
                &format!("-n{}", count),
                "--format=%H%x00%gd%x00%gs%x00%ct",
            ],
        )?;
        let mut entries = Vec::new();
        for (idx, line) in stdout.lines().enumerate() {
            let parts: Vec<&str> = line.split('\x00').collect();
            if parts.len() < 4 {
                continue;
            }
            let full_msg = parts[2];
            let (action, message) = if let Some(colon_idx) = full_msg.find(':') {
                (
                    full_msg[..colon_idx].trim().to_string(),
                    full_msg[colon_idx + 1..].trim().to_string(),
                )
            } else {
                ("action".to_string(), full_msg.to_string())
            };
            entries.push(ReflogEntry {
                index: idx,
                commit_id: parts[0].to_string(),
                selector: parts[1].to_string(),
                action,
                message,
                timestamp: parts[3].parse().unwrap_or(0),
            });
        }
        Ok(entries)
    }

    pub fn get_repo_language_stats(repo_path: &str) -> Result<LanguageStatsReport, String> {
        Self::get_repo_language_stats_bounded(repo_path, Some(LANG_STATS_DEADLINE))
    }

    /// Like [`Self::get_repo_language_stats`] with an explicit budget;
    /// `None` means unbounded (tests).
    pub fn get_repo_language_stats_bounded(
        repo_path: &str,
        deadline: Option<Duration>,
    ) -> Result<LanguageStatsReport, String> {
        let started = Instant::now();
        let expired = || match deadline {
            Some(d) => started.elapsed() >= d,
            None => false,
        };
        let repo = validate_repo(repo_path)?;
        let stdout = git_text(
            &repo,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;

        let mut candidates = Vec::new();
        for rel_path in stdout.split('\0') {
            let rel_path = LanguageDetector::normalize_rel_path(rel_path);
            if rel_path.is_empty() || LanguageDetector::is_ignored_source_path(&rel_path) {
                continue;
            }
            let lang_info = LanguageDetector::detect_from_path(&rel_path);
            if !LanguageDetector::should_count_for_stats(&rel_path, &lang_info) {
                continue;
            }
            candidates.push((rel_path, lang_info));
        }
        let candidate_files = candidates.len();
        let selected = LanguageDetector::prioritize_for_stats(candidates, 10_000);
        let selected_len = selected.len();

        let mut lang_counts: HashMap<&'static str, (usize, usize, &'static str, &'static str)> =
            HashMap::new();
        let mut total_lines = 0usize;
        let mut scanned_files = 0usize;
        let mut attempted_files = 0usize;
        let mut deadline_hit = false;

        for (rel_path, path_info) in selected {
            if expired() {
                deadline_hit = true;
                break;
            }
            attempted_files += 1;
            // Resolve through symlinks so a tracked file that is really a link
            // pointing outside the repo is refused instead of read.
            let full_path = match git_cli::sandbox_join_canonical(&repo, &rel_path) {
                Ok(path) => path,
                Err(_) => {
                    record_lang(&mut lang_counts, path_info, 0);
                    continue;
                }
            };
            // Stat before reading: a tracked multi-gigabyte file must not be
            // pulled into memory just to discover it exceeds the budget. The
            // post-read length check stays as defense against growth between
            // stat and read.
            if check_working_tree_size(&full_path, 1_048_576).is_err() {
                record_lang(&mut lang_counts, path_info, 0);
                continue;
            }
            let bytes = match std::fs::read(&full_path) {
                Ok(bytes) => bytes,
                Err(_) => {
                    record_lang(&mut lang_counts, path_info, 0);
                    continue;
                }
            };
            scanned_files += 1;
            if bytes.len() > 1_048_576 {
                record_lang(&mut lang_counts, path_info, 0);
                continue;
            }
            if LanguageDetector::looks_binary(&bytes) && !path_info.is_programming() {
                continue;
            }
            let lang_info = LanguageDetector::detect_from_bytes(&rel_path, &bytes);
            if !LanguageDetector::should_count_for_stats(&rel_path, &lang_info) {
                continue;
            }
            let content = String::from_utf8_lossy(&bytes);
            let loc = LocCounter::count(&content, LanguageDetector::comment_prefix(lang_info.name));
            record_lang(&mut lang_counts, lang_info, loc.code_lines);
            total_lines += loc.code_lines;
        }

        let mut stats: Vec<RepoLanguageStat> = lang_counts
            .into_iter()
            .map(|(name, (code_lines, file_count, color_hex, category))| {
                let percentage = if total_lines > 0 {
                    (code_lines as f64 / total_lines as f64) * 100.0
                } else {
                    0.0
                };
                RepoLanguageStat {
                    language: name.to_string(),
                    color_hex: color_hex.to_string(),
                    category: category.to_string(),
                    code_lines,
                    file_count,
                    percentage: (percentage * 10.0).round() / 10.0,
                }
            })
            .collect();
        stats.sort_by(|a, b| {
            let pa = u8::from(a.category == "programming");
            let pb = u8::from(b.category == "programming");
            pb.cmp(&pa)
                .then_with(|| b.code_lines.cmp(&a.code_lines))
                .then_with(|| a.language.cmp(&b.language))
        });
        Ok(LanguageStatsReport {
            stats,
            // Deadline hit, or the 10k prioritization cap dropped candidates,
            // mean the reported numbers cover only part of the worktree.
            truncated: deadline_hit || attempted_files < selected_len,
            scanned_files,
            candidate_files,
        })
    }
}

/// Resolves the remote whose HEAD marks the repository's default branch.
///
/// Priority: `checkout.defaultRemote`, the current branch's configured
/// upstream remote, a lone configured remote, else "origin".
pub(crate) fn resolve_default_remote(repo: &Path) -> String {
    if let Ok(configured) = git_text(repo, &["config", "--get", "checkout.defaultRemote"]) {
        let trimmed = configured.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(branch) = git_text(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        let branch = branch.trim();
        if !branch.is_empty() {
            let key = format!("branch.{branch}.remote");
            if let Ok(upstream) = git_text(repo, &["config", "--get", key.as_str()]) {
                let trimmed = upstream.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    if let Ok(remotes) = git_text(repo, &["remote"]) {
        let names: Vec<&str> = remotes
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if let [only] = names.as_slice() {
            return (*only).to_string();
        }
    }
    "origin".to_string()
}

/// Remote-tracking refname whose symref target marks the default branch.
pub(crate) fn remote_head_ref(remote: &str) -> String {
    format!("refs/remotes/{remote}/HEAD")
}

fn pick_default_branch(local_names: &[String], remote_head: Option<&str>, remote: &str) -> String {
    if let Some(head) = remote_head {
        let trimmed = head.trim();
        let full_prefix = format!("refs/remotes/{remote}/");
        let short = trimmed
            .strip_prefix(full_prefix.as_str())
            .or_else(|| trimmed.strip_prefix(format!("{remote}/").as_str()))
            .unwrap_or_else(|| trimmed.rsplit('/').next().unwrap_or(trimmed));
        if local_names.iter().any(|n| n == short) {
            return short.to_string();
        }
    }
    for candidate in ["main", "master", "trunk", "develop"] {
        if local_names.iter().any(|n| n == candidate) {
            return candidate.to_string();
        }
    }
    local_names
        .first()
        .cloned()
        .unwrap_or_else(|| "main".to_string())
}

/// for-each-ref format for the branch list: everything except churn. The
/// ahead-behind atom is appended dynamically only when a default base resolves.
///
/// Fields are NUL-separated (`%00` — for-each-ref only understands octal
/// escapes, unlike log's `%xNN`): author names and commit subjects may legally
/// contain any byte except NUL, so \x01 separators could be split by hostile
/// content while %00 cannot.
const BRANCH_LIST_FORMAT: &str = "%(HEAD)%00%(refname)%00%(objectname)%00%(upstream:track)%00%(upstream:short)%00%(committerdate:unix)%00%(authorname)%00%(contents:subject)";

/// Resolves the default branch to (short name, commit oid) without needing
/// the local ref list, so callers can use it before listing refs.
///
/// Priority mirrors [`pick_default_branch`]: the primary remote's HEAD branch
/// first (local head preferred over the remote-tracking ref so remote-only
/// repos still resolve), then conventional main/master/trunk/develop heads.
/// None when no candidate resolves (empty or detached-only repository).
pub(crate) fn resolve_default_base_on(
    repo: &Path,
    remote: &str,
    remote_head: Option<&str>,
) -> Option<(String, String)> {
    // Ordered (short name, full ref). First hit wins; duplicates are skipped
    // so a remote HEAD that already is `main` does not probe twice.
    let mut ordered: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push = |ordered: &mut Vec<(String, String)>,
                seen: &mut HashSet<String>,
                short: &str,
                refname: String| {
        if seen.insert(refname.clone()) {
            ordered.push((short.to_string(), refname));
        }
    };

    if let Some(head) = remote_head.map(str::trim) {
        if let Some(short) = strip_remote_tracking(head, remote) {
            push(
                &mut ordered,
                &mut seen,
                short,
                format!("refs/heads/{short}"),
            );
            push(&mut ordered, &mut seen, short, head.to_string());
        }
    }
    for candidate in ["main", "master", "trunk", "develop"] {
        push(
            &mut ordered,
            &mut seen,
            candidate,
            format!("refs/heads/{candidate}"),
        );
    }
    if ordered.is_empty() {
        return None;
    }

    let mut arg_store = Vec::with_capacity(2 + ordered.len());
    arg_store.push("for-each-ref".to_string());
    arg_store.push("--format=%(refname)%00%(objectname)".to_string());
    arg_store.extend(ordered.iter().map(|(_, refname)| refname.clone()));
    let args: Vec<&str> = arg_store.iter().map(String::as_str).collect();
    let stdout = git_text(repo, &args).ok()?;

    let mut found: HashMap<String, String> = HashMap::new();
    for line in stdout.lines() {
        let mut parts = line.split('\0');
        let Some(refname) = parts.next() else {
            continue;
        };
        let Some(oid) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if validate_oid(oid).is_ok() {
            found.insert(refname.to_string(), oid.to_string());
        }
    }
    ordered
        .into_iter()
        .find_map(|(short, refname)| found.get(&refname).cloned().map(|oid| (short, oid)))
}

/// Strips `refs/remotes/{remote}/` from a remote HEAD symbolic ref.
fn strip_remote_tracking<'a>(head: &'a str, remote: &str) -> Option<&'a str> {
    let rest = head.strip_prefix("refs/remotes/")?;
    let (name, short) = rest.split_once('/')?;
    if name == remote && !short.is_empty() {
        Some(short)
    } else {
        None
    }
}

/// Branch name with the `remote/` prefix removed, without allocating a
/// `"{remote}/"` prefix string per call.
fn strip_remote_prefix<'a>(name: &'a str, remote: Option<&str>) -> &'a str {
    let Some(remote) = remote else {
        return name;
    };
    if name.len() > remote.len()
        && name.as_bytes().get(remote.len()) == Some(&b'/')
        && name.starts_with(remote)
    {
        &name[remote.len() + 1..]
    } else {
        name
    }
}

/// Parses `%(ahead-behind:<base>)` output (`"<ahead> <behind>"`). Malformed
/// input yields zeros rather than failing the whole listing.
fn parse_ahead_behind_field(raw: &str) -> (usize, usize) {
    let bytes = raw.as_bytes();
    let mut i = 0;
    let ahead = next_decimal_token(bytes, &mut i);
    let behind = next_decimal_token(bytes, &mut i);
    (ahead, behind)
}

fn next_decimal_token(bytes: &[u8], i: &mut usize) -> usize {
    while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
        *i += 1;
    }
    if *i >= bytes.len() {
        return 0;
    }
    let start = *i;
    let mut n = 0usize;
    let mut valid = true;
    while *i < bytes.len() && !bytes[*i].is_ascii_whitespace() {
        let b = bytes[*i];
        *i += 1;
        if valid && b.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
        } else {
            valid = false;
        }
    }
    if !valid || *i == start {
        0
    } else {
        n
    }
}

/// One listed ref awaiting churn computation in [`GitReader::branch_stats`].
struct BranchStatTarget {
    name: String,
    tip_commit_id: String,
    is_remote: bool,
    remote_name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ComputedBranchChurn {
    additions: usize,
    deletions: usize,
    files_changed: usize,
    commits_ahead: usize,
    commits_behind: usize,
}

const ZERO_CHURN: ComputedBranchChurn = ComputedBranchChurn {
    additions: 0,
    deletions: 0,
    files_changed: 0,
    commits_ahead: 0,
    commits_behind: 0,
};

type ChurnKey = (String, String, String);

/// Content-addressed memo for branch churn keyed by (repo path, base oid, tip
/// oid): churn depends only on the two trees, so entries cannot go stale — a
/// force-moved branch simply misses on its new tip.
///
/// Recency is a monotonic sequence, not a linear `retain` scan: re-insert is
/// O(1) under the mutex (push a new order node, bump seq). Eviction pops
/// stale order nodes until it finds a seq that still matches the map.
struct ChurnCache {
    capacity: usize,
    seq: u64,
    entries: HashMap<ChurnKey, (u64, ComputedBranchChurn)>,
    order: VecDeque<(ChurnKey, u64)>,
}

impl ChurnCache {
    fn new() -> Self {
        Self {
            capacity: CHURN_CACHE_CAPACITY,
            seq: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, key: &ChurnKey) -> Option<ComputedBranchChurn> {
        self.entries.get(key).map(|(_, value)| *value)
    }

    /// Inserts a value, refreshing the key's recency and evicting oldest-seq
    /// keys past the capacity bound.
    fn insert(&mut self, key: ChurnKey, value: ComputedBranchChurn) {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        self.entries.insert(key.clone(), (seq, value));
        self.order.push_back((key, seq));
        self.evict_overflow();
    }

    fn evict_overflow(&mut self) {
        while self.entries.len() > self.capacity {
            let Some((key, seq)) = self.order.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&key)
                .is_some_and(|(stored, _)| *stored == seq)
            {
                self.entries.remove(&key);
            }
        }
        // Re-inserts leave stale order nodes; compact before the deque can
        // grow without bound under a hot key.
        if self.order.len() > self.capacity.saturating_mul(2) {
            let mut live = VecDeque::with_capacity(self.entries.len());
            for (key, seq) in self.order.drain(..) {
                if self
                    .entries
                    .get(&key)
                    .is_some_and(|(stored, _)| *stored == seq)
                {
                    live.push_back((key, seq));
                }
            }
            self.order = live;
        }
    }
}

fn churn_cache() -> MutexGuard<'static, ChurnCache> {
    static CACHE: OnceLock<Mutex<ChurnCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(ChurnCache::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Looks up cache in bulk, computes unique uncached tips (capped), stores
/// results, and returns an update per eligible branch we have a value for.
/// The fifth tuple element counts attempted walks that FAILED — branches
/// which will be missing from `updates` through error, not through capping.
#[allow(clippy::type_complexity)]
fn compute_eligible_churn(
    repo: &Path,
    repo_key: &str,
    base_oid: &str,
    eligible: Vec<BranchStatTarget>,
) -> (Vec<BranchStatsUpdate>, usize, usize, bool, usize) {
    let mut cached_hits: HashMap<String, ComputedBranchChurn> = HashMap::new();
    let mut unique_uncached: Vec<String> = Vec::new();
    let mut seen_miss: HashSet<String> = HashSet::new();

    {
        let cache = churn_cache();
        for target in &eligible {
            let tip = target.tip_commit_id.as_str();
            if cached_hits.contains_key(tip) || seen_miss.contains(tip) {
                continue;
            }
            let key = (
                repo_key.to_string(),
                base_oid.to_string(),
                target.tip_commit_id.clone(),
            );
            if let Some(hit) = cache.get(&key) {
                cached_hits.insert(target.tip_commit_id.clone(), hit);
            } else if seen_miss.insert(target.tip_commit_id.clone()) {
                unique_uncached.push(target.tip_commit_id.clone());
            }
        }
    }

    let remaining_after = unique_uncached
        .len()
        .saturating_sub(MAX_BRANCH_STAT_TARGETS);
    let capped = remaining_after > 0;
    unique_uncached.truncate(MAX_BRANCH_STAT_TARGETS);
    let attempted = unique_uncached.len();

    let computed_map: HashMap<String, ComputedBranchChurn> = unique_uncached
        .into_par_iter()
        .filter_map(|tip| compute_branch_churn(repo, base_oid, &tip).map(|churn| (tip, churn)))
        .collect();
    let compute_failures = attempted.saturating_sub(computed_map.len());

    {
        let mut cache = churn_cache();
        for (tip, churn) in &computed_map {
            cache.insert(
                (repo_key.to_string(), base_oid.to_string(), tip.clone()),
                *churn,
            );
        }
    }

    let mut updates = Vec::with_capacity(eligible.len());
    let mut computed = 0;
    let mut cached = 0;
    for target in eligible {
        let churn = if let Some(churn) = computed_map.get(&target.tip_commit_id) {
            computed += 1;
            *churn
        } else if let Some(churn) = cached_hits.get(&target.tip_commit_id) {
            cached += 1;
            *churn
        } else {
            continue;
        };
        updates.push(BranchStatsUpdate {
            name: target.name,
            tip_commit_id: target.tip_commit_id,
            is_remote: target.is_remote,
            remote_name: target.remote_name,
            additions: churn.additions,
            deletions: churn.deletions,
            files_changed: churn.files_changed,
            commits_ahead_of_base: churn.commits_ahead,
            commits_behind_base: churn.commits_behind,
        });
    }

    (updates, computed, cached, capped, compute_failures)
}

/// Computes diff churn between two validated ref names or full oids via
/// `<base>...<tip>` (two git processes). Returns None when either side fails
/// validation or git rejects the walk. Identical oids are mathematically
/// zero — no subprocess is spawned.
fn compute_branch_churn(repo: &Path, base: &str, branch: &str) -> Option<ComputedBranchChurn> {
    if validate_ref_name(base).is_err() || validate_ref_name(branch).is_err() {
        return None;
    }
    if base == branch {
        return Some(ZERO_CHURN);
    }
    let spec = format!("{}...{}", base, branch);
    let shortstat = git_text(repo, &["diff", "--shortstat", &spec]).ok()?;
    let churn = DiffChurn::parse_shortstat(&shortstat);
    let counts = git_text(repo, &["rev-list", "--left-right", "--count", &spec]).ok()?;
    let (behind, ahead) = git_cli::parse_left_right_count(&counts);
    Some(ComputedBranchChurn {
        additions: churn.additions,
        deletions: churn.deletions,
        files_changed: churn.files_changed,
        commits_ahead: ahead,
        commits_behind: behind,
    })
}

/// Wraps a user-supplied path in `:(literal)` pathspec magic so glob
/// metacharacters in filenames (`*?[`) cannot widen the query beyond the
/// intended file. Applies to every path: a leading `:` would otherwise be
/// read as magic-pathspec syntax too.
fn literal_pathspec(path: &str) -> String {
    format!(":(literal){path}")
}

/// Decodes one `\x01`-separated history record laid out by
/// [`GitReader::read_commit_history_paged`] (`id, parents, timestamp,
/// subject, author name, author email`) into a commit node.
///
/// Parsing works from BOTH ends inward: `splitn(3)` pins the structurally
/// rigid prefix (hex id, whitespace parents, integer timestamp) and
/// `rsplitn(3)` peels the trailing author email and name, because \x01 is a
/// legal byte inside every variable-length field. Positional left-to-right
/// splitting let such a byte shift every later field — an email becoming a
/// fragment of a name, a timestamp collapsing to zero. Here a stray \x01 can
/// only degrade the field it landed in: one inside the subject rides in the
/// summary verbatim, and one inside the author name garbles at most that
/// name plus the summary text — never id, parents, timestamp or email.
///
/// Records whose id field is missing or blank are skipped (None) rather than
/// fabricated: an unidentifiable row would render as an unclickable ghost.
fn parse_history_record(record: &str) -> Option<RawCommitNode> {
    let mut head = record.splitn(3, '\x01');
    let id = head.next().map(str::trim).filter(|s| !s.is_empty())?;
    let parent_str = head.next().unwrap_or("");
    let parent_ids = if parent_str.is_empty() {
        Vec::new()
    } else {
        parent_str.split_whitespace().map(String::from).collect()
    };
    let core = head.next().unwrap_or("");
    // core = timestamp, then the subject, then author name, then email.
    let mut tail = core.rsplitn(3, '\x01');
    let author_email = tail.next().unwrap_or("").to_string();
    let author_name = tail.next().unwrap_or("").to_string();
    let mut stamp_and_summary = tail.next().unwrap_or("").splitn(2, '\x01');
    let timestamp = stamp_and_summary
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let summary = stamp_and_summary.next().unwrap_or("").to_string();
    Some(RawCommitNode {
        id: id.to_string(),
        parent_ids,
        timestamp,
        author_name,
        author_email,
        summary,
    })
}

/// Returns a synthesized git-shaped new-file diff when `file_path` is
/// untracked (`??` in porcelain status), or `None` when git should answer via
/// its own machinery.
///
/// `git diff -- :(literal)<path>` outputs NOTHING for untracked paths, so
/// clicking a brand-new file used to render a blank pane. The worktree bytes
/// go through the same capped, sandbox-validated read as every other entry
/// point (`sandbox_join_canonical` + [`check_working_tree_size`] at
/// [`MAX_WORKING_TREE_BYTES`]), and text content is rendered exactly like
/// `git diff` would: header lines, `@@ -0,0 +1,N @@`, one `+{line}` per line,
/// and `\ No newline at end of file` when the last byte is not a newline.
fn untracked_new_file_diff(repo: &Path, file_path: &str) -> Result<Option<String>, String> {
    let spec = literal_pathspec(file_path);
    let stdout = git_text(
        repo,
        &[
            "-c",
            "core.quotepath=off",
            "status",
            "--porcelain=v1",
            "-z",
            "--",
            spec.as_str(),
        ],
    )?;
    let untracked = parse_status_records(stdout.as_bytes())
        .iter()
        .any(|record| {
            record.index_status == '?' && record.work_status == '?' && record.path == file_path
        });
    if !untracked {
        return Ok(None);
    }

    // Same read discipline as get_file_blob's working-tree branch: validate
    // containment (resolving symlinks), cap on metadata BEFORE reading, then
    // read whole.
    let dest = sandbox_join_canonical(repo, file_path)?;
    check_working_tree_size(&dest, MAX_WORKING_TREE_BYTES)?;
    let bytes = std::fs::read(&dest).map_err(|e| format!("Failed to read from disk: {}", e))?;
    Ok(Some(render_new_file_diff(file_path, &bytes)))
}

/// Renders an untracked file as a unified new-file diff in git's output shape.
///
/// Binary payloads reuse the repository-wide heuristic
/// ([`LanguageDetector::looks_binary`]) and collapse to git's single-line
/// binary notice. An empty file keeps a zero-count hunk header and no body.
fn render_new_file_diff(path: &str, bytes: &[u8]) -> String {
    let mut out = format!("diff --git a/{path} b/{path}\nnew file mode 100644\n");
    if LanguageDetector::looks_binary(bytes) {
        out.push_str(&format!("Binary files /dev/null and b/{path} differ\n"));
        return out;
    }
    out.push_str("--- /dev/null\n");
    out.push_str(&format!("+++ b/{path}\n"));
    if bytes.is_empty() {
        out.push_str("@@ -0,0 +0,0 @@\n");
        return out;
    }
    // Split on '\n': a trailing terminator yields a final empty chunk that is
    // NOT a line; absence of a trailing terminator means the last chunk IS a
    // line but needs git's no-newline marker.
    let mut body = Vec::new();
    let mut chunks = bytes.split(|&b| b == b'\n').peekable();
    while let Some(chunk) = chunks.next() {
        if chunks.peek().is_none() && chunk.is_empty() {
            break;
        }
        body.push(format!("+{}", String::from_utf8_lossy(chunk)));
    }
    out.push_str(&format!("@@ -0,0 +1,{} @@\n", body.len()));
    for line in &body {
        out.push_str(line);
        out.push('\n');
    }
    if !bytes.ends_with(b"\n") {
        out.push_str("\\ No newline at end of file\n");
    }
    out
}

fn validate_oid(oid: &str) -> Result<(), String> {
    if oid.is_empty() || oid.len() > 64 || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid commit id".into());
    }
    Ok(())
}

/// Rejects files larger than `max_bytes` BEFORE their bytes are read.
///
/// Metadata-only check so an oversized file never enters memory at all.
fn check_working_tree_size(path: &Path, max_bytes: u64) -> Result<(), String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("Failed to stat '{}': {}", path.display(), e))?
        .len();
    if len > max_bytes {
        return Err(format!(
            "file exceeds the {} MB working-tree size limit",
            max_bytes / (1024 * 1024)
        ));
    }
    Ok(())
}

/// Pure parser for NUL-separated `ls-files -z` output.
///
/// Trims the trailing slash a submodule entry carries, drops empty records,
/// and fails closed per entry: anything equal to "." or "..", escaping via
/// "../" (leading or mid-path), or absolute-looking is skipped alone rather
/// than failing the whole listing — one suspicious record must not blank the
/// file explorer. Duplicates collapse through a set and the result is
/// byte-sorted so callers get deterministic order. No other normalization:
/// git always emits forward slashes, and entries cannot contain `\0` once
/// split on `\0`.
fn parse_ls_files_entries(raw: &str) -> Vec<String> {
    let mut unique: HashSet<&str> = HashSet::new();
    for entry in raw.split('\0') {
        let entry = entry.strip_suffix('/').unwrap_or(entry);
        if entry.is_empty()
            || entry == "."
            || entry == ".."
            || entry.starts_with("../")
            || entry.contains("/../")
            || entry.starts_with('/')
        {
            continue;
        }
        unique.insert(entry);
    }
    let mut files: Vec<String> = unique.into_iter().map(String::from).collect();
    files.sort();
    files
}

/// One `git status --porcelain=v1 -z` record decoded into typed fields.
#[derive(Debug)]
struct RawStatusRecord {
    index_status: char,
    work_status: char,
    /// Post-image path; for rename/copy records this is the NEW name.
    path: String,
    /// Pre-image path, present only for rename/copy records.
    old_path: Option<String>,
}

/// Reads one NUL-terminated field starting at `*cursor`, advancing the cursor
/// past the terminator. `None` marks a truncated stream.
fn read_nul_field(bytes: &[u8], cursor: &mut usize) -> Option<String> {
    let start = *cursor;
    let len = bytes[start..].iter().position(|&b| b == 0)?;
    *cursor = start + len + 1;
    Some(String::from_utf8_lossy(&bytes[start..start + len]).into_owned())
}

/// Parses `git status --porcelain=v1 -z` output.
///
/// Every record is `XY <path>\0`. Rename (`R`) AND copy (`C`) records carry
/// their pre-image as a SECOND NUL-terminated field laid out `<new>\0<old>\0`
/// — post-image first (verified empirically via `git mv` and staged-copy
/// status) — and both must be consumed exactly like worktree's
/// `count_status_entries` does, or every later record desyncs. `-z` never
/// renders the non-z `old -> new` arrow, so a tracked file literally named
/// "a -> b" cannot be misread as a rename. Records truncated mid-stream are
/// dropped whole rather than half-decoded.
fn parse_status_records(bytes: &[u8]) -> Vec<RawStatusRecord> {
    let mut records = Vec::new();
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        let index_status = bytes[i] as char;
        let work_status = bytes[i + 1] as char;
        i += 3; // XY plus separator
        let Some(path) = read_nul_field(bytes, &mut i) else {
            break;
        };
        if path.is_empty() {
            continue;
        }
        let mut old_path = None;
        if matches!(
            (index_status, work_status),
            ('R', _) | (_, 'R') | ('C', _) | (_, 'C')
        ) {
            match read_nul_field(bytes, &mut i) {
                Some(origin) => old_path = (!origin.is_empty()).then_some(origin),
                None => break,
            }
        }
        records.push(RawStatusRecord {
            index_status,
            work_status,
            path,
            old_path,
        });
    }
    records
}

/// Returns the commit oid when `line` opens a `--line-porcelain` header.
///
/// Oid length depends on the repository hash (SHA-1: 40, SHA-256: 64), so any
/// 32–64 character hex token qualifies; once a first header pins the expected
/// length, only tokens of exactly that length count, keeping unrelated hex
/// text from hijacking blame state.
fn blame_header_oid(line: &str, expected_len: Option<usize>) -> Option<&str> {
    let token = line.split_whitespace().next()?;
    let len = token.len();
    if !(32..=64).contains(&len) || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match expected_len {
        Some(expected) if expected != len => None,
        _ => Some(token),
    }
}

/// Parses `git blame --line-porcelain` output. See [`blame_header_oid`] for
/// how the commit oid is recognised across hash algorithms.
fn parse_blame_porcelain(stdout: &str) -> Vec<BlameLine> {
    let mut blame_lines = Vec::new();
    let mut current_sha = String::new();
    let mut current_author = String::new();
    let mut current_email = String::new();
    let mut current_time: i64 = 0;
    let mut line_no = 0usize;
    let mut oid_len: Option<usize> = None;

    for line in stdout.lines() {
        if let Some(content) = line.strip_prefix('\t') {
            line_no += 1;
            blame_lines.push(BlameLine {
                line_no,
                commit_id: current_sha.clone(),
                author_name: current_author.clone(),
                author_email: current_email.clone(),
                timestamp: current_time,
                content: content.to_string(),
            });
        } else if let Some(author) = line.strip_prefix("author ") {
            current_author = author.to_string();
        } else if let Some(mail) = line.strip_prefix("author-mail ") {
            current_email = mail.trim_matches(|c| c == '<' || c == '>').to_string();
        } else if let Some(time) = line.strip_prefix("author-time ") {
            current_time = time.parse().unwrap_or(0);
        } else if let Some(oid) = blame_header_oid(line, oid_len) {
            oid_len = Some(oid.len());
            current_sha = oid.to_string();
        }
    }
    blame_lines
}

/// True when `token` is a `git diff --numstat -z` header (`add\tdel\tpath`
/// or `add\tdel\t` with an empty path that introduces a rename pair).
fn is_numstat_header(token: &str) -> bool {
    let mut parts = token.splitn(3, '\t');
    let add = parts.next().unwrap_or("");
    let del = parts.next().unwrap_or("");
    (add.parse::<usize>().is_ok() || add == "-")
        && (del.parse::<usize>().is_ok() || del == "-")
        && parts.next().is_some()
}

/// Parses `git diff --numstat -z` output into a path → (add, del) map.
///
/// Records are NUL-terminated and paths are never C-quoted, so names
/// containing tabs, arrows or unicode survive intact. A rename emits
/// `add\tdel\t\0<old>\0<new>\0`: the empty path field signals that the
/// pre-image and post-image follow as complete NUL fields, pre-image first
/// (verified against git's actual output). Both names are keyed so lookups
/// by either side of the rename succeed.
#[cfg(test)]
fn parse_numstat(stdout: &str) -> HashMap<String, (usize, usize)> {
    parse_numstat_with_issues(stdout).churn
}

/// One `git diff --numstat -z` decode: churn keyed by path plus the records
/// whose numbers could not be trusted. Mirrors the coverage scanner's
/// skip/skip-reason pattern: a row that would silently read as 0±0 carries
/// the reason instead of laundering a broken diff into fact.
struct NumstatParse {
    churn: HashMap<String, (usize, usize)>,
    /// (path, reason) for every record whose add/del fields were not usable.
    /// Unattributable garbage (no recoverable path) cannot ride on any row
    /// and is dropped here — documented limitation, not hidden data.
    issues: Vec<(String, String)>,
}

fn parse_numstat_with_issues(stdout: &str) -> NumstatParse {
    let mut out = NumstatParse {
        churn: HashMap::new(),
        issues: Vec::new(),
    };
    let mut tokens = stdout.split('\0').peekable();
    while let Some(head) = tokens.next() {
        if head.is_empty() {
            continue;
        }
        let mut fields = head.splitn(3, '\t');
        let add_raw = fields.next().unwrap_or("");
        let del_raw = fields.next();
        let path_field = fields.next().unwrap_or("");
        // "-" is git's binary marker and legitimately decodes to 0±0; only
        // values that are neither a number nor "-" are untrustworthy.
        let trust = |raw: &str| raw == "-" || raw.parse::<usize>().is_ok();
        let add_ok = trust(add_raw);
        let del_ok = del_raw.map(trust).unwrap_or(false);
        if !del_ok {
            if let Some(reason) = issue_for(path_field, add_raw, del_raw) {
                out.issues.push(reason);
            }
            if del_raw.is_none() && path_field.is_empty() {
                continue;
            }
        }
        if !add_ok {
            if path_field.is_empty() {
                // Rename-shaped record with bad counts: attribute to both
                // names so whichever side renders gets the warning.
                if let Some(first) = tokens.next() {
                    out.issues.push((
                        first.to_string(),
                        format!("numstat record had unparseable change count {:?}", add_raw),
                    ));
                }
            } else {
                out.issues.push((
                    path_field.to_string(),
                    format!("numstat record had unparseable change count {:?}", add_raw),
                ));
            }
        }
        let add: usize = add_raw.parse().unwrap_or(0);
        let del: usize = del_raw.and_then(|d| d.parse().ok()).unwrap_or(0);
        if path_field.is_empty() {
            if let Some(first_path) = tokens.next().filter(|s| !s.is_empty()) {
                let is_rename = tokens
                    .peek()
                    .is_some_and(|next| !next.is_empty() && !is_numstat_header(next));
                if is_rename {
                    let second_path = tokens.next().unwrap();
                    out.churn.insert(first_path.to_string(), (add, del));
                    out.churn.insert(second_path.to_string(), (add, del));
                } else {
                    out.churn.insert(first_path.to_string(), (add, del));
                }
            }
        } else {
            out.churn.insert(path_field.to_string(), (add, del));
        }
    }
    out
}

/// Shapes one untrustworthy numstat record into a row-attributable warning,
/// or None when no path survives to attach it to.
fn issue_for(path_field: &str, add_raw: &str, del_raw: Option<&str>) -> Option<(String, String)> {
    if !path_field.is_empty() {
        Some((
            path_field.to_string(),
            format!(
                "numstat record had unparseable change counts ({}, {})",
                add_raw,
                del_raw.unwrap_or("<missing>")
            ),
        ))
    } else {
        None
    }
}

/// Same wire format as [`parse_numstat`], but every record becomes a
/// [`CommitFileChange`]; renames are reported once under their post-image
/// path with status "R", binary files keep status "B".
fn parse_numstat_files(stdout: &str) -> Vec<CommitFileChange> {
    let mut files = Vec::new();
    let mut tokens = stdout.split('\0').peekable();
    while let Some(head) = tokens.next() {
        if head.is_empty() {
            continue;
        }
        let mut fields = head.splitn(3, '\t');
        let add_s = fields.next().unwrap_or("");
        let del_s = match fields.next() {
            Some(d) => d,
            None => continue,
        };
        let additions = add_s.parse().unwrap_or(0);
        let deletions = del_s.parse().unwrap_or(0);
        let binary = add_s == "-" && del_s == "-";
        let path_field = fields.next().unwrap_or("");
        if path_field.is_empty() {
            if let Some(first_path) = tokens.next().filter(|s| !s.is_empty()) {
                let is_rename = tokens
                    .peek()
                    .is_some_and(|next| !next.is_empty() && !is_numstat_header(next));
                if is_rename {
                    let second_path = tokens.next().unwrap();
                    files.push(CommitFileChange {
                        path: second_path.to_string(),
                        status_code: "R".to_string(),
                        additions,
                        deletions,
                    });
                } else {
                    let status_code = if binary { "B" } else { "M" };
                    files.push(CommitFileChange {
                        path: first_path.to_string(),
                        status_code: status_code.to_string(),
                        additions,
                        deletions,
                    });
                }
            }
        } else {
            let status_code = if binary { "B" } else { "M" };
            files.push(CommitFileChange {
                path: path_field.to_string(),
                status_code: status_code.to_string(),
                additions,
                deletions,
            });
        }
    }
    files
}

fn parse_co_authors(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("Co-authored-by:")
                .map(|rest| rest.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn record_lang(
    lang_counts: &mut HashMap<&'static str, (usize, usize, &'static str, &'static str)>,
    lang_info: LanguageInfo,
    code_lines: usize,
) {
    let entry = lang_counts.entry(lang_info.name).or_insert((
        0,
        0,
        lang_info.color_hex,
        lang_info.category,
    ));
    entry.0 += code_lines;
    entry.1 += 1;
}

fn mime_from_path(path: &str) -> String {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn b64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let capacity = 4 * bytes.len().div_ceil(3);
    let mut out = String::with_capacity(capacity);
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        let triple = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(TABLE[((triple >> 18) & 63) as usize] as char);
        out.push(TABLE[((triple >> 12) & 63) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(TABLE[((triple >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < bytes.len() {
            out.push(TABLE[(triple & 63) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_stats_repo(dir: &std::path::Path) {
        let output = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .output()
            .expect("git init");
        assert!(output.status.success());
        let rs = dir.join("app.rs");
        std::fs::write(&rs, "fn main() {}\n").expect("write file");
        let output = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .expect("git add");
        assert!(output.status.success());
    }

    /// The language scan must report honestly when it stopped early: a
    /// zero deadline reads nothing, and the report must say truncated
    /// rather than presenting an empty scan as complete coverage.
    #[test]
    fn language_stats_report_truncates_honestly_under_a_zero_deadline() {
        let dir = tempfile::TempDir::new().unwrap();
        init_stats_repo(dir.path());
        let report = GitReader::get_repo_language_stats_bounded(
            dir.path().to_str().unwrap(),
            Some(Duration::ZERO),
        )
        .expect("report");
        assert_eq!(report.scanned_files, 0);
        assert!(
            report.candidate_files >= 1,
            "the seeded .rs file is a candidate"
        );
        assert!(report.truncated, "a scan that read nothing is truncated");
    }

    #[test]
    fn language_stats_report_is_complete_on_a_small_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        init_stats_repo(dir.path());
        let report = GitReader::get_repo_language_stats_bounded(
            dir.path().to_str().unwrap(),
            Some(Duration::from_secs(30)),
        )
        .expect("report");
        assert!(!report.truncated);
        assert_eq!(report.scanned_files, 1);
        assert_eq!(report.candidate_files, 1);
        assert_eq!(report.stats.len(), 1);
        assert_eq!(report.stats[0].language, "Rust");
    }

    #[test]
    fn test_parse_co_authors() {
        let body = "Implements login.\n\nCo-authored-by: Bob <bob@example.com>\n";
        assert_eq!(
            parse_co_authors(body),
            vec!["Bob <bob@example.com>".to_string()]
        );
    }

    /// A clean record round-trips every field positionally.
    #[test]
    fn parse_history_record_parses_a_clean_record() {
        // Field order per the format string: id, parents, timestamp,
        // subject, author name, author email.
        let node = parse_history_record(
            "abc123\x01p1 p2\x011700000000\x01feat: thing\x01Ada\x01ada@example.com",
        )
        .expect("clean record");
        assert_eq!(node.id, "abc123");
        assert_eq!(node.parent_ids, vec!["p1".to_string(), "p2".to_string()]);
        assert_eq!(node.timestamp, 1700000000);
        assert_eq!(node.author_name, "Ada");
        assert_eq!(node.author_email, "ada@example.com");
        assert_eq!(node.summary, "feat: thing");

        let root = parse_history_record("def456\x01\x01\x01\x01\x01").expect("empty-fields record");
        assert_eq!(root.parent_ids, Vec::<String>::new());
        assert_eq!(root.timestamp, 0);
        assert!(root.author_name.is_empty() && root.author_email.is_empty());
    }

    /// Records without an identifiable id are skipped whole, never fabricated.
    #[test]
    fn parse_history_record_rejects_records_without_an_id() {
        assert!(parse_history_record("").is_none());
        assert!(parse_history_record("   \x01p1\x011\x01a\x01b\x01c").is_none());
    }

    /// Regression (\x01-safe field framing): \x01 is legal inside an author
    /// name, so the risky variable-length fields sit at the END of the record
    /// and are parsed from the RIGHT. Id, parents and timestamp are then
    /// structurally immune, and the email — the final field — always wins;
    /// a stray \x01 can only truncate the single field it landed in.
    #[test]
    fn parse_history_record_survives_0x01_inside_author_name() {
        let record = "abc123\x01p1 p2\x011700000000\x01feat: thing\x01Ev\x01il\x01n@e.com";
        let node = parse_history_record(record).expect("hostile-author record");
        assert_eq!(node.id, "abc123");
        assert_eq!(node.parent_ids, vec!["p1".to_string(), "p2".to_string()]);
        assert_eq!(
            node.timestamp, 1700000000,
            "timestamp must be structurally immune to later-field corruption"
        );
        assert_eq!(
            node.author_email, "n@e.com",
            "the email is the record's last field and must not shift"
        );
    }

    /// A \x01 inside the subject rides INSIDE the summary verbatim (an
    /// improvement over the old positional parse, which truncated it);
    /// the name and email parsed from the right must stay intact.
    #[test]
    fn parse_history_record_keeps_hostile_summary_intact() {
        let record = "abc124\x01\x011700000001\x01feat: half\x01 embedded\x01Full Name\x01n@e.com";
        let node = parse_history_record(record).expect("hostile-subject record");
        assert_eq!(node.timestamp, 1700000001);
        assert_eq!(node.author_name, "Full Name");
        assert_eq!(node.author_email, "n@e.com");
        assert_eq!(node.summary, "feat: half\x01 embedded");
    }

    /// Regression (probe at the ceiling): the `-n` bound must admit MAX+1 so
    /// cmd_get_commit_graph's has_more probe row survives the clamp — the
    /// probe arrives as exactly MAX_HISTORY_COMMITS+1 when the user's cap is
    /// the ceiling, and a clamp back down to MAX swallowed it, making such
    /// repositories report has_more=false forever while silently truncating.
    #[test]
    fn page_count_limit_admits_the_has_more_probe_row_at_the_ceiling() {
        let max = GitReader::MAX_HISTORY_COMMITS;
        // An ordinary ceiling-sized request is unchanged...
        assert_eq!(GitReader::page_count_limit(max), max);
        // ...but the probe row arrives as MAX+1 and must survive the clamp,
        // while larger overshoots still collapse onto that bound.
        assert_eq!(
            GitReader::page_count_limit(max + 1),
            max + 1,
            "the probe row must survive the clamp at exactly the ceiling"
        );
        assert_eq!(GitReader::page_count_limit(max + 50), max + 1);
        assert_eq!(GitReader::page_count_limit(0), 1);
    }

    /// Audit C2: every extension the frontend's image path list accepts must
    /// map to a proper image/* MIME type, including .bmp.
    #[test]
    fn mime_from_path_maps_known_image_extensions() {
        let cases = [
            ("a.png", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("a.svg", "image/svg+xml"),
            ("a.bmp", "image/bmp"),
        ];
        for (path, want) in cases {
            assert_eq!(mime_from_path(path), want, "{path}");
        }
    }

    /// Audit C2: extension case must not change the mapping (uppercase and
    /// mixed-case names are common from cameras and Windows exports).
    #[test]
    fn mime_from_path_is_case_insensitive() {
        let cases = [
            ("photo.PNG", "image/png"),
            ("IMG.JPG", "image/jpeg"),
            ("scan.BMP", "image/bmp"),
            ("icon.WebP", "image/webp"),
            ("art.SVG", "image/svg+xml"),
        ];
        for (path, want) in cases {
            assert_eq!(mime_from_path(path), want, "{path}");
        }
    }

    /// Unknown or missing extensions stay generic binary.
    #[test]
    fn mime_from_path_unknown_falls_back_to_octet_stream() {
        for path in ["a.exe", "notes.txt", "noext", "archive.tar.gz2"] {
            assert_eq!(mime_from_path(path), "application/octet-stream", "{path}");
        }
    }

    #[test]
    fn test_validate_oid_rejects_injection() {
        assert!(validate_oid("abc123").is_ok());
        assert!(validate_oid("--output=/tmp/x").is_err());
        assert!(validate_oid("HEAD;rm").is_err());
    }

    #[test]
    fn test_pick_default_branch() {
        let names = vec!["develop".into(), "main".into(), "feat/a".into()];
        assert_eq!(
            pick_default_branch(&names, Some("refs/remotes/origin/main"), "origin"),
            "main"
        );
        assert_eq!(
            pick_default_branch(&names, Some("refs/remotes/upstream/develop"), "upstream"),
            "develop"
        );
        let master_only = vec!["master".into(), "hotfix".into()];
        assert_eq!(pick_default_branch(&master_only, None, "origin"), "master");
        let other = vec!["trunk".into()];
        assert_eq!(pick_default_branch(&other, None, "gitlab"), "trunk");
    }

    #[test]
    fn test_remote_head_ref_assembly() {
        assert_eq!(remote_head_ref("origin"), "refs/remotes/origin/HEAD");
        assert_eq!(remote_head_ref("company"), "refs/remotes/company/HEAD");
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test User")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test User")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo_with_remotes(remotes: &[&str], default_branch_name: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", default_branch_name]);
        git_in(dir.path(), &["config", "user.email", "t@example.com"]);
        git_in(dir.path(), &["config", "user.name", "T"]);
        git_in(dir.path(), &["commit", "--allow-empty", "-m", "seed"]);
        for remote in remotes {
            git_in(
                dir.path(),
                &[
                    "remote",
                    "add",
                    remote,
                    &format!("https://example.com/{remote}.git"),
                ],
            );
        }
        dir
    }

    #[test]
    fn resolve_default_remote_prefers_checkout_config() {
        let dir = init_repo_with_remotes(&["alpha", "beta"], "release");
        git_in(dir.path(), &["config", "checkout.defaultRemote", "beta"]);
        assert_eq!(resolve_default_remote(dir.path()), "beta");
    }

    #[test]
    fn resolve_default_remote_falls_back_to_current_branch_upstream() {
        let dir = init_repo_with_remotes(&["alpha", "beta"], "release");
        git_in(dir.path(), &["config", "branch.release.remote", "beta"]);
        assert_eq!(resolve_default_remote(dir.path()), "beta");
    }

    #[test]
    fn resolve_default_remote_uses_lone_remote_without_config() {
        let dir = init_repo_with_remotes(&["company"], "release");
        assert_eq!(resolve_default_remote(dir.path()), "company");
    }

    #[test]
    fn resolve_default_remote_defaults_to_origin_when_ambiguous() {
        let dir = init_repo_with_remotes(&["alpha", "beta"], "release");
        assert_eq!(resolve_default_remote(dir.path()), "origin");

        let empty = init_repo_with_remotes(&[], "release");
        assert_eq!(resolve_default_remote(empty.path()), "origin");
    }

    #[test]
    fn resolve_default_remote_skips_branch_upstream_when_detached() {
        let dir = init_repo_with_remotes(&["alpha", "beta"], "release");
        git_in(dir.path(), &["config", "branch.release.remote", "beta"]);
        git_in(dir.path(), &["checkout", "--detach"]);
        // Detached HEAD cannot name a branch upstream; ambiguity falls back.
        assert_eq!(resolve_default_remote(dir.path()), "origin");
    }

    /// Wire format verified against real git: a `-z` numstat rename record is
    /// `add\tdel\t\0<old>\0<new>\0` — the empty path field announces that the
    /// pre-image and post-image follow as complete NUL fields.
    #[test]
    fn parse_numstat_keys_rename_by_both_paths() {
        let raw = "3\t1\t\0src/old.rs\0src/new.rs\0-\t-\tbin.dat\0";
        let map = parse_numstat(raw);
        assert_eq!(map.get("src/new.rs"), Some(&(3, 1)));
        assert_eq!(map.get("src/old.rs"), Some(&(3, 1)));
        assert_eq!(map.get("bin.dat"), Some(&(0, 0)));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn parse_numstat_plain_records_and_empty_input() {
        assert!(parse_numstat("").is_empty());
        let map = parse_numstat("12\t4\ta.txt\x005\t5\tb.txt\0");
        assert_eq!(map.get("a.txt"), Some(&(12, 4)));
        assert_eq!(map.get("b.txt"), Some(&(5, 5)));
    }

    #[test]
    fn parse_numstat_files_reports_renames_under_new_path_only() {
        let files = parse_numstat_files("2\t0\t\0old.txt\0new.txt\0-\t-\timg.png\0");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "new.txt");
        assert_eq!(files[0].status_code, "R");
        assert_eq!((files[0].additions, files[0].deletions), (2, 0));
        assert_eq!(files[1].path, "img.png");
        assert_eq!(files[1].status_code, "B");
    }

    #[test]
    fn list_tags_caps_payload_at_tag_list_cap() {
        use std::io::Write;
        let dir = init_repo_with_remotes(&[], "main");
        git_in(dir.path(), &["commit", "--allow-empty", "-m", "second"]);
        let head = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir.path())
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let head = head.trim().to_string();
        // Batch-create more than the cap in one process; lightweight tags are
        // plain refs so update-ref is equivalent to `git tag` here.
        let mut stdin_cmd = std::process::Command::new("git")
            .args(["update-ref", "--stdin"])
            .current_dir(dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn update-ref");
        {
            let stdin = stdin_cmd.stdin.as_mut().expect("stdin pipe");
            for i in 0..TAG_LIST_CAP + 7 {
                writeln!(stdin, "create refs/tags/bulk-{i:04} {head}").unwrap();
            }
        }
        assert!(stdin_cmd.wait().unwrap().success());
        drop(stdin_cmd);

        let tags = GitReader::list_tags(&dir.path().to_string_lossy()).expect("tags");
        assert_eq!(tags.len(), TAG_LIST_CAP, "payload must be capped");
        assert!(tags.iter().all(|t| t.name.starts_with("bulk-")));
    }

    #[test]
    fn test_parse_ahead_behind_field() {
        // Well-formed `%(ahead-behind:<oid>)` output.
        assert_eq!(parse_ahead_behind_field("3 12"), (3, 12));
        assert_eq!(parse_ahead_behind_field("0 0"), (0, 0));
        // Trailing field missing / empty / padded.
        assert_eq!(parse_ahead_behind_field("7"), (7, 0));
        assert_eq!(parse_ahead_behind_field(""), (0, 0));
        assert_eq!(parse_ahead_behind_field(" 4   9 "), (4, 9));
        // Malformed values degrade to zeros, never poison the listing.
        assert_eq!(parse_ahead_behind_field("junk data"), (0, 0));
        assert_eq!(parse_ahead_behind_field("-1 2"), (0, 2));
    }

    #[test]
    fn test_churn_cache_evicts_oldest_and_refreshes_on_reinsert() {
        let churn = |additions| ComputedBranchChurn {
            additions,
            deletions: 0,
            files_changed: 0,
            commits_ahead: 0,
            commits_behind: 0,
        };
        let key = |name: &str| (name.to_string(), "base".to_string(), "tip".to_string());
        let mut cache = ChurnCache::new();
        cache.capacity = 2;

        cache.insert(key("a"), churn(1));
        cache.insert(key("b"), churn(2));
        cache.insert(key("b"), churn(22)); // re-insert refreshes recency
        cache.insert(key("c"), churn(3)); // evicts "a"
        assert!(cache.get(&key("a")).is_none());
        assert_eq!(cache.get(&key("b")).map(|c| c.additions), Some(22));
        assert_eq!(cache.get(&key("c")).map(|c| c.additions), Some(3));

        cache.insert(key("d"), churn(4)); // evicts "b" (oldest after refresh)
        assert!(cache.get(&key("b")).is_none());
        assert!(cache.get(&key("c")).is_some());
        assert!(cache.get(&key("d")).is_some());
    }

    #[test]
    fn test_churn_cache_reinsert_stays_bounded() {
        let mut cache = ChurnCache::new();
        cache.capacity = 4;
        let key = |i: usize| (format!("r{i}"), "base".to_string(), "tip".to_string());
        for i in 0..10_000 {
            cache.insert(key(i % 3), ZERO_CHURN);
        }
        assert!(cache.entries.len() <= 4);
        assert!(
            cache.order.len() <= 8,
            "order grew to {}",
            cache.order.len()
        );

        let mut full = ChurnCache::new();
        for i in 0..CHURN_CACHE_CAPACITY + 400 {
            full.insert((format!("repo-{i}"), "b".into(), "t".into()), ZERO_CHURN);
        }
        assert_eq!(full.entries.len(), CHURN_CACHE_CAPACITY);
        assert!(full.order.len() <= CHURN_CACHE_CAPACITY * 2);
    }

    #[test]
    fn test_strip_remote_prefix_and_tracking() {
        assert_eq!(
            strip_remote_prefix("origin/feat/a", Some("origin")),
            "feat/a"
        );
        assert_eq!(strip_remote_prefix("origin/feat/a", None), "origin/feat/a");
        assert_eq!(strip_remote_prefix("main", Some("origin")), "main");
        assert_eq!(
            strip_remote_prefix("originate/x", Some("origin")),
            "originate/x"
        );
        assert_eq!(
            strip_remote_tracking("refs/remotes/origin/main", "origin"),
            Some("main")
        );
        assert_eq!(
            strip_remote_tracking("refs/remotes/origin/feat/a", "origin"),
            Some("feat/a")
        );
        assert_eq!(
            strip_remote_tracking("refs/remotes/upstream/main", "origin"),
            None
        );
    }

    fn rev_parse_at(dir: &Path, rev: &str) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", rev])
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn resolve_default_base_on_uses_one_for_each_ref() {
        let dir = init_repo_with_remotes(&[], "main");
        let resolved = resolve_default_base_on(dir.path(), "origin", None);
        assert_eq!(resolved.as_ref().map(|(n, _)| n.as_str()), Some("main"));
        assert_eq!(resolved.unwrap().1, rev_parse_at(dir.path(), "HEAD"));
    }

    #[test]
    fn identical_tips_share_compute_and_base_equals_tip_is_zero() {
        use std::io::Write;
        let dir = init_repo_with_remotes(&[], "main");
        std::fs::write(dir.path().join("base.txt"), "one\n").unwrap();
        git_in(dir.path(), &["add", "base.txt"]);
        git_in(dir.path(), &["commit", "-m", "base"]);
        let main_oid = rev_parse_at(dir.path(), "HEAD");

        git_in(dir.path(), &["checkout", "-q", "-b", "topic"]);
        std::fs::write(dir.path().join("feat.txt"), "a\nb\nc\n").unwrap();
        git_in(dir.path(), &["add", "feat.txt"]);
        git_in(dir.path(), &["commit", "-m", "topic"]);
        let topic_oid = rev_parse_at(dir.path(), "HEAD");
        git_in(dir.path(), &["checkout", "-q", "main"]);

        let mut stdin_cmd = std::process::Command::new("git")
            .args(["update-ref", "--stdin"])
            .current_dir(dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn update-ref");
        {
            let mut stdin = stdin_cmd.stdin.take().expect("stdin");
            for i in 0..16 {
                writeln!(stdin, "create refs/heads/shared-{i:02} {topic_oid}").unwrap();
            }
            for i in 0..8 {
                writeln!(stdin, "create refs/heads/pointer-{i:02} {main_oid}").unwrap();
            }
        }
        assert!(stdin_cmd.wait().unwrap().success());

        let path = dir.path().to_str().unwrap();
        let first = GitReader::branch_stats(path).expect("stats 1");
        assert!(!first.capped);
        assert_eq!(first.updates.len(), 16 + 8 + 1); // shared + pointer + topic
        assert_eq!(first.computed, first.updates.len());
        assert_eq!(first.cached, 0);
        let topic_churn = first
            .updates
            .iter()
            .find(|u| u.name == "topic")
            .expect("topic");
        assert_eq!(topic_churn.files_changed, 1);
        assert!(topic_churn.additions > 0);
        for i in 0..16 {
            let u = first
                .updates
                .iter()
                .find(|u| u.name == format!("shared-{i:02}"))
                .unwrap_or_else(|| panic!("shared-{i:02}"));
            assert_eq!(u.tip_commit_id, topic_oid);
            assert_eq!(u.additions, topic_churn.additions);
            assert_eq!(u.files_changed, 1);
        }
        for i in 0..8 {
            let u = first
                .updates
                .iter()
                .find(|u| u.name == format!("pointer-{i:02}"))
                .unwrap_or_else(|| panic!("pointer-{i:02}"));
            assert_eq!(u.tip_commit_id, main_oid);
            assert_eq!(
                (u.additions, u.deletions, u.commits_ahead_of_base),
                (0, 0, 0)
            );
        }

        let second = GitReader::branch_stats(path).expect("stats 2");
        assert_eq!(second.computed, 0);
        assert_eq!(second.cached, first.updates.len());
        assert_eq!(
            serde_json::to_string(&first.updates).unwrap(),
            serde_json::to_string(&second.updates).unwrap()
        );
    }

    #[test]
    fn test_b64_encode_padding() {
        assert_eq!(b64_encode(b"Man"), "TWFu");
        assert_eq!(b64_encode(b"Ma"), "TWE=");
        assert_eq!(b64_encode(b"M"), "TQ==");
    }

    #[test]
    fn test_working_tree_size_check_rejects_oversize() {
        let dir = tempfile::TempDir::new().unwrap();
        let small = dir.path().join("small.txt");
        std::fs::write(&small, b"ok").unwrap();
        assert!(check_working_tree_size(&small, 4).is_ok());
        assert_eq!(
            check_working_tree_size(&small, 1).unwrap_err(),
            "file exceeds the 0 MB working-tree size limit"
        );

        // Sparse file: metadata reports the oversized length without the
        // bytes ever existing on disk, so this stays cheap.
        let sparse = dir.path().join("sparse.bin");
        std::fs::File::create(&sparse)
            .unwrap()
            .set_len(MAX_WORKING_TREE_BYTES + 1)
            .unwrap();
        let err = check_working_tree_size(&sparse, MAX_WORKING_TREE_BYTES).unwrap_err();
        assert!(err.contains("working-tree size limit"), "got: {err}");
    }

    #[test]
    fn test_get_file_blob_refuses_oversize_working_tree_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .output()
            .expect("spawn git init");
        assert!(output.status.success());

        // Sparse 64 MiB+1 file: get_file_blob must reject on metadata before
        // attempting the (never-materialized) read.
        let big = dir.path().join("big.bin");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_WORKING_TREE_BYTES + 1)
            .unwrap();
        let err = GitReader::get_file_blob(&dir.path().to_string_lossy(), "big.bin", None)
            .expect_err("oversized working-tree file");
        assert!(err.contains("working-tree size limit"), "got: {err}");
    }

    /// Regression (M1+M11): porcelain v1 `-z` lays rename/copy records out as
    /// `XY <new>\0<old>\0` — post-image FIRST — and the paired field must be
    /// consumed for both letters or the parse cursor desyncs.
    #[test]
    fn test_parse_status_records_orders_rename_and_copy_fields() {
        let raw = b"M  kept.txt\0\
                     ?? draft.txt\0\
                     R  renamed-new.txt\0renamed-old.txt\0\
                     C  copy-new.txt\0copy-old.txt\0";
        let records = parse_status_records(raw);
        assert_eq!(records.len(), 4, "every record must decode: {records:?}");

        assert_eq!(records[2].index_status, 'R');
        assert_eq!(records[2].path, "renamed-new.txt");
        assert_eq!(records[2].old_path.as_deref(), Some("renamed-old.txt"));

        assert_eq!(records[3].index_status, 'C');
        assert_eq!(records[3].path, "copy-new.txt");
        assert_eq!(records[3].old_path.as_deref(), Some("copy-old.txt"));
    }

    #[test]
    fn test_parse_status_records_survives_arrow_in_filename() {
        // "-z" never emits the " -> " arrow; a tracked file literally named
        // "a -> b" is an ordinary modify, not a rename record.
        let records = parse_status_records(b"M  a -> b\0M  after.txt\0");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, "a -> b");
        assert_eq!(records[0].old_path, None);
        assert_eq!(records[1].path, "after.txt");
    }

    #[test]
    fn test_parse_status_records_drops_truncated_tail() {
        // Rename missing its second NUL field: dropped whole, earlier
        // records intact, no bogus entry synthesized from garbage.
        let records = parse_status_records(b"M  ok.txt\0R  new-only\0");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, "ok.txt");

        assert!(parse_status_records(b"").is_empty());
        assert!(parse_status_records(b"M ").is_empty());
    }

    #[test]
    fn test_parse_numstat_keys_rename_on_post_image() {
        // Verified wire shapes: `add\tdel\t<path>\0` and, for renames,
        // `add\tdel\t\0<old>\0<new>\0`.
        let map = parse_numstat("3\t1\tplain.txt\0");
        assert_eq!(map.get("plain.txt"), Some(&(3, 1)));

        let map = parse_numstat("1\t0\t\0src/old-name.txt\0src/new-name.txt\0");
        assert_eq!(
            map.get("src/new-name.txt"),
            Some(&(1, 0)),
            "post-image path must be keyed"
        );
        assert_eq!(map.get("src/old-name.txt"), Some(&(1, 0)));

        let map = parse_numstat("-\t-\tbin.dat\0");
        assert_eq!(map.get("bin.dat"), Some(&(0, 0)));
    }

    #[test]
    fn test_parse_numstat_files_reports_renames_under_new_name() {
        let files =
            parse_numstat_files("1\t0\t\0src/old.rs\0src/new.rs\0-\t-\timg.png\x002\t1\tkept.rs\0");
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "src/new.rs");
        assert_eq!(files[0].status_code, "R");
        assert_eq!(files[1].status_code, "B");
        assert_eq!(files[2].path, "kept.rs");
        assert_eq!(files[2].status_code, "M");
    }

    /// Regression (silent-failure surfacing): a numstat record whose change
    /// counts are not numbers used to decode into an unremarkable 0±0 row.
    /// The zeros still render (the row exists), but the path must now carry
    /// the reason so the UI can stop presenting fabricated churn as fact.
    #[test]
    fn parse_numstat_flags_unparseable_counts_instead_of_silent_zeros() {
        let parsed =
            parse_numstat_with_issues(concat!("junk\t5\tweird.txt\0", "3\tNaN\tother.txt\0"));
        // Old behavior preserved: zeros for the broken fields.
        assert_eq!(parsed.churn.get("weird.txt"), Some(&(0, 5)));
        assert_eq!(parsed.churn.get("other.txt"), Some(&(3, 0)));
        // New: both rows are flagged with the offending input named.
        let flagged: Vec<&str> = parsed.issues.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(flagged.len(), 2, "both bad records must be flagged");
        assert!(flagged.contains(&"weird.txt"));
        assert!(flagged.contains(&"other.txt"));

        let (_, reason) = parsed
            .issues
            .iter()
            .find(|(p, _)| p == "other.txt")
            .expect("issue recorded");
        assert!(
            reason.contains("unparseable"),
            "warning must say why, got: {reason}"
        );
    }

    /// Binary ("-") markers and healthy records must NOT be flagged: the
    /// warning exists to catch corruption, not to cry wolf on legal output.
    #[test]
    fn parse_numstat_does_not_flag_binary_or_healthy_records() {
        let parsed = parse_numstat_with_issues(concat!(
            "-\t-\tbin.dat\0",
            "7\t2\tclean.txt\0",
            "1\t0\t\0a\0b\0",
        ));
        assert!(parsed.issues.is_empty(), "got: {:?}", parsed.issues);
        assert_eq!(parsed.churn.len(), 4);
    }

    /// The additive FileStatus warnings field must be invisible in JSON while
    /// empty (existing JS consumers see the exact old shape) and present when
    /// populated.
    #[test]
    fn file_status_warnings_are_absent_from_json_while_empty() {
        let clean = FileStatus {
            path: "a.txt".into(),
            old_path: None,
            status_code: "M ".into(),
            is_staged: true,
            is_conflicted: false,
            additions: 1,
            deletions: 2,
            warnings: Vec::new(),
        };
        let value = serde_json::to_value(&clean).unwrap();
        assert!(
            value.get("warnings").is_none(),
            "empty warnings must not appear on the wire: {value}"
        );

        let warned = FileStatus {
            warnings: vec!["numstat record had unparseable counts".into()],
            ..clean
        };
        let value = serde_json::to_value(&warned).unwrap();
        assert_eq!(
            value["warnings"],
            serde_json::json!(["numstat record had unparseable counts"])
        );
        // And it round-trips through deserialization (serde default) so older
        // persisted payloads still load.
        let back: FileStatus = serde_json::from_value(serde_json::json!({
            "path": "b.txt", "old_path": null, "status_code": "??",
            "is_staged": false, "is_conflicted": false,
            "additions": 0, "deletions": 0
        }))
        .unwrap();
        assert!(back.warnings.is_empty());
    }

    fn blame_block(oid: &str, content: &str) -> String {
        format!(
            "{oid} 1 1 1\n\
             author T\n\
             author-mail <t@example.com>\n\
             author-time 1700000000\n\
             author-tz +0000\n\
             committer T\n\
             committer-mail <t@example.com>\n\
             committer-time 1700000000\n\
             committer-tz +0000\n\
             summary s\n\
             filename f.txt\n\
             \t{content}\n"
        )
    }

    /// Regression (m2): header detection hardcoded SHA-1's 40-char oid and
    /// broke SHA-256 repositories. Length must be learned from the stream.
    #[test]
    fn test_blame_porcelain_accepts_sha256_oid_length() {
        let oid64 = "b".repeat(64);
        let lines = parse_blame_porcelain(&format!(
            "{}{}",
            blame_block(&oid64, "alpha"),
            blame_block(&oid64, "beta")
        ));
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.commit_id.len() == 64));
        assert_eq!(lines[0].line_no, 1);
        assert_eq!(lines[0].content, "alpha");
        assert_eq!(lines[0].author_email, "t@example.com");

        let oid40 = "a".repeat(40);
        let lines = parse_blame_porcelain(&blame_block(&oid40, "gamma"));
        assert_eq!(lines[0].commit_id.len(), 40);
    }

    #[test]
    fn test_blame_porcelain_pins_oid_length_after_first_header() {
        let oid64 = "b".repeat(64);
        let mut stream = blame_block(&oid64, "real");
        // A 40-char token arriving after the length was pinned to 64 must not
        // hijack blame attribution for later lines.
        stream.push_str(&format!("{} 9 9 1\n", "c".repeat(40)));
        stream.push_str("\thijack probe\n");
        let lines = parse_blame_porcelain(&stream);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[1].commit_id.len(),
            64,
            "pinned length must reject mismatched oids"
        );
        assert_eq!(lines[1].content, "hijack probe");
    }

    #[test]
    fn test_literal_pathspec_always_prefixes_magic() {
        assert_eq!(literal_pathspec("weird*.txt"), ":(literal)weird*.txt");
        assert_eq!(literal_pathspec(":3:lockfile"), ":(literal):3:lockfile");
        assert_eq!(literal_pathspec("plain.txt"), ":(literal)plain.txt");
    }

    // -------------------------------------------------------------------------
    // Untracked-file diff synthesis (audit B)
    // -------------------------------------------------------------------------

    #[test]
    fn render_new_file_diff_matches_git_shape_for_text() {
        let rendered = render_new_file_diff("src/new.rs", b"fn a() {}\nfn b() {}\n");
        assert_eq!(
            rendered,
            concat!(
                "diff --git a/src/new.rs b/src/new.rs\n",
                "new file mode 100644\n",
                "--- /dev/null\n",
                "+++ b/src/new.rs\n",
                "@@ -0,0 +1,2 @@\n",
                "+fn a() {}\n",
                "+fn b() {}\n",
            )
        );
    }

    #[test]
    fn render_new_file_diff_marks_missing_trailing_newline() {
        let rendered = render_new_file_diff("note.txt", b"alpha\nbeta");
        assert!(rendered.contains("+beta\n\\ No newline at end of file\n"));
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(
            lines.last().copied(),
            Some("\\ No newline at end of file"),
            "marker must be the final line"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                .count(),
            2,
            "one + line per content line"
        );
    }

    #[test]
    fn render_new_file_diff_empty_file_keeps_zero_count_hunk() {
        assert_eq!(
            render_new_file_diff("empty.txt", b""),
            concat!(
                "diff --git a/empty.txt b/empty.txt\n",
                "new file mode 100644\n",
                "--- /dev/null\n",
                "+++ b/empty.txt\n",
                "@@ -0,0 +0,0 @@\n",
            )
        );
    }

    #[test]
    fn render_new_file_diff_binary_uses_single_line_notice() {
        let rendered = render_new_file_diff("img.bin", &[0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF]);
        assert_eq!(
            rendered,
            concat!(
                "diff --git a/img.bin b/img.bin\n",
                "new file mode 100644\n",
                "Binary files /dev/null and b/img.bin differ\n",
            )
        );
        assert!(!rendered.contains("@@"), "binary notice carries no hunks");
    }

    /// Regression (audit B): clicking an untracked file used to return empty
    /// output because `git diff` ignores untracked paths. Text, no-trailing-
    /// newline, binary and empty variants must each synthesize; tracked
    /// files must keep going through real git.
    #[test]
    fn get_file_diff_synthesizes_untracked_new_file() {
        use std::fs;
        let dir = init_repo_with_remotes(&[], "main");
        fs::write(dir.path().join("tracked.txt"), "old\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "seed"]);

        // Untracked text WITH trailing newline.
        fs::write(dir.path().join("fresh.txt"), "line one\nline two\n").unwrap();
        let diff =
            GitReader::get_file_diff(dir.path().to_str().unwrap(), "fresh.txt", false, false)
                .expect("untracked text diff");
        assert!(
            diff.starts_with("diff --git a/fresh.txt b/fresh.txt\n"),
            "{diff}"
        );
        assert!(diff.contains("new file mode 100644"));
        assert!(diff.contains("--- /dev/null"));
        assert!(diff.contains("+++ b/fresh.txt"));
        assert!(diff.contains("@@ -0,0 +1,2 @@"));
        assert!(diff.contains("+line one\n+line two\n"));
        assert!(!diff.contains("No newline"), "{diff}");

        // Untracked text WITHOUT trailing newline gains git's marker.
        fs::write(dir.path().join("partial.txt"), "no trailing").unwrap();
        let diff =
            GitReader::get_file_diff(dir.path().to_str().unwrap(), "partial.txt", false, false)
                .expect("untracked partial diff");
        assert!(diff.contains("+no trailing\n\\ No newline at end of file"));

        // Untracked BINARY collapses to git's notice shape.
        fs::write(dir.path().join("blob.bin"), [0x00, 0x01, 0x02]).unwrap();
        let diff = GitReader::get_file_diff(dir.path().to_str().unwrap(), "blob.bin", false, false)
            .expect("untracked binary diff");
        assert!(
            diff.contains("Binary files /dev/null and b/blob.bin differ"),
            "{diff}"
        );

        // Untracked EMPTY file keeps a zero-count hunk and no body.
        fs::write(dir.path().join("hollow.txt"), "").unwrap();
        let diff =
            GitReader::get_file_diff(dir.path().to_str().unwrap(), "hollow.txt", false, false)
                .expect("untracked empty diff");
        assert!(diff.contains("@@ -0,0 +0,0 @@"), "{diff}");
        assert!(
            !diff
                .lines()
                .any(|l| l.starts_with('+') && !l.starts_with("+++")),
            "{diff}"
        );

        // Tracked-but-modified files keep REAL git output (deletions present,
        // which synthesis never emits).
        fs::write(dir.path().join("tracked.txt"), "new\n").unwrap();
        let diff =
            GitReader::get_file_diff(dir.path().to_str().unwrap(), "tracked.txt", false, false)
                .expect("tracked diff");
        assert!(diff.contains("-old\n+new"), "{diff}");

        // A nonexistent never-tracked path keeps today's behavior: empty Ok.
        let diff =
            GitReader::get_file_diff(dir.path().to_str().unwrap(), "ghost.txt", false, false)
                .expect("missing path stays non-fatal");
        assert!(diff.is_empty());
    }

    /// Audit D: merges previously rendered as combined `--cc` diffs whose @@@
    /// headers cannot be numbered by the frontend. Both a clean merge (which
    /// plain `git show` renders as NOTHING) and a conflicted one must come
    /// back as standard hunks against the first parent.
    #[test]
    fn merge_commit_renders_standard_hunks_against_first_parent() {
        let dir = init_repo_with_remotes(&[], "main");
        let path = dir.path().to_str().unwrap();

        // Clean merge: side adds a file while main moves on.
        git_in(dir.path(), &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.path().join("side.txt"), "from side\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "side work"]);
        git_in(dir.path(), &["checkout", "-q", "main"]);
        std::fs::write(dir.path().join("main.txt"), "on main\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "main work"]);
        git_in(
            dir.path(),
            &["merge", "--no-ff", "-q", "side", "-m", "merge"],
        );

        let merge_oid = rev_parse_at(dir.path(), "HEAD");
        let diff = GitReader::get_commit_diff(path, &merge_oid).expect("clean merge diff");
        assert!(
            diff.contains("diff --git a/side.txt b/side.txt"),
            "first-parent diff must include the merged file: {diff}"
        );
        assert!(diff.contains("@@ -0,0 +1"), "{diff}");
        assert!(diff.contains("+from side"), "{diff}");

        // Conflicted merge resolved by hand: raw `show` renders this as
        // `diff --cc` with @@@ hunk headers; first-parent mode must not.
        git_in(dir.path(), &["checkout", "-q", "-b", "clash", "main"]);
        std::fs::write(dir.path().join("main.txt"), "clash version\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "clash base"]);
        git_in(dir.path(), &["checkout", "-q", "side"]);
        std::fs::write(dir.path().join("main.txt"), "side clash\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "side clash"]);
        git_in(dir.path(), &["checkout", "-q", "clash"]);
        let merged = std::process::Command::new("git")
            .args(["merge", "--no-ff", "side", "-m", "clash merge"])
            .current_dir(dir.path())
            .env("GIT_AUTHOR_NAME", "Test User")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test User")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("spawn merge");
        assert!(!merged.status.success(), "fixture needs a real conflict");
        std::fs::write(dir.path().join("main.txt"), "resolved\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "resolve clash"]);

        let merge_oid = rev_parse_at(dir.path(), "HEAD");
        let parents = merge_parents(dir.path(), &merge_oid);
        assert_eq!(parents.len(), 2, "fixture must be a merge");
        let diff = GitReader::get_commit_diff(path, &merge_oid).expect("conflicted merge diff");
        assert!(!diff.contains("diff --cc"), "{diff}");
        assert!(!diff.contains("@@@"), "{diff}");
        assert!(diff.contains("diff --git a/main.txt b/main.txt"), "{diff}");
        assert!(diff.contains("@@"), "{diff}");
    }

    /// Helper for merge_commit_renders_standard_hunks_against_first_parent:
    /// lists the parent oids of `oid` so the fixture can assert it really
    /// produced a two-parent merge commit.
    fn merge_parents(dir: &Path, oid: &str) -> Vec<String> {
        let out = std::process::Command::new("git")
            .args(["rev-parse", &format!("{oid}^@")])
            .current_dir(dir)
            .output()
            .expect("rev-parse parents");
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }

    /// Regression (audit C): without `-c core.quotepath=off`, git C-quotes
    /// non-ASCII paths in diff headers (`"a/caf\303\251.txt"`), garbling them
    /// on the wire. File, commit and range diffs must all carry raw UTF-8,
    /// matching what status/numstat already do.
    #[test]
    fn non_ascii_paths_render_unquoted_in_diff_headers() {
        let dir = init_repo_with_remotes(&[], "main");
        let path = dir.path().to_str().unwrap();
        let unicode_name = "caf\u{e9}.txt";

        // Untracked file diff (synthesized) is raw UTF-8 by construction but
        // must still round-trip through status plumbing keyed by raw path.
        std::fs::write(dir.path().join(unicode_name), "unicode\n").unwrap();
        let diff = GitReader::get_file_diff(path, unicode_name, false, false).expect("untracked");
        assert!(
            diff.starts_with(&format!("diff --git a/{unicode_name} b/{unicode_name}\n")),
            "{diff}"
        );

        // Commit diff header.
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "unicode name"]);
        let oid = rev_parse_at(dir.path(), "HEAD");
        let diff = GitReader::get_commit_diff(path, &oid).expect("commit diff");
        assert!(
            diff.contains(&format!("diff --git a/{unicode_name} b/{unicode_name}")),
            "{diff}"
        );
        let per_file =
            GitReader::get_commit_file_diff(path, &oid, unicode_name).expect("commit file diff");
        assert!(
            per_file.contains(&format!("diff --git a/{unicode_name}")),
            "{per_file}"
        );

        // Range diff header between two branches touching the same file.
        git_in(dir.path(), &["checkout", "-q", "-b", "feat-uni"]);
        std::fs::write(dir.path().join(unicode_name), "unicode v2\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "unicode edit"]);
        let range = GitReader::get_range_diff(path, "main", "feat-uni").expect("range diff");
        assert!(range.contains(unicode_name), "{range}");
        assert!(!range.contains("\\303\\251"), "{range}");
    }
    #[test]
    fn branch_stats_counts_failed_walks_instead_of_hiding_them() {
        // A repo with one healthy branch off the default and one "ghost" branch
        // whose tip object does not exist: the ghost's churn walk errors, which
        // previously vanished silently — now it must surface as compute_failures
        // while the healthy branch still gets its update.
        fn run_git(dir: &std::path::Path, args: &[&str]) {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .expect("git spawn");
            assert!(
                out.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let dir = init_repo_with_remotes(&[], "main");
        run_git(dir.path(), &["branch", "healthy"]);
        // Ghost branch: a real tip whose object file is then deleted from the
        // store — ref resolves to valid-format hex, but every churn walk fails.
        run_git(dir.path(), &["commit", "--allow-empty", "-m", "doomed"]);
        let ghost_oid = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir.path())
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .trim()
        .to_string();
        assert_eq!(ghost_oid.len(), 40);
        run_git(dir.path(), &["branch", "ghost"]);
        run_git(dir.path(), &["reset", "--hard", "HEAD~1"]);
        let obj_path = dir
            .path()
            .join(".git/objects")
            .join(&ghost_oid[..2])
            .join(&ghost_oid[2..]);
        std::fs::remove_file(&obj_path).expect("delete ghost object");

        let report = GitReader::branch_stats(dir.path().to_str().unwrap()).unwrap();
        let names: Vec<&str> = report.updates.iter().map(|u| u.name.as_str()).collect();
        assert!(names.contains(&"healthy"), "healthy branch got churn");
        assert_eq!(report.compute_failures, 1, "ghost walk counted as failure");
    }

    #[test]
    fn list_repo_files_empty_repo_returns_empty_list() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let files = GitReader::list_repo_files(&dir.path().to_string_lossy()).expect("empty repo");
        assert!(files.is_empty());
    }

    /// Exact-result assertion covers everything at once: tracked files, the
    /// untracked-but-not-ignored file, exclusion of the ignored file, and
    /// exclusion of .git internals (any leak breaks the equality).
    #[test]
    fn list_repo_files_mixes_tracked_untracked_and_hides_ignored() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        std::fs::write(dir.path().join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# t\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::write(dir.path().join("src/deep/nested.rs"), "fn main() {}\n").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "seed"]);
        std::fs::write(dir.path().join("fresh.txt"), "new\n").unwrap();
        std::fs::write(dir.path().join("ignored.log"), "noise\n").unwrap();

        let files = GitReader::list_repo_files(&dir.path().to_string_lossy()).expect("listing");
        assert_eq!(
            files,
            [".gitignore", "README.md", "fresh.txt", "src/deep/nested.rs"]
        );
    }

    /// Names a C-quoted renderer would escape (or a shell reinterpret) must
    /// survive verbatim: `-z` + core.quotepath=off deliver raw UTF-8, and
    /// ls-files takes no path arguments here, so a leading dash cannot be
    /// parsed as one.
    #[test]
    fn list_repo_files_preserves_hostile_filenames_verbatim() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let names = [
            "*.md",
            "-dash.txt",
            "[bracket].txt",
            "a/b/c/d/e/f/g.txt",
            "café.txt",
            "emoji-🚀.txt",
            "quo\"te.txt",
            "weird?.txt",
            "with space.txt",
        ];
        for name in names {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, "x\n").unwrap();
        }
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "hostile names"]);

        let mut expected: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        expected.sort();
        let files = GitReader::list_repo_files(&dir.path().to_string_lossy()).expect("listing");
        assert_eq!(files, expected);
    }

    #[test]
    fn list_repo_files_is_deterministic_across_calls() {
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        for name in ["zeta.txt", "alpha/beta.txt", "mid.txt"] {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x\n").unwrap();
        }
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-m", "seed"]);

        let first = GitReader::list_repo_files(&dir.path().to_string_lossy()).expect("first");
        assert!(
            first.windows(2).all(|pair| pair[0] < pair[1]),
            "must be strictly ascending: {first:?}"
        );
        let second = GitReader::list_repo_files(&dir.path().to_string_lossy()).expect("second");
        assert_eq!(first, second);
    }

    /// validate_repo's wording ("Not a Git repository: …") is pinned here so
    /// the explorer can surface a real diagnosis rather than a generic error.
    #[test]
    fn list_repo_files_rejects_non_repository_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let err = GitReader::list_repo_files(&dir.path().to_string_lossy())
            .expect_err("plain directory is not a repository");
        assert!(
            err.to_lowercase().contains("not a git repository"),
            "got: {err}"
        );
    }

    /// The cap must fail loud, never truncate. Registered purely in the index
    /// via update-index --index-info — no blob objects are written and
    /// ls-files reads the index, not the worktree — same ghost-object trick
    /// as the ghost branch in branch_stats_counts_failed_walks_instead_of_hiding_them.
    #[test]
    fn list_repo_files_fails_loudly_past_max_repo_files_cap() {
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let mut child = std::process::Command::new("git")
            .args(["update-index", "--index-info"])
            .current_dir(dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn update-index");
        {
            let stdin = child.stdin.as_mut().expect("stdin pipe");
            for i in 0..=MAX_REPO_FILES {
                writeln!(stdin, "100644 {oid} 0\tbulk-{i:06}.txt").unwrap();
            }
        }
        assert!(child.wait().unwrap().success());

        let err = GitReader::list_repo_files(&dir.path().to_string_lossy())
            .expect_err("over-cap listing must fail loudly");
        assert!(
            err.to_lowercase().contains("file explorer unavailable"),
            "got: {err}"
        );
        assert!(err.contains(&MAX_REPO_FILES.to_string()), "got: {err}");
    }

    #[test]
    fn parse_ls_files_entries_empty_input_yields_empty_vec() {
        assert!(parse_ls_files_entries("").is_empty());
        // A trailing NUL is ordinary ls-files shape; the empty tail record
        // must vanish.
        assert!(parse_ls_files_entries("\0").is_empty());
    }

    /// Submodule entries arrive as "name/": the slash goes, the entry stays.
    #[test]
    fn parse_ls_files_entries_trims_submodule_trailing_slash() {
        let parsed = parse_ls_files_entries("lib/\0src/main.rs\0");
        assert_eq!(parsed, ["lib", "src/main.rs"]);
    }

    #[test]
    fn parse_ls_files_entries_skips_suspicious_entries_dedups_and_sorts_bytewise() {
        let parsed = parse_ls_files_entries(
            ".\0..\0../escape.txt\0deep/../../out.txt\0/etc/passwd\0dup.txt\0dup.txt\0keep.txt\0",
        );
        assert_eq!(parsed, ["dup.txt", "keep.txt"]);
        // Byte-wise order, not collator order: uppercase sorts first.
        let parsed = parse_ls_files_entries("b.txt\0A.txt\0a.txt\0");
        assert_eq!(parsed, ["A.txt", "a.txt", "b.txt"]);
    }
}
