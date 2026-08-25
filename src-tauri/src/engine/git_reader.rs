use crate::analyzer::{DiffChurn, LanguageDetector, LanguageInfo, LocCounter};
use crate::engine::git_cli::{
    self, git, git_text, sandbox_join, sandbox_join_canonical, validate_repo,
};
use crate::engine::git_writer::validate_ref_name;
use crate::graph::lane_solver::RawCommitNode;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

const MAX_BRANCH_STAT_TARGETS: usize = 96;

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

/// One file's patch is excluded from a commit's rendered diff when its changed
/// line count (`additions + deletions` from numstat) exceeds this.
pub const PER_FILE_DIFF_LINE_LIMIT: usize = 5_000;

/// Total changed lines a single rendered commit diff may carry before the
/// remaining normal files are skipped and `truncated` is reported.
pub const DIFF_TOTAL_LINE_BUDGET: usize = 60_000;

/// Hard ceiling on how many per-file patches one commit-diff payload may
/// include regardless of budget headroom.
pub const MAX_INCLUDED_FILES: usize = 2_000;

/// Pathspecs are passed to `git show` in batches no larger than this so argv
/// stays far below OS limits even for enormous commits.
const DIFF_PATHSPEC_BATCH_SIZE: usize = 200;

/// Cap on how many entries [`GitReader::get_commit_files`] returns; the full
/// count rides along on [`CommitDetails`] as `files_total_count`.
pub const COMMIT_FILES_LIST_CAP: usize = 500;

/// Most recent tags returned by the tag listing; older tags beyond this cap
/// are dropped so a repository with thousands of tags cannot flood the UI.
pub const REFS_TAG_CAP: usize = 200;

/// Hard cap on lines fetched by one ranged blame request.
pub const BLAME_MAX_RANGE_LINES: usize = 50_000;

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
    /// True when more eligible branches existed than MAX_BRANCH_STAT_TARGETS;
    /// callers re-invoke to drain the rest.
    pub capped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagInfo {
    pub name: String,
    pub commit_id: String,
    pub message: Option<String>,
}

/// Wire shape for [`GitReader::list_tags_summary`]: the newest
/// [`REFS_TAG_CAP`] tags (creatordate descending) plus the totals needed to
/// tell the user that older tags were dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagListSummary {
    pub tags: Vec<TagInfo>,
    /// Total number of tags present in the repository.
    pub total_tags: usize,
    /// True when `total_tags` exceeded [`REFS_TAG_CAP`] and only the newest
    /// cap worth of tags are listed.
    pub tags_truncated: bool,
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
}

/// Wire shape for [`GitReader::get_status_payload`]: the working-tree status
/// plus a health flag for the auxiliary numstat lookups.
///
/// `stats_degraded` is true only when BOTH numstat attempts (worktree and
/// staged diff) failed; per-file addition/deletion counts are then zero but
/// the paths themselves are still trustworthy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPayload {
    pub files: Vec<FileStatus>,
    pub stats_degraded: bool,
}

/// Wire shape for [`GitReader::get_commit_diff_payload`].
///
/// Large commits no longer fail wholesale: normal-sized files are included
/// until the line budget or file cap is hit, and everything else (oversized
/// text, binary, budget-exhausted) lands in `skipped_files` with its numstat
/// counts so callers can explain what was left out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitDiffPayload {
    pub content: String,
    pub truncated: bool,
    pub included_files: u32,
    pub skipped_files: Vec<CommitFileChange>,
    pub total_files: u32,
    pub total_additions: usize,
    pub total_deletions: usize,
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

