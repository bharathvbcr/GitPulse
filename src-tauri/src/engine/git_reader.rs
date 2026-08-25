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
        let origin_head =
            git_text(&repo, &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]).ok();
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
            &["for-each-ref", format_arg.as_str(), "refs/heads/", "refs/remotes/"],
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
        let origin_head =
            git_text(&repo, &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]).ok();

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
                let (churn, was_cached) = cached_branch_churn(
                    &repo_key,
                    &base_oid,
                    &target.tip_commit_id,
                    || compute_branch_churn(&repo, &base_oid, &target.tip_commit_id),
                );
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
        let count = max_count.clamp(1, Self::MAX_HISTORY_COMMITS).to_string();
        let skipped = skip.min(Self::MAX_HISTORY_COMMITS).to_string();
        let count_arg = format!("-n{}", count);
        let skip_arg = format!("--skip={}", skipped);
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
        let stdout = git_text(&repo, &args)?;

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
        let repo = validate_repo(repo_path)?;
        let stdout = git_text(&repo, &["status", "--porcelain=v1", "-z"])?;
        let numstat_work =
            parse_numstat(&git_text(&repo, &["diff", "--numstat"]).unwrap_or_default());
        let numstat_index =
            parse_numstat(&git_text(&repo, &["diff", "--cached", "--numstat"]).unwrap_or_default());

        let mut statuses = Vec::new();
        let bytes = stdout.into_bytes();
        let mut i = 0;
        while i + 3 < bytes.len() {
            let index_status = bytes[i] as char;
            let work_status = bytes[i + 1] as char;
            i += 3;
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
                let old = rest.clone();
                let new = match bytes.get(i..) {
                    Some(slice) => match slice.iter().position(|&b| b == 0) {
                        Some(n) => {
                            let s = String::from_utf8_lossy(&slice[..n]).into_owned();
                            i += n + 1;
                            s
                        }
                        None => rest.clone(),
                    },
                    None => rest.clone(),
                };
                (new, Some(old))
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
        Ok(statuses)
    }

    pub fn get_file_blame(repo_path: &str, file_path: &str) -> Result<Vec<BlameLine>, String> {
        let repo = validate_repo(repo_path)?;
        let _ = sandbox_join(&repo, file_path)?;
        let stdout = git_text(&repo, &["blame", "--line-porcelain", "--", file_path])?;

        let mut blame_lines = Vec::new();
        let mut current_sha = String::new();
        let mut current_author = String::new();
        let mut current_email = String::new();
        let mut current_time: i64 = 0;
        let mut line_count = 1;

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
        Ok(blame_lines)
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

    pub fn get_commit_diff(repo_path: &str, commit_id: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        validate_oid(commit_id)?;
        git_text(&repo, &["show", "--unified=3", "--format=", commit_id])
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
        let stdout = git_text(&repo, &["show", "--pretty=format:", "--numstat", commit_id])?;
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
        let changed_files = Self::get_commit_files(repo_path, commit_id).unwrap_or_default();
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

fn parse_numstat(stdout: &str) -> HashMap<String, (usize, usize)> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            let add = parts[0].parse().unwrap_or(0);
            let del = parts[1].parse().unwrap_or(0);
            let path = parts[2]
                .rsplit(" => ")
                .next()
                .unwrap_or(parts[2])
                .to_string();
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
        let path = parts[2]
            .rsplit(" => ")
            .next()
            .unwrap_or(parts[2])
            .to_string();
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
}