/// Wire shape for [`GitReader::get_blame_range`]: the blame rows for the
/// requested range plus a flag set when the range exceeded
/// [`BLAME_MAX_RANGE_LINES`] and `end_line` was clamped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlameResult {
    pub lines: Vec<BlameLine>,
    pub truncated: bool,
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
    /// Total number of files changed by the commit, BEFORE the
    /// [`COMMIT_FILES_LIST_CAP`] truncation applied to `changed_files`.
    pub files_total_count: usize,
    /// True when more files changed than [`COMMIT_FILES_LIST_CAP`] and
    /// `changed_files` was truncated.
    pub files_list_truncated: bool,
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
        let origin_head = git_text(
            &repo,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        )
        .ok();
        // Resolving the default base BEFORE listing lets ahead-behind vs that
        // base ride the same for-each-ref process, so this call never blocks
        // on per-branch churn subprocesses.
        let default_base = resolve_default_base(&repo, origin_head.as_deref());

        let mut format = String::from(BRANCH_LIST_FORMAT);
        if let Some((_, base_oid)) = default_base.as_ref() {
            format.push_str("%01%(ahead-behind:");
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
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\u{01}').collect();
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
        let default_branch = pick_default_branch(&local_names, origin_head.as_deref());
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
        let origin_head = git_text(
            &repo,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        )
        .ok();

        // Cheap listing only: refnames and tips, no history walks here.
        let stdout = git_text(
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname)%01%(objectname)",
                "refs/heads/",
                "refs/remotes/",
            ],
        )?;

        let mut local_names: Vec<String> = Vec::new();
        let mut targets: Vec<BranchStatTarget> = Vec::new();
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\u{01}').collect();
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

        let default_branch = pick_default_branch(&local_names, origin_head.as_deref());
        // Same resolution rules as list_branches' pre-listing probe; when that
        // finds nothing (no origin/HEAD, no conventional name), fall back to
        // pick_default_branch's choice now that the locals are known.
        let resolved_default = resolve_default_base(&repo, origin_head.as_deref()).or_else(|| {
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
                let local = target
                    .remote_name
                    .as_deref()
                    .and_then(|remote| target.name.strip_prefix(&format!("{remote}/")))
                    .unwrap_or(target.name.as_str());
                if local_set.contains(local) {
                    continue;
                }
            }
            eligible.push(target);
        }
        let capped = eligible.len() > MAX_BRANCH_STAT_TARGETS;
        eligible.truncate(MAX_BRANCH_STAT_TARGETS);

        let results: Vec<(BranchStatTarget, Option<ComputedBranchChurn>, bool)> = eligible
            .into_par_iter()
            .map(|target| {
                let (churn, was_cached) =
                    cached_branch_churn(&repo_key, &base_oid, &target.tip_commit_id, || {
                        compute_branch_churn(&repo, &base_oid, &target.tip_commit_id)
                    });
                (target, churn, was_cached)
            })
            .collect();

        let mut updates = Vec::with_capacity(results.len());
        let mut computed = 0;
        let mut cached = 0;
        for (target, churn, was_cached) in results {
            let Some(churn) = churn else {
                continue;
            };
            if was_cached {
                cached += 1;
            } else {
                computed += 1;
            }
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

        Ok(BranchStatsReport {
            compared_to,
            updates,
            computed,
            cached,
            capped,
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
    /// A single walk is capped at [`MAX_HISTORY_COMMITS`]; callers that need
    /// deeper history paginate rather than raise the cap, so a request can
    /// never ask git for an unbounded log on a monorepo-scale repository.
    pub fn read_commit_history_paged(
        repo_path: &str,
        skip: usize,
        max_count: usize,
        revision: Option<&str>,
    ) -> Result<Vec<RawCommitNode>, String> {
        let repo = validate_repo(repo_path)?;
        let count = max_count.clamp(1, Self::MAX_HISTORY_COMMITS);
        let skipped = skip.min(Self::MAX_HISTORY_COMMITS);
        Self::history_rows(&repo, skipped, count, revision)
    }

    /// Reads one page of history AND answers "are there more rows?" in a
    /// single walk, so paginators do not have to fetch `limit + 1` themselves.
    ///
    /// The probe fetch runs BEFORE any clamping: it asks git for
    /// `min(limit, MAX_HISTORY_COMMITS) + 1` rows, reports
    /// `has_more = fetched > effective_limit`, and only then truncates the
    /// returned rows to the effective limit. Unlike
    /// [`GitReader::read_commit_history_paged`], this stays correct when the
    /// caller's limit equals [`MAX_HISTORY_COMMITS`] exactly — the extra probe
    /// row is fetched as CAP + 1, never clamped away.
    pub fn read_commit_history_probe(
        repo_path: &str,
        skip: usize,
        max_count: usize,
        revision: Option<&str>,
    ) -> Result<(Vec<RawCommitNode>, bool), String> {
        let repo = validate_repo(repo_path)?;
        let effective_limit = max_count.clamp(1, Self::MAX_HISTORY_COMMITS);
        let fetch = (effective_limit + 1).min(Self::MAX_HISTORY_COMMITS.saturating_add(1));
        let skipped = skip.min(Self::MAX_HISTORY_COMMITS);
        let mut rows = Self::history_rows(&repo, skipped, fetch, revision)?;
        let has_more = rows.len() > effective_limit;
        rows.truncate(effective_limit);
        Ok((rows, has_more))
    }

    /// Shared log walker: exactly `count` rows after `skip` skips. No clamping
    /// here — both public wrappers own their own limits.
    fn history_rows(
        repo: &Path,
        skip: usize,
        count: usize,
        revision: Option<&str>,
    ) -> Result<Vec<RawCommitNode>, String> {
        let count_arg = format!("-n{}", count);
        let skip_arg = format!("--skip={}", skip);
        let mut args = vec![
            "log",
            count_arg.as_str(),
            skip_arg.as_str(),
            "--topo-order",
            "--format=format:%H%x00%P%x00%ct%x00%an%x00%ae%x00%s%x01",
        ];
        if let Some(rev) = revision {
            validate_ref_name(rev)?;
            args.push(rev);
        } else {
            args.push("--all");
        }
        let stdout = git_text(repo, &args)?;

        let mut commits = Vec::new();
        for record in stdout.split('\x01') {
            let record = record.trim();
            if record.is_empty() {
                continue;
            }
            let fields: Vec<&str> = record.split('\x00').collect();
            if fields.len() < 6 {
                continue;
            }
            let parent_ids = if fields[1].is_empty() {
                Vec::new()
            } else {
                fields[1].split_whitespace().map(String::from).collect()
            };
            commits.push(RawCommitNode {
                id: fields[0].to_string(),
                parent_ids,
                timestamp: fields[2].parse().unwrap_or(0),
                author_name: fields[3].to_string(),
                author_email: fields[4].to_string(),
                summary: fields[5].to_string(),
            });
        }
        Ok(commits)
    }

    pub fn commits_touching_path(
        repo_path: &str,
        file_path: &str,
        max_count: usize,
    ) -> Result<Vec<String>, String> {
        let repo = validate_repo(repo_path)?;
        let _ = sandbox_join(&repo, file_path)?;
        let count = max_count.clamp(1, 100_000).to_string();
        let stdout = git_text(
            &repo,
            &[
                "log",
                &format!("-n{}", count),
                "--format=%H",
                "--",
                file_path,
            ],
        )?;
        Ok(stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    pub fn get_status(repo_path: &str) -> Result<Vec<FileStatus>, String> {
        Ok(Self::get_status_payload(repo_path)?.files)
    }

    /// Full status payload: files plus the `stats_degraded` health flag.
    ///
    /// The two auxiliary numstat walks (`diff --numstat` for unstaged,
    /// `diff --cached --numstat` for staged) are retried once on failure; if
    /// both attempts fail, `stats_degraded` is true and those counts are zero
    /// while paths/status codes remain authoritative.
    pub fn get_status_payload(repo_path: &str) -> Result<StatusPayload, String> {
        let repo = validate_repo(repo_path)?;
        let stdout = git_text(&repo, &["status", "--porcelain=v1", "-z"])?;
        Self::status_from_porcelain(&repo, &stdout, numstat_with_retry)
    }

    /// Assembles [`StatusPayload`] from porcelain output with an injectable
    /// numstat fetcher so tests can simulate repeated failures.
    fn status_from_porcelain(
        repo: &Path,
        stdout: &str,
        fetch_numstat: impl Fn(&Path, bool) -> (HashMap<String, (usize, usize)>, bool),
    ) -> Result<StatusPayload, String> {
        let (numstat_work, degraded_work) = fetch_numstat(repo, false);
        let (numstat_index, degraded_index) = fetch_numstat(repo, true);
        let stats_degraded = degraded_work || degraded_index;

        let mut statuses = Vec::new();
        let bytes = stdout.as_bytes();
        let mut i = 0;
        while i + 3 < bytes.len() {
            let index_status = bytes[i] as char;
            let work_status = bytes[i + 1] as char;
            i += 3;
            // In -z mode a rename record is `XY NEW\0OLD\0` (git swaps the
            // arrow order versus non-z output), every other entry is
            // `XY PATH\0`.
            let rest = match bytes[i..].iter().position(|&b| b == 0) {
                Some(n) => {
                    let s = String::from_utf8_lossy(&bytes[i..i + n]).into_owned();
                    i += n + 1;
                    s
                }
                None => break,
            };

            let (path, old_path) = if rest.contains(" -> ") {
                let mut split = rest.splitn(2, " -> ");
                let old = split.next().map(String::from);
                let new = split.next().unwrap_or(&rest).to_string();
                (new, old)
            } else if index_status == 'R' || work_status == 'R' {
                match bytes.get(i..).and_then(|slice| {
                    slice
                        .iter()
                        .position(|&b| b == 0)
                        .map(|n| String::from_utf8_lossy(&slice[..n]).into_owned())
                }) {
                    // git lists the NEW path first, then the ORIGINAL path;
                    // assigning them swapped made stage/unstage/discard target
                    // the wrong file and broke numstat lookups.
                    Some(old) => {
                        i += old.len() + 1;
                        (rest.clone(), Some(old))
                    }
                    None => (rest, None),
                }
            } else {
                (rest, None)
            };

            let is_conflicted = index_status == 'U'
                || work_status == 'U'
                || (index_status == 'A' && work_status == 'A')
                || (index_status == 'D' && work_status == 'D');
            let is_staged = index_status != ' ' && index_status != '?';
            let (additions, deletions) = if is_staged {
                numstat_index.get(&path).copied().unwrap_or((0, 0))
            } else {
                numstat_work.get(&path).copied().unwrap_or((0, 0))
            };

            statuses.push(FileStatus {
                path,
                old_path,
                status_code: format!("{}{}", index_status, work_status),
                is_staged,
                is_conflicted,
                additions,
                deletions,
            });
        }
        Ok(StatusPayload {
            files: statuses,
            stats_degraded,
        })
    }

    pub fn get_file_blame(repo_path: &str, file_path: &str) -> Result<Vec<BlameLine>, String> {
        let repo = validate_repo(repo_path)?;
        Ok(Self::blame_porcelain(&repo, file_path, None)?.lines)
    }

    /// Blame for a 1-based, inclusive `[start_line, end_line]` window.
    ///
    /// Ranges larger than [`BLAME_MAX_RANGE_LINES`] are clamped and reported
    /// via [`BlameResult::truncated`] instead of letting one request pull
    /// hundreds of thousands of porcelain records through the pipe.
    pub fn get_blame_range(
        repo_path: &str,
        file_path: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<BlameResult, String> {
        let repo = validate_repo(repo_path)?;
        if start_line == 0 {
            return Err("blame lines are 1-based; start line must be at least 1".into());
        }
        if end_line < start_line {
            return Err("blame end line precedes start line".into());
        }
        let clamped_end = end_line.min(start_line.saturating_add(BLAME_MAX_RANGE_LINES - 1));
        let truncated = clamped_end != end_line;
        let mut result = Self::blame_porcelain(&repo, file_path, Some((start_line, clamped_end)))?;
        result.truncated = truncated;
        Ok(result)
    }

    /// Shared blame runner. `range` is an inclusive 1-based window; when it is
    /// `None` the whole file is blamed exactly as before (no `-L`, so git's
    /// own line-count validation never rejects short files).
    fn blame_porcelain(
        repo: &Path,
        file_path: &str,
        range: Option<(usize, usize)>,
    ) -> Result<BlameResult, String> {
        let _ = sandbox_join(repo, file_path)?;
        let stdout = match range {
            None => git_text(repo, &["blame", "--line-porcelain", "--", file_path])?,
            Some((start, end)) => {
                let range_arg = format!("-L{},{}", start, end);
                git_text(
                    repo,
                    &[
                        "blame",
                        "--line-porcelain",
                        range_arg.as_str(),
                        "--",
                        file_path,
                    ],
                )?
            }
        };
        let base_line = range.map(|(start, _)| start).unwrap_or(1);

        let mut blame_lines = Vec::new();
        let mut current_sha = String::new();
        let mut current_author = String::new();
        let mut current_email = String::new();
        let mut current_time: i64 = 0;
        let mut line_count = base_line;

        for line in stdout.lines() {
            if let Some(content) = line.strip_prefix('\t') {
                blame_lines.push(BlameLine {
                    line_no: line_count,
                    commit_id: current_sha.clone(),
                    author_name: current_author.clone(),
                    author_email: current_email.clone(),
                    timestamp: current_time,
                    content: content.to_string(),
                });
                line_count += 1;
            } else if let Some(author) = line.strip_prefix("author ") {
                current_author = author.to_string();
            } else if let Some(mail) = line.strip_prefix("author-mail ") {
                current_email = mail.trim_matches(|c| c == '<' || c == '>').to_string();
            } else if let Some(time) = line.strip_prefix("author-time ") {
                current_time = time.parse().unwrap_or(0);
            } else {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if !parts.is_empty()
                    && parts[0].len() == 40
                    && parts[0].chars().all(|c| c.is_ascii_hexdigit())
                {
                    current_sha = parts[0].to_string();
                }
            }
        }
        Ok(BlameResult {
            lines: blame_lines,
            truncated: false,
        })
    }

    pub fn get_file_diff(
        repo_path: &str,
        file_path: &str,
        is_staged: bool,
        ignore_whitespace: bool,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _ = sandbox_join(&repo, file_path)?;
        let mut args = vec!["diff"];
        if is_staged {
            args.push("--cached");
        }
        if ignore_whitespace {
            args.push("-w");
        }
        args.push("--");
        args.push(file_path);
        git_text(&repo, &args)
    }

    /// Legacy string view of a commit's diff, kept for existing callers.
    ///
    /// Delegates to [`GitReader::get_commit_diff_payload`]; unlike the
    /// historical implementation it no longer fails outright when the raw
    /// patch would exceed the 64 MB pipe cap — oversized/binary files are
    /// skipped and the returned content is simply truncated. New callers
    /// should use [`GitReader::get_commit_diff_payload`] to also receive the
    /// truncation metadata.
    pub fn get_commit_diff(repo_path: &str, commit_id: &str) -> Result<String, String> {
        Ok(Self::get_commit_diff_payload(repo_path, commit_id)?.content)
    }

    /// Renders one commit's diff with hard bounds on time, memory, and IPC
    /// size.
    ///
    /// A cheap numstat preflight (`--numstat -z`, no patch text) classifies
    /// every changed file as binary, oversized (more than
    /// [`PER_FILE_DIFF_LINE_LIMIT`] changed lines), or normal. When everything
    /// is normal and within [`DIFF_TOTAL_LINE_BUDGET`], the fast path runs the
    /// exact command the legacy implementation used and returns its full
    /// output unmodified. Otherwise normal files are included until the budget
    /// or [`MAX_INCLUDED_FILES`] is reached — fetched via explicit pathspec
    /// batches of at most [`DIFF_PATHSPEC_BATCH_SIZE`] paths — and every other
    /// file lands in `skipped_files` with its numstat counts.
    pub fn get_commit_diff_payload(
        repo_path: &str,
        commit_id: &str,
    ) -> Result<CommitDiffPayload, String> {
        let repo = validate_repo(repo_path)?;
        validate_oid(commit_id)?;
        let stdout = git_text(
            &repo,
            &["show", "--pretty=format:", "--numstat", "-z", commit_id],
        )?;
        let entries = parse_numstat_z(&stdout);

        let total_files = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        let total_additions = entries.iter().map(|e| e.additions).sum();
        let total_deletions = entries.iter().map(|e| e.deletions).sum();

        let all_normal = entries
            .iter()
            .all(|e| !e.is_binary && e.additions + e.deletions <= PER_FILE_DIFF_LINE_LIMIT);
        let total_changed_lines: usize = entries
            .iter()
            .filter(|e| !e.is_binary)
            .map(|e| e.additions + e.deletions)
            .sum();
        if all_normal && total_changed_lines <= DIFF_TOTAL_LINE_BUDGET {
            // Fast path: identical bytes to the historical implementation.
            let content = git_text(&repo, &["show", "--unified=3", "--format=", commit_id])?;
            return Ok(CommitDiffPayload {
                content,
                truncated: false,
                included_files: total_files,
                skipped_files: Vec::new(),
                total_files,
                total_additions,
                total_deletions,
            });
        }

        // Slow path: greedy inclusion in numstat order; smaller later files may
        // still fit when an earlier one missed the remaining budget.
        let mut included: Vec<&NumstatZEntry> = Vec::new();
        let mut skipped_files: Vec<CommitFileChange> = Vec::new();
        let mut budget = DIFF_TOTAL_LINE_BUDGET;
        for entry in &entries {
            let change = CommitFileChange {
                path: entry.path.clone(),
                status_code: if entry.is_binary {
                    "B".to_string()
                } else {
                    "M".to_string()
                },
                additions: entry.additions,
                deletions: entry.deletions,
            };
            let fits_budget =
                !entry.is_binary && entry.additions + entry.deletions <= PER_FILE_DIFF_LINE_LIMIT;
            if fits_budget
                && included.len() < MAX_INCLUDED_FILES
                && entry.additions + entry.deletions <= budget
            {
                budget -= entry.additions + entry.deletions;
                included.push(entry);
            } else {
                skipped_files.push(change);
            }
        }

        let mut content = String::new();
        for batch in included.chunks(DIFF_PATHSPEC_BATCH_SIZE) {
            let mut args: Vec<&str> = vec!["show", "--pretty=format:", "--unified=3", "--"];
            for entry in batch {
                args.push(&entry.pathspec);
            }
            let piece = git_text(&repo, &args)?;
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&piece);
        }

        Ok(CommitDiffPayload {
            content,
            truncated: true,
            included_files: u32::try_from(included.len()).unwrap_or(u32::MAX),
            skipped_files,
            total_files,
            total_additions,
            total_deletions,
        })
    }

    pub fn get_range_diff(repo_path: &str, from: &str, to: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        validate_ref_name(from)?;
        validate_ref_name(to)?;
        let spec = format!("{}...{}", from, to);
        git_text(&repo, &["diff", "--unified=3", &spec])
    }

    pub fn get_commit_files(
        repo_path: &str,
        commit_id: &str,
    ) -> Result<Vec<CommitFileChange>, String> {
        let repo = validate_repo(repo_path)?;
        validate_oid(commit_id)?;
        let mut files = Self::commit_files_uncapped(&repo, commit_id)?;
        files.truncate(COMMIT_FILES_LIST_CAP);
        Ok(files)
    }

    /// Uncapped numstat listing; [`GitReader::get_commit_details`] needs the
    /// full list to compute accurate totals before truncation.
    fn commit_files_uncapped(
        repo: &Path,
        commit_id: &str,
    ) -> Result<Vec<CommitFileChange>, String> {
        let stdout = git_text(repo, &["show", "--pretty=format:", "--numstat", commit_id])?;
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
        let all_files = Self::commit_files_uncapped(&repo, commit_id).unwrap_or_default();
        let files_total_count = all_files.len();
        let mut changed_files = all_files;
        // Totals describe the WHOLE commit even when the shipped list is
        // truncated for the UI.
        let total_additions = changed_files.iter().map(|f| f.additions).sum();
        let total_deletions = changed_files.iter().map(|f| f.deletions).sum();
        let files_list_truncated = changed_files.len() > COMMIT_FILES_LIST_CAP;
        changed_files.truncate(COMMIT_FILES_LIST_CAP);
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
            files_total_count,
            files_list_truncated,
        })
    }

    pub fn list_tags(repo_path: &str) -> Result<Vec<TagInfo>, String> {
        let mut summary = Self::list_tags_summary(repo_path)?;
        // Legacy callers expect the historical alphabetical (refname) order;
        // the cap selection itself is newest-first.
        summary.tags.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(summary.tags)
    }

    /// Tag listing with the decoration cap made explicit: only the
    /// [`REFS_TAG_CAP`] most recent tags (by creatordate, refname descending as
    /// a deterministic tie-break) are returned, with `total_tags` and
    /// `tags_truncated` telling the caller what was dropped.
    pub fn list_tags_summary(repo_path: &str) -> Result<TagListSummary, String> {
        let repo = validate_repo(repo_path)?;
        let stdout = git_text(
            &repo,
            &[
                "tag",
                "-l",
                "--sort=-creatordate",
                "--sort=-refname",
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
        let total_tags = tags.len();
        let tags_truncated = total_tags > REFS_TAG_CAP;
        tags.truncate(REFS_TAG_CAP);
        Ok(TagListSummary {
            tags,
            total_tags,
            tags_truncated,
        })
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

    pub fn get_repo_language_stats(repo_path: &str) -> Result<Vec<RepoLanguageStat>, String> {
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
        let selected = LanguageDetector::prioritize_for_stats(candidates, 10_000);

        let mut lang_counts: HashMap<&'static str, (usize, usize, &'static str, &'static str)> =
            HashMap::new();
        let mut total_lines = 0usize;

        for (rel_path, path_info) in selected {
            // Resolve through symlinks so a tracked file that is really a link
            // pointing outside the repo is refused instead of read.
            let full_path = match git_cli::sandbox_join_canonical(&repo, &rel_path) {
                Ok(path) => path,
                Err(_) => {
                    record_lang(&mut lang_counts, path_info, 0);
                    continue;
                }
            };
            let bytes = match std::fs::read(&full_path) {
                Ok(bytes) => bytes,
                Err(_) => {
                    record_lang(&mut lang_counts, path_info, 0);
                    continue;
                }
            };
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
        Ok(stats)
    }
}

fn pick_default_branch(local_names: &[String], origin_head: Option<&str>) -> String {
    if let Some(head) = origin_head {
        let trimmed = head.trim();
        let short = trimmed
            .strip_prefix("refs/remotes/origin/")
            .or_else(|| trimmed.strip_prefix("origin/"))
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
const BRANCH_LIST_FORMAT: &str = "%(HEAD)%01%(refname)%01%(objectname)%01%(upstream:track)%01%(upstream:short)%01%(committerdate:unix)%01%(authorname)%01%(contents:subject)";

/// Resolves the default branch to (short name, commit oid) without needing the
/// local ref list, so `list_branches` can use it before listing refs.
///
/// Priority mirrors [`pick_default_branch`]: origin/HEAD's branch first (local
/// head preferred over the remote-tracking ref so remote-only repos still
/// resolve), then conventional main/master/trunk/develop heads. None when no
/// candidate resolves (empty or detached-only repository).
fn resolve_default_base(repo: &Path, origin_head: Option<&str>) -> Option<(String, String)> {
    let mut candidates: Vec<(&str, String)> = Vec::new();
    if let Some(head) = origin_head.map(str::trim) {
        if let Some(short) = head.strip_prefix("refs/remotes/origin/") {
            candidates.push((short, format!("refs/heads/{short}")));
            candidates.push((short, head.to_string()));
        }
    }
    for candidate in ["main", "master", "trunk", "develop"] {
        candidates.push((candidate, format!("refs/heads/{candidate}")));
    }
    candidates.into_iter().find_map(|(short, refname)| {
        let peeled = format!("{refname}^{{commit}}");
        git_text(repo, &["rev-parse", "--verify", "--quiet", &peeled])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|oid| !oid.is_empty() && validate_oid(oid).is_ok())
            .map(|oid| (short.to_string(), oid))
    })
}

/// Parses `%(ahead-behind:<base>)` output (`"<ahead> <behind>"`). Malformed
/// input yields zeros rather than failing the whole listing.
fn parse_ahead_behind_field(raw: &str) -> (usize, usize) {
    let mut parts = raw.split_whitespace();
    let ahead = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let behind = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (ahead, behind)
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

/// Content-addressed memo for branch churn keyed by (repo path, base oid, tip
/// oid): churn depends only on the two trees, so entries cannot go stale — a
/// force-moved branch simply misses on its new tip. Oldest-inserted eviction
/// keeps the map bounded without pulling in an LRU crate.
struct ChurnCache {
    capacity: usize,
    entries: HashMap<(String, String, String), ComputedBranchChurn>,
    order: VecDeque<(String, String, String)>,
}

impl ChurnCache {
    fn new() -> Self {
        Self {
            capacity: CHURN_CACHE_CAPACITY,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, key: &(String, String, String)) -> Option<ComputedBranchChurn> {
        self.entries.get(key).copied()
    }

    /// Inserts a value, refreshing the key's insertion recency and evicting
    /// oldest-inserted keys past the capacity bound.
    fn insert(&mut self, key: (String, String, String), value: ComputedBranchChurn) {
        if self.entries.insert(key.clone(), value).is_some() {
            self.order.retain(|existing| existing != &key);
        }
        self.order.push_back(key);
        while self.order.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            }
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

/// Looks up churn by (repo, base oid, tip oid), computing and memoizing on
/// miss. Failed computations are never cached: a transient git failure must
/// not be remembered as truth.
fn cached_branch_churn(
    repo_key: &str,
    base_oid: &str,
    tip_oid: &str,
    compute: impl FnOnce() -> Option<ComputedBranchChurn>,
) -> (Option<ComputedBranchChurn>, bool) {
    let key = (
        repo_key.to_string(),
        base_oid.to_string(),
        tip_oid.to_string(),
    );
    if let Some(hit) = churn_cache().get(&key) {
        return (Some(hit), true);
    }
    let Some(computed) = compute() else {
        return (None, false);
    };
    churn_cache().insert(key, computed);
    (Some(computed), false)
}

/// Computes diff churn between two validated ref names or full oids via
/// `<base>...<tip>` (two git processes). Returns None when either side fails
/// validation or git rejects the walk.
fn compute_branch_churn(repo: &Path, base: &str, branch: &str) -> Option<ComputedBranchChurn> {
    if validate_ref_name(base).is_err() || validate_ref_name(branch).is_err() {
        return None;
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

/// One numstat record from the `-z` preflight of
/// [`GitReader::get_commit_diff_payload`].
struct NumstatZEntry {
    path: String,
    /// `:(literal)`-prefixed pathspec so metacharacters in real filenames are
    /// never interpreted as globs when fetching per-file patches.
    pathspec: String,
    additions: usize,
    deletions: usize,
    is_binary: bool,
}

/// Parses `git show --pretty=format: --numstat -z <commit>`.
///
/// With `-z`, records are NUL-separated and never quoted: normal and binary
/// files are `add\tdel\tpath\0` (binary uses `-` for both counts), while
/// renames/copies emit an empty third field followed by the ORIGINAL path and
/// then the NEW path (`add\tdel\t\0from\0to\0`).
fn parse_numstat_z(stdout: &str) -> Vec<NumstatZEntry> {
    let mut entries = Vec::new();
    let mut records = stdout.split('\0');
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, '\t');
        let add_raw = fields.next().unwrap_or("");
        let del_raw = fields.next().unwrap_or("");
        let Some(third) = fields.next() else {
            continue;
        };
        let is_binary = add_raw == "-" && del_raw == "-";
        let entry = |path: String| NumstatZEntry {
            pathspec: format!(":(literal){}", path),
            path,
            additions: add_raw.parse().unwrap_or(0),
            deletions: del_raw.parse().unwrap_or(0),
            is_binary,
        };
        if third.is_empty() {
            // Rename/copy: the ORIGINAL path and then the NEW path follow as
            // their own NUL-terminated records (`add\tdel\t\0from\0to\0`).
            // The original record is consumed to advance the stream; patches
            // for renames are fetched under the new path alone.
            match records.next() {
                Some(from) if !from.is_empty() => {}
                _ => break,
            }
            let to = match records.next() {
                Some(t) if !t.is_empty() => t,
                _ => break,
            };
            entries.push(entry(to.to_string()));
        } else {
            entries.push(entry(third.to_string()));
        }
    }
    entries
}

fn parse_numstat(stdout: &str) -> HashMap<String, (usize, usize)> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            let add = parts[0].parse().unwrap_or(0);
            let del = parts[1].parse().unwrap_or(0);
            let path = c_unquote_path(parts[2].rsplit(" => ").next().unwrap_or(parts[2]));
            map.insert(path, (add, del));
        }
    }
    map
}

fn parse_numstat_files(stdout: &str) -> Vec<CommitFileChange> {
    let mut files = Vec::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let path = c_unquote_path(parts[2].rsplit(" => ").next().unwrap_or(parts[2]));
        let status_code = if parts[0] == "-" && parts[1] == "-" {
            "B".to_string()
        } else if parts[2].contains(" => ") {
            "R".to_string()
        } else {
            "M".to_string()
        };
        files.push(CommitFileChange {
            path,
            status_code,
            additions: parts[0].parse().unwrap_or(0),
            deletions: parts[1].parse().unwrap_or(0),
        });
    }
    files
}

/// Fetches one status numstat map with a single retry; reports `true` (the
/// degraded flag) only when BOTH attempts fail. Success-path behavior is
/// unchanged from the historical `unwrap_or_default()` — a healthy repo gets
/// exactly the same numbers.
fn numstat_with_retry(repo: &Path, cached: bool) -> (HashMap<String, (usize, usize)>, bool) {
    let mut args: Vec<&str> = vec!["diff"];
    if cached {
        args.push("--cached");
    }
    args.push("--numstat");
    match git_text(repo, &args) {
        Ok(stdout) => (parse_numstat(&stdout), false),
        Err(_) => match git_text(repo, &args) {
            Ok(stdout) => (parse_numstat(&stdout), false),
            Err(_) => (HashMap::new(), true),
        },
    }
}

/// Decodes a C-style git-quoted path (`"\346\226\207.txt"`) back to raw bytes.
///
/// With `core.quotepath=false` non-ASCII names arrive unquoted, but paths
/// containing double quotes or control characters are still always quoted by
/// git regardless of config, so numstat parsing stays defensive.
fn c_unquote_path(raw: &str) -> String {
    if raw.len() < 2 || !raw.starts_with('"') || !raw.ends_with('"') {
        return raw.to_string();
    }
    let inner = &raw[1..raw.len() - 1];
    // Quoted output escapes every byte outside printable ASCII, so iterating
    // bytes is safe and keeps octal sequences exact before UTF-8 reassembly.
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0B),
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            b'0'..=b'7' => {
                let mut value: u32 = (bytes[i] - b'0') as u32;
                let mut digits = 1;
                while digits < 3 && i + 1 < bytes.len() && (b'0'..=b'7').contains(&bytes[i + 1]) {
                    i += 1;
                    digits += 1;
                    value = value * 8 + (bytes[i] - b'0') as u32;
                }
                out.push((value & 0xFF) as u8);
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

    #[test]
    fn test_parse_co_authors() {
        let body = "Implements login.\n\nCo-authored-by: Bob <bob@example.com>\n";
        assert_eq!(
            parse_co_authors(body),
            vec!["Bob <bob@example.com>".to_string()]
        );
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
            pick_default_branch(&names, Some("refs/remotes/origin/main")),
            "main"
        );
        assert_eq!(
            pick_default_branch(&names, Some("origin/develop")),
            "develop"
        );
        let master_only = vec!["master".into(), "hotfix".into()];
        assert_eq!(pick_default_branch(&master_only, None), "master");
        let other = vec!["trunk".into()];
        assert_eq!(pick_default_branch(&other, None), "trunk");
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

    // ------------------------------------------------------------------
    // Hardening regression suite: helpers first.
    // ------------------------------------------------------------------

    fn run_git(dir: &Path, args: &[&str]) -> String {
        run_git_at(dir, 1_700_000_000, args)
    }

    /// Runs git with fixed author/committer identity; `unix_date` pins the
    /// commit (and therefore lightweight-tag creatordate) so ordering tests
    /// are deterministic even when many refs share one wall-clock second.
    fn run_git_at(dir: &Path, unix_date: i64, args: &[&str]) -> String {
        let date = format!("@{unix_date} +0000");
        let output = std::process::Command::new("git")
            .args(args)
            .env("GIT_AUTHOR_NAME", "GitPulse")
            .env("GIT_AUTHOR_EMAIL", "gitpulse@test.local")
            .env("GIT_COMMITTER_NAME", "GitPulse")
            .env("GIT_COMMITTER_EMAIL", "gitpulse@test.local")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        dir
    }

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(dest, content).unwrap();
    }

    fn commit_file(dir: &Path, rel: &str, content: &str, msg: &str) -> String {
        write_file(dir, rel, content);
        run_git(dir, &["add", rel]);
        run_git(dir, &["commit", "-m", msg]);
        run_git(dir, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn repo_str(dir: &Path) -> String {
        dir.to_string_lossy().into_owned()
    }

    fn numstat_ok(cached: bool) -> impl Fn(&Path, bool) -> (HashMap<String, (usize, usize)>, bool) {
        move |_repo, want_cached| {
            if want_cached == cached {
                (
                    HashMap::from([
                        ("new.txt".to_string(), (3usize, 4usize)),
                        ("tracked.txt".to_string(), (3usize, 4usize)),
                    ]),
                    false,
                )
            } else {
                (HashMap::new(), false)
            }
        }
    }

    // Item 1: porcelain -z rename paths must be (NEW, Some(OLD)).

    #[test]
    fn status_z_rename_record_assigns_new_then_old() {
        let stdout = "R  new.txt\0old.txt\0M  edited.txt\0";
        let payload =
            GitReader::status_from_porcelain(Path::new("/unused"), stdout, numstat_ok(true))
                .expect("parse synthetic status");
        assert!(!payload.stats_degraded);
        let rename = &payload.files[0];
        assert_eq!(rename.path, "new.txt");
        assert_eq!(rename.old_path.as_deref(), Some("old.txt"));
        assert_eq!(rename.status_code, "R ");
        assert!(rename.is_staged);
        // Numstat lookup must hit under the NEW path (this is what stage and
        // discard act on).
        assert_eq!((rename.additions, rename.deletions), (3, 4));
        let plain = &payload.files[1];
        assert_eq!(plain.path, "edited.txt");
        assert!(plain.old_path.is_none());
    }

    #[test]
    fn get_status_reports_staged_rename_new_path_with_old_path() {
        let dir = init_repo();
        commit_file(dir.path(), "a.txt", "one\n", "base");
        run_git(dir.path(), &["mv", "a.txt", "b.txt"]);
        let payload = GitReader::get_status_payload(&repo_str(dir.path())).expect("status");
        assert!(!payload.stats_degraded);
        let renames: Vec<&FileStatus> = payload
            .files
            .iter()
            .filter(|f| f.status_code.starts_with('R'))
            .collect();
        assert_eq!(
            renames.len(),
            1,
            "expected the staged rename, got {renames:?}"
        );
        assert_eq!(renames[0].path, "b.txt");
        assert_eq!(renames[0].old_path.as_deref(), Some("a.txt"));
        assert!(renames[0].is_staged);
    }

    // Item 2: unicode paths must line up between status -z bytes and numstat.

    #[test]
    fn unicode_paths_match_between_status_and_numstat() {
        let dir = init_repo();
        let unicode = "docs/日本語メモ.txt";
        commit_file(dir.path(), unicode, "line1\n", "add unicode");
        write_file(dir.path(), unicode, "line1\nline2\n");

        let payload = GitReader::get_status_payload(&repo_str(dir.path())).expect("status");
        let entry = payload
            .files
            .iter()
            .find(|f| f.path == unicode)
            .expect("unicode path listed verbatim");
        assert_eq!((entry.additions, entry.deletions), (1, 0));

        let oid = run_git(dir.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        let files = GitReader::get_commit_files(&repo_str(dir.path()), &oid).expect("commit files");
        let changed = files
            .iter()
            .find(|f| f.path == unicode)
            .expect("numstat for committed unicode file decoded");
        assert_eq!((changed.additions, changed.deletions), (1, 0));
    }

    #[test]
    fn parse_numstat_decodes_c_quoted_paths_defensively() {
        let map = parse_numstat("12\t34\t\"\\346\\227\\245.txt\"\n5\t6\tplain.txt\n");
        assert_eq!(map.get("日.txt"), Some(&(12, 34)));
        assert_eq!(map.get("plain.txt"), Some(&(5, 6)));

        // Quotes/control characters stay quoted even with quotepath off.
        let tab_map = parse_numstat("7\t8\t\"we\\trd.txt\"\n");
        assert_eq!(tab_map.get("we\trd.txt"), Some(&(7, 8)));

        // Passthrough of unquoted names is untouched.
        assert!(parse_numstat("").is_empty());
    }

    // Item 3: massive-commit diff pipeline.

    #[test]
    fn oversized_and_binary_files_are_skipped_in_commit_diff_payload() {
        let dir = init_repo();
        for name in ["small_a.txt", "small_b.txt", "small_c.txt"] {
            commit_file(dir.path(), name, "original\n", &format!("base {name}"));
        }
        write_file(dir.path(), "small_a.txt", "original\nsmall_a edit\n");
        write_file(dir.path(), "small_b.txt", "original\nsmall_b edit\n");
        write_file(dir.path(), "small_c.txt", "original\nsmall_c edit\n");
        let big_body: String = std::iter::repeat_n("filler line\n", 6001).collect();
        write_file(dir.path(), "big.txt", &big_body);
        write_file(dir.path(), "blob.bin", "\u{0}\u{1}\u{2}binary\u{0}");
        run_git(dir.path(), &["add", "-A"]);
        run_git(dir.path(), &["commit", "-m", "massive"]);
        let oid = run_git(dir.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_string();

        let payload =
            GitReader::get_commit_diff_payload(&repo_str(dir.path()), &oid).expect("payload");
        assert!(payload.truncated);
        assert_eq!(payload.total_files, 5);
        assert_eq!(payload.included_files, 3);
        assert_eq!(payload.skipped_files.len(), 2);

        let big = payload
            .skipped_files
            .iter()
            .find(|f| f.path == "big.txt")
            .expect("oversized text file skipped");
        assert_eq!((big.additions, big.deletions), (6001, 0));
        let bin = payload
            .skipped_files
            .iter()
            .find(|f| f.path == "blob.bin")
            .expect("binary file skipped");
        assert_eq!(bin.status_code, "B");

        // Totals describe the whole commit, including skipped files.
        assert_eq!(payload.total_additions, 6001 + 3);
        assert_eq!(payload.total_deletions, 0);

        for small in ["small_a.txt", "small_b.txt", "small_c.txt"] {
            assert!(
                payload.content.contains(small),
                "content must include the {small} hunk"
            );
        }
        assert!(
            !payload.content.contains("big.txt"),
            "the 6001-line blob must not leak into the rendered diff"
        );
        assert!(!payload.content.contains("blob.bin"));
    }

    #[test]
    fn fitting_commit_diff_matches_raw_git_show_byte_for_byte() {
        let dir = init_repo();
        commit_file(dir.path(), "one.txt", "alpha\nbeta\n", "first");
        let oid = commit_file(dir.path(), "two.txt", "gamma\n", "second");

        let payload =
            GitReader::get_commit_diff_payload(&repo_str(dir.path()), &oid).expect("payload");
        assert!(!payload.truncated);
        assert!(payload.skipped_files.is_empty());
        assert_eq!(payload.total_files, 1);
        assert_eq!(payload.total_additions, 1);
        assert_eq!(payload.total_deletions, 0);

        // Byte-identical to what plain `git show` prints (ASCII paths only, so
        // core.quotepath cannot influence either side).
        let raw = std::process::Command::new("git")
            .args(["show", "--unified=3", "--format=", &oid])
            .current_dir(dir.path())
            .output()
            .expect("spawn raw git show");
        assert!(raw.status.success());
        assert_eq!(
            payload.content,
            String::from_utf8_lossy(&raw.stdout).into_owned()
        );

        // The legacy string wrapper rides the same pipeline.
        assert_eq!(
            GitReader::get_commit_diff(&repo_str(dir.path()), &oid).expect("legacy wrapper"),
            payload.content
        );
    }

    #[test]
    fn parse_numstat_z_handles_plain_binary_and_rename_records() {
        let records = [
            "3\t1\tmod.txt",
            "-\t-\tbin.dat",
            "4\t2\t",
            "old.txt",
            "new.txt",
        ];
        let entries = parse_numstat_z(&records.join("\0"));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "mod.txt");
        assert!(!entries[0].is_binary);
        assert_eq!((entries[0].additions, entries[0].deletions), (3, 1));
        assert_eq!(entries[1].path, "bin.dat");
        assert!(entries[1].is_binary);
        assert_eq!(entries[2].path, "new.txt");
        assert_eq!(entries[2].pathspec, ":(literal)new.txt");
    }

    // Item 3e: changed-file list cap on commit details.

    #[test]
    fn get_commit_details_reports_file_list_truncation() {
        let dir = init_repo();
        commit_file(dir.path(), "base.txt", "base\n", "root");
        commit_file(dir.path(), "small.txt", "small\n", "one file");
        // One commit changing more than COMMIT_FILES_LIST_CAP files at once.
        for i in 0..(COMMIT_FILES_LIST_CAP + 10) {
            write_file(dir.path(), &format!("f{i:04}.txt"), "x\n");
        }
        run_git(dir.path(), &["add", "-A"]);
        run_git(dir.path(), &["commit", "-m", "bulk"]);
        let oid = run_git(dir.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        let details = GitReader::get_commit_details(&repo_str(dir.path()), &oid).expect("details");
        assert!(details.files_list_truncated);
        assert_eq!(details.files_total_count, COMMIT_FILES_LIST_CAP + 10);
        assert_eq!(details.changed_files.len(), COMMIT_FILES_LIST_CAP);
        // Totals still cover every file, not just the shipped slice.
        assert_eq!(details.total_additions, COMMIT_FILES_LIST_CAP + 10);
        // A small commit keeps the flags quiet.
        let base = run_git(dir.path(), &["rev-parse", "HEAD~1"])
            .trim()
            .to_string();
        let small =
            GitReader::get_commit_details(&repo_str(dir.path()), &base).expect("small details");
        assert!(!small.files_list_truncated);
        assert_eq!(small.files_total_count, 1);
        assert_eq!(small.changed_files.len(), 1);
    }

    // Item 4 lives in git_cli.rs; NETWORK_TIMEOUT wiring is exported there.

    // Item 5: history pagination probe.

    #[test]
    fn history_probe_detects_more_pages_across_the_boundary() {
        let dir = init_repo();
        let mut last = String::new();
        for i in 0..5 {
            last = commit_file(dir.path(), &format!("p{i}.txt"), "x\n", &format!("c{i}"));
        }
        let (rows, has_more) =
            GitReader::read_commit_history_probe(&repo_str(dir.path()), 0, 5, None).expect("probe");
        assert!(!has_more);
        assert_eq!(rows.len(), 5);

        commit_file(dir.path(), "extra.txt", "x\n", "sixth");
        let (rows, has_more) =
            GitReader::read_commit_history_probe(&repo_str(dir.path()), 0, 5, None)
                .expect("probe 2");
        assert!(has_more);
        assert_eq!(rows.len(), 5);
        assert_ne!(rows[0].id, last, "newest row is the sixth commit");
        let _ = last;

        // At exactly MAX_HISTORY_COMMITS the probe still fetches cap+1 rows,
        // so has_more stays truthful — the old paged clamp swallowed it.
        let (rows, has_more) = GitReader::read_commit_history_probe(
            &repo_str(dir.path()),
            0,
            GitReader::MAX_HISTORY_COMMITS,
            None,
        )
        .expect("probe at cap");
        assert!(!has_more);
        assert_eq!(rows.len(), 6);
    }

    // Item 6: tag decoration cap.

    #[test]
    fn tag_listing_caps_at_newest_two_hundred_tags() {
        let dir = init_repo();
        let total = REFS_TAG_CAP + 50;
        for i in 0..total {
            let date = 1_700_000_000 + i as i64;
            let name = format!("f{i:04}.txt");
            write_file(dir.path(), &name, "x\n");
            run_git(dir.path(), &["add", &name]);
            run_git_at(dir.path(), date, &["commit", "-m", &format!("c{i}")]);
            run_git_at(dir.path(), date, &["tag", &format!("tag-{i:03}")]);
        }

        let summary = GitReader::list_tags_summary(&repo_str(dir.path())).expect("tag summary");
        assert!(summary.tags_truncated);
        assert_eq!(summary.total_tags, total);
        assert_eq!(summary.tags.len(), REFS_TAG_CAP);
        // Newest by creatordate: tag-249 .. tag-050.
        assert_eq!(summary.tags[0].name, format!("tag-{:03}", total - 1));
        assert_eq!(
            summary.tags.last().unwrap().name,
            format!("tag-{:03}", total - REFS_TAG_CAP)
        );

        // Legacy listing: same capped membership, historical alphabetical order.
        let legacy = GitReader::list_tags(&repo_str(dir.path())).expect("legacy tags");
        assert_eq!(legacy.len(), REFS_TAG_CAP);
        let names: Vec<&str> = legacy.iter().map(|t| t.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "legacy order remains alphabetical");
        let expected_last = format!("tag-{:03}", total - REFS_TAG_CAP);
        assert!(legacy
            .iter()
            .all(|t| t.name.as_str() >= expected_last.as_str()));
        assert!(legacy
            .iter()
            .any(|t| t.name == format!("tag-{:03}", total - 1)));
    }

    // Item 7: ranged blame.

    #[test]
    fn blame_range_returns_requested_window_with_numbers() {
        let dir = init_repo();
        let body: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        let _oid = commit_file(dir.path(), "code.rs", &body, "ten lines");

        let result = GitReader::get_blame_range(&repo_str(dir.path()), "code.rs", 4, 6)
            .expect("range blame");
        assert!(!result.truncated);
        assert_eq!(result.lines.len(), 3);
        assert_eq!(result.lines[0].line_no, 4);
        assert_eq!(result.lines[1].line_no, 5);
        assert_eq!(result.lines[2].line_no, 6);
        assert_eq!(result.lines[0].content, "line 4");
        assert_eq!(result.lines[2].content, "line 6");

        // Requests beyond BLAME_MAX_RANGE_LINES clamp and flag truncation.
        let result = GitReader::get_blame_range(
            &repo_str(dir.path()),
            "code.rs",
            1,
            BLAME_MAX_RANGE_LINES + 10,
        )
        .expect("clamped range");
        assert!(result.truncated);
        assert_eq!(result.lines.len(), 10, "file only has ten lines");
        assert_eq!(result.lines[0].line_no, 1);

        assert!(GitReader::get_blame_range(&repo_str(dir.path()), "code.rs", 0, 5).is_err());
        assert!(GitReader::get_blame_range(&repo_str(dir.path()), "code.rs", 5, 4).is_err());

        // Legacy whole-file blame is untouched and 1-based.
        let full = GitReader::get_file_blame(&repo_str(dir.path()), "code.rs").expect("full blame");
        assert_eq!(full.len(), 10);
        assert_eq!(full[0].line_no, 1);
        assert_eq!(full[9].content, "line 10");
    }

    // Item 8: degraded numstat reporting.

    #[test]
    fn double_numstat_failure_marks_status_degraded_but_keeps_paths() {
        let stdout = "M  tracked.txt\0?? untracked.txt\0";
        let always_fails = |_repo: &Path, _cached: bool| (HashMap::new(), true);
        let payload = GitReader::status_from_porcelain(Path::new("/unused"), stdout, always_fails)
            .expect("degraded status");
        assert!(payload.stats_degraded);
        assert_eq!(payload.files.len(), 2);
        assert_eq!(payload.files[0].path, "tracked.txt");
        assert_eq!(
            (payload.files[0].additions, payload.files[0].deletions),
            (0, 0)
        );
        assert_eq!(payload.files[1].status_code, "??");
        assert!(!payload.files[1].is_staged);

        // A healthy fetcher leaves the flag off and numbers intact.
        let payload =
            GitReader::status_from_porcelain(Path::new("/unused"), stdout, numstat_ok(true))
                .expect("healthy status");
        assert!(!payload.stats_degraded);
        assert_eq!(
            (payload.files[0].additions, payload.files[0].deletions),
            (3, 4)
        );
    }

    #[test]
    fn get_status_payload_is_not_degraded_on_a_healthy_repo() {
        let dir = init_repo();
        commit_file(dir.path(), "ok.txt", "fine\n", "clean");
        let payload = GitReader::get_status_payload(&repo_str(dir.path())).expect("status");
        assert!(!payload.stats_degraded);
        assert!(payload.files.is_empty());
    }

    // Item 2 (cli side): every invocation must carry the quoting config.

    #[test]
    fn show_output_for_unicode_paths_is_unquoted_end_to_end() {
        let dir = init_repo();
        let unicode = "レポート.md";
        let oid = commit_file(dir.path(), unicode, "hello\n", "unicode commit");
        let files = GitReader::get_commit_files(&repo_str(dir.path()), &oid).expect("files");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, unicode, "no C-style quoting may survive");
        let diff =
            GitReader::get_commit_diff_payload(&repo_str(dir.path()), &oid).expect("diff payload");
        assert!(diff.content.contains(unicode));
    }
}
