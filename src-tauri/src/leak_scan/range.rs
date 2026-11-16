//! Secret scan of what a push sends, and the push gate on it.
//!
//! A push sends every commit reachable from the refs it updates that the
//! remote does not already have. For each of those commits this reads what
//! the commit itself added — the added lines of its diff against its parent,
//! and the whole blob of every file git calls binary — the way
//! [`super::staged`] reads the index: the same parser ([`parse_patch`],
//! [`parse_raw`]), the same detector ([`Tally`]) and the same blob reader
//! ([`super::staged::scan_blobs`]).
//!
//! **What "the remote already has" means here.** Everything reachable from
//! the remote's tracking refs, `refs/remotes/<remote>/*`, as the last fetch
//! recorded them. For a branch with no upstream that already covers the
//! merge-base with the default branch (it is reachable from
//! `refs/remotes/<remote>/<default>`), and it is tighter: commits that are on
//! any branch the remote has are left out too, so a new branch cut from a
//! teammate's published branch is scanned only for its own commits. A remote
//! with no tracking refs (a new, empty remote) has nothing, and every
//! reachable commit is in range, up to [`RangeLimits::commits`].
//!
//! One case reads less than git sends: a branch deleted on the remote whose
//! tracking ref was never pruned still counts as the remote's. A stale ref in
//! the other direction (the remote moved on) only makes the scan read more.
//!
//! **Which refs.** The branch the push names; otherwise what git would push
//! with no arguments, always read as a superset of what it sends:
//! - `remote.<name>.mirror`: every ref under `refs/`. A mirror sends
//!   backups, snapshots and notes too, not only branches and tags.
//! - `remote.<name>.push` refspecs: each one's source. A glob reads every ref
//!   under its prefix, `:` reads every branch and tag, and a plain source
//!   reads the revision it names.
//! - `push.default=matching`: every local branch and tag.
//! - Otherwise: the current branch.
//!
//! A ref pointing at a tree or blob rather than a commit is counted as a skip.
//!
//! **Merges.** A merge is read with `--remerge-diff`: its diff against what
//! git's own merge of its parents would have produced, so the lines a person
//! added while resolving it (or slipped into it) are read, and the parents'
//! own changes are not read again. Git does not remerge an octopus merge
//! (three or more parents); one is counted as a skip, never passed.
//!
//! **Bounds.** At most [`RangeLimits::commits`] commits, [`RangeLimits::
//! patch_bytes`] bytes of patch and [`RangeLimits::time`] of git time per
//! scan, plus [`super::staged`]'s blob budgets. Each bound reached is a
//! counted skip, and a skip refuses the push like a finding does.
//!
//! **Not covered.** Commit messages and tag messages; a push GitPulse does
//! not run (a shell or an agent's `git push`, the console's argv); and a ref
//! that moves between this scan and the push, which sends what the ref then
//! holds. Missing objects are not fetched from a promisor remote: a commit
//! whose objects are not all here fails the scan.

use super::staged::{refusal, scan_blobs, Gate, Policy, ScanFailure};
use super::{note_scan_completed, parse_patch, parse_raw, RawEntry, SkipReason, Tally, ENGINE};
use crate::engine::git_cli::{git, git_captured, git_observed, git_with_stdin, without_lazy_fetch};
use crate::engine::process::{is_timed_out, Incomplete, DEFAULT_TIMEOUT, MAX_OUTPUT_BYTES};
use crate::ipc_error::CommandError;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

/// The bounds one push scan works within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeLimits {
    /// Commits read, newest first. The rest are one counted skip.
    pub commits: usize,
    /// Bytes of `git log -p` output read across the whole scan.
    pub patch_bytes: u64,
    /// Wall time for the commits' diffs. A batch git has not finished by then
    /// is stopped, and it and every commit after it are counted as skipped.
    pub time: Duration,
}

impl RangeLimits {
    /// What a push from GitPulse is scanned within.
    ///
    /// Measured by `benches/leak_scan.rs` (2026-11-15, 18 CPUs, load 16):
    /// 20 commits of 50 added lines scan in 228 ms p50 and 500 commits
    /// (2.3 MB of patch) in 792 ms p50. So the commit limit is reached at
    /// about 2.6 s of scanning on such commits, the time limit is for
    /// large commits or a starved host, and the byte limit sits about a
    /// hundred times above the 500-commit row.
    pub const DEFAULT: Self = Self {
        commits: 2_000,
        patch_bytes: 256 * 1024 * 1024,
        time: Duration::from_secs(60),
    };
}

/// Commits read per `git log` run. A batch whose patch overflows the
/// runner's output cap is split in half and read again.
const BATCH_COMMITS: usize = 64;

/// Which refs a push updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushRefs<'a> {
    /// `git push [<remote> [<branch>]]`: the named branch, or what git
    /// pushes when none is named.
    Branch(Option<&'a str>),
    /// One tag, `refs/tags/<name>`.
    Tag(&'a str),
}

/// What a push would send, by object id: the commits reachable from
/// `include` and from none of `exclude`. Resolved once, so a ref moved by
/// someone else while the scan runs cannot change what is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRange {
    pub remote: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Refs the push would send that do not point at a commit.
    pub non_commits: Vec<String>,
}

/// A completed scan of a push's commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeScan {
    pub tally: Tally,
    /// Commits whose diffs were read.
    pub commits_scanned: u64,
    pub duration_ms: u64,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// A git read whose non-zero exit is an answer ("not set"), not a failure.
fn optional(repo: &Path, args: &[&str]) -> Result<Option<String>, ScanFailure> {
    let run = git_captured(repo, args)
        .map_err(|_| ScanFailure("git could not read which refs the push sends".into()))?;
    if run.incomplete.is_some() {
        return Err(ScanFailure(
            "git's answer about which refs the push sends was cut short".into(),
        ));
    }
    Ok(match run.status_code {
        0 => Some(text(&run.stdout)),
        1 => None,
        _ => {
            return Err(ScanFailure(
                "git could not read which refs the push sends".into(),
            ))
        }
    })
}

/// Commit ids the refs matching `patterns` point at, peeled through tags;
/// refs that do not end at a commit go to `non_commits`.
fn ref_commits(
    repo: &Path,
    patterns: &[&str],
    include: &mut Vec<String>,
    non_commits: &mut Vec<String>,
) -> Result<(), ScanFailure> {
    let mut args = vec![
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(*objectname)%00%(*objecttype)",
    ];
    args.extend_from_slice(patterns);
    let out = git(repo, &args)
        .map_err(|_| ScanFailure("git could not list the refs the push would send".into()))?;
    for line in out.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let fields: Vec<String> = line
            .split(|b| *b == 0)
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        let [name, oid, kind, peeled, peeled_kind] = fields.as_slice() else {
            return Err(ScanFailure(
                "git listed the refs the push would send in an unexpected form".into(),
            ));
        };
        match (kind.as_str(), peeled_kind.as_str()) {
            ("commit", _) => include.push(oid.clone()),
            ("tag", "commit") => include.push(peeled.clone()),
            _ => non_commits.push(name.clone()),
        }
    }
    Ok(())
}

/// The commit a pushed name resolves to. Git resolves it the way it resolves
/// any revision, so a name with no branch behind it can still push a tag.
fn named_commit(
    repo: &Path,
    name: &str,
    include: &mut Vec<String>,
    non_commits: &mut Vec<String>,
) -> Result<(), ScanFailure> {
    if let Some(oid) = optional(repo, &["rev-parse", "--verify", "--quiet", name])? {
        let peeled = format!("{oid}^{{commit}}");
        match optional(repo, &["rev-parse", "--verify", "--quiet", &peeled])? {
            Some(commit) => include.push(commit),
            None => non_commits.push(name.to_string()),
        }
    }
    Ok(())
}

/// What one configured `remote.<name>.push` refspec sends, read as a
/// superset: a glob source reads every ref under its prefix, `:` (matching)
/// every branch and tag, and a plain source the revision it names. A refspec
/// with no source deletes a remote ref and sends nothing.
fn refspec_commits(
    repo: &Path,
    refspec: &str,
    include: &mut Vec<String>,
    non_commits: &mut Vec<String>,
) -> Result<(), ScanFailure> {
    let spec = refspec.trim_start_matches('+');
    if spec.starts_with('^') {
        // A negative refspec only narrows the others; reading more is safe.
        return Ok(());
    }
    if spec == ":" {
        return ref_commits(repo, &["refs/heads/", "refs/tags/"], include, non_commits);
    }
    let source = spec.split_once(':').map_or(spec, |(source, _)| source);
    if source.is_empty() {
        return Ok(());
    }
    match source.split_once('*') {
        Some((prefix, _)) => {
            // `for-each-ref` matches a pattern ending in `/` as a prefix.
            let prefix = match prefix.rfind('/') {
                Some(slash) => &prefix[..=slash],
                None => "refs/",
            };
            ref_commits(repo, &[prefix], include, non_commits)
        }
        None => named_commit(repo, source, include, non_commits),
    }
}

/// Resolve what a push of `refs` to `remote` (or the remote git would push
/// to) sends. See the module documentation for the rules.
pub fn resolve_push(
    repo: &Path,
    remote: Option<&str>,
    refs: PushRefs<'_>,
) -> Result<PushRange, ScanFailure> {
    let head = text(
        &git(repo, &["rev-parse", "--symbolic-full-name", "HEAD"])
            .map_err(|_| ScanFailure("git could not read the current branch".into()))?,
    );
    let current = head.starts_with("refs/heads/").then_some(head.as_str());
    let remote = match remote {
        Some(remote) => remote.to_string(),
        None => {
            let branch = current.ok_or_else(|| {
                ScanFailure("a push without a named remote needs a checked-out branch".into())
            })?;
            let named = text(
                &git(
                    repo,
                    &["for-each-ref", "--format=%(push:remotename)", branch],
                )
                .map_err(|_| ScanFailure("git could not tell where this push goes".into()))?,
            );
            if named.is_empty() {
                return Err(ScanFailure(
                    "git could not tell where this push goes".into(),
                ));
            }
            named
        }
    };

    let mut include = Vec::new();
    let mut non_commits = Vec::new();
    match refs {
        PushRefs::Tag(tag) => {
            let pattern = format!("refs/tags/{tag}");
            ref_commits(repo, &[&pattern], &mut include, &mut non_commits)?;
        }
        PushRefs::Branch(Some(branch)) => {
            named_commit(repo, branch, &mut include, &mut non_commits)?;
        }
        PushRefs::Branch(None) => {
            let refspecs = optional(
                repo,
                &["config", "--get-all", &format!("remote.{remote}.push")],
            )?;
            let mirror = optional(
                repo,
                &[
                    "config",
                    "--type=bool",
                    "--get",
                    &format!("remote.{remote}.mirror"),
                ],
            )?;
            let default = optional(repo, &["config", "--get", "push.default"])?;
            let refspecs = refspecs.filter(|r| !r.trim().is_empty());
            if mirror.as_deref() == Some("true") {
                // `--mirror` sends every ref under refs/, not only branches
                // and tags: backups, snapshots, notes, other remotes' refs.
                ref_commits(repo, &["refs/"], &mut include, &mut non_commits)?;
            } else if let Some(refspecs) = refspecs {
                for refspec in refspecs.lines().map(str::trim).filter(|r| !r.is_empty()) {
                    refspec_commits(repo, refspec, &mut include, &mut non_commits)?;
                }
            } else if default.as_deref() == Some("matching") {
                ref_commits(
                    repo,
                    &["refs/heads/", "refs/tags/"],
                    &mut include,
                    &mut non_commits,
                )?;
            } else {
                let branch = current.ok_or_else(|| {
                    ScanFailure("a push of the current branch needs a checked-out branch".into())
                })?;
                ref_commits(repo, &[branch], &mut include, &mut non_commits)?;
            }
        }
    }
    // A push of refs that do not exist sends nothing; git says so itself.

    // The remote's tracking refs, without those of another remote whose name
    // nests under this one's (`team` and `team/fork`).
    let names =
        git(repo, &["remote"]).map_err(|_| ScanFailure("git could not list the remotes".into()))?;
    let nested: Vec<String> = String::from_utf8_lossy(&names)
        .lines()
        .map(str::trim)
        .filter(|other| other.starts_with(&format!("{remote}/")))
        .map(|other| format!("refs/remotes/{other}/"))
        .collect();
    let tracking = format!("refs/remotes/{remote}/");
    let out = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            &tracking,
        ],
    )
    .map_err(|_| ScanFailure("git could not list what the remote already has".into()))?;
    let mut exclude = Vec::new();
    for line in out.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let line = String::from_utf8_lossy(line);
        let Some((name, oid)) = line.split_once('\0') else {
            return Err(ScanFailure(
                "git listed the remote's refs in an unexpected form".into(),
            ));
        };
        if !nested
            .iter()
            .any(|prefix| name.starts_with(prefix.as_str()))
        {
            exclude.push(oid.to_string());
        }
    }
    Ok(PushRange {
        remote,
        include,
        exclude,
        non_commits,
    })
}

/// One commit in range: its id and how many parents it has.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Commit {
    oid: String,
    parents: usize,
}

impl Commit {
    fn short(&self) -> &str {
        &self.oid[..self.oid.len().min(12)]
    }
}

/// The commits in range, newest first, at most `limit` of them, and how many
/// more there are.
fn commits_in(
    repo: &Path,
    range: &PushRange,
    limit: usize,
) -> Result<(Vec<Commit>, u64), ScanFailure> {
    let mut request = Vec::new();
    for oid in &range.include {
        request.extend_from_slice(oid.as_bytes());
        request.push(b'\n');
    }
    for oid in &range.exclude {
        request.push(b'^');
        request.extend_from_slice(oid.as_bytes());
        request.push(b'\n');
    }
    let max = format!("--max-count={}", limit.saturating_add(1));
    let out = without_lazy_fetch(|| {
        git_with_stdin(repo, &["rev-list", "--parents", &max, "--stdin"], &request)
    })
    .map_err(|_| ScanFailure("git could not list the commits this push would send".into()))?;
    let mut commits = Vec::new();
    for line in String::from_utf8_lossy(&out).lines() {
        let mut ids = line.split(' ');
        let Some(oid) = ids.next().filter(|id| !id.is_empty()) else {
            return Err(ScanFailure(
                "git listed the push's commits in an unexpected form".into(),
            ));
        };
        commits.push(Commit {
            oid: oid.to_string(),
            parents: ids.count(),
        });
    }
    if commits.len() <= limit {
        return Ok((commits, 0));
    }
    commits.truncate(limit);
    let counted =
        without_lazy_fetch(|| git_with_stdin(repo, &["rev-list", "--count", "--stdin"], &request))
            .map_err(|_| {
                ScanFailure("git could not count the commits this push would send".into())
            })?;
    let total: u64 = text(&counted)
        .parse()
        .map_err(|_| ScanFailure("git counted the push's commits in an unexpected form".into()))?;
    Ok((commits, total.saturating_sub(limit as u64)))
}

/// Options for both listings of a batch, so the user's configuration cannot
/// change what is read and both list the same files in the same order.
const LOG_OPTIONS: [&str; 12] = [
    "log",
    "--no-walk=unsorted",
    "--root",
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
    "--no-relative",
    "--no-notes",
    "--no-show-signature",
    "--submodule=short",
    "-M",
    "--diff-merges=remerge",
];

/// The header each commit's listing starts with: a NUL, which no line of a
/// patch and no field of a raw listing can start with.
const HEADER: &[u8] = b"\0C ";

/// How one git run over a batch ended, when it did not fail outright.
enum Run {
    Done(Vec<u8>),
    OverCap,
    TimedOut,
}

fn run_log(repo: &Path, args: &[&str], timeout: Duration) -> Result<Run, ScanFailure> {
    let failed =
        || ScanFailure("git could not produce the diff of the commits this push would send".into());
    let run =
        match without_lazy_fetch(|| git_observed(repo, args, timeout, MAX_OUTPUT_BYTES, &mut ())) {
            Ok(run) => run,
            Err(error) if is_timed_out(error.message()) => return Ok(Run::TimedOut),
            Err(_) => return Err(failed()),
        };
    match run.incomplete {
        None if run.success => Ok(Run::Done(run.stdout)),
        None => Err(failed()),
        Some(Incomplete::OverCap(_)) => Ok(Run::OverCap),
        Some(Incomplete::Deadline { .. }) => Ok(Run::TimedOut),
        Some(Incomplete::Unread(_)) => Err(failed()),
    }
}

/// Split a batch's listing into one body per commit, checking the headers
/// name exactly `batch`, in order. `in_raw` says the listing is `--raw -z`,
/// where the header is a NUL-terminated field; in a patch it is a line.
fn split_listing<'a>(
    listing: &'a [u8],
    batch: &[&Commit],
    in_raw: bool,
) -> Result<Vec<&'a [u8]>, ScanFailure> {
    let unexpected = || ScanFailure("git listed the push's commits in an unexpected form".into());
    // A header starts the listing or follows the end of a line. In a raw
    // listing it also follows the end of a field; after a commit with no
    // entries git ends the header's field with a newline instead. A file
    // name that happens to end the same way can only fail the check below
    // that each header names the commit expected next.
    let after_boundary = |byte: u8| byte == b'\n' || (in_raw && byte == 0);
    let mut starts = Vec::new();
    let mut at = 0usize;
    while let Some(found) = listing[at..]
        .windows(HEADER.len())
        .position(|w| w == HEADER)
    {
        let pos = at + found;
        if pos == 0 || after_boundary(listing[pos - 1]) {
            starts.push(pos);
        }
        at = pos + 1;
    }
    if starts.first() != Some(&0) || starts.len() != batch.len() {
        return Err(unexpected());
    }
    let terminator = if in_raw { 0u8 } else { b'\n' };
    let mut bodies = Vec::with_capacity(batch.len());
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(listing.len());
        let section = &listing[*start + 1..end];
        let header_end = section
            .iter()
            .position(|b| *b == terminator)
            .unwrap_or(section.len());
        let header = String::from_utf8_lossy(&section[..header_end]);
        let named = header.strip_prefix("C ").and_then(|h| h.split(' ').next());
        if named != Some(batch[index].oid.as_str()) {
            return Err(unexpected());
        }
        let mut body = &section[(header_end + 1).min(section.len())..];
        // A blank line (patch) or newline (raw) separates header and body.
        if let Some(rest) = body.strip_prefix(b"\n") {
            body = rest;
        }
        bodies.push(body);
    }
    Ok(bodies)
}

/// Scan the push's commits, newest first, within `limits`.
pub fn scan_range(
    repo: &Path,
    range: &PushRange,
    limits: RangeLimits,
) -> Result<RangeScan, ScanFailure> {
    let started = Instant::now();
    let deadline = started + limits.time;
    let mut tally = Tally::default();
    for name in &range.non_commits {
        tally.skip(name, SkipReason::Unsupported, None);
    }
    let (commits, past_limit) = commits_in(repo, range, limits.commits)?;
    if past_limit > 0 {
        tally.skip_many(
            &format!(
                "{past_limit} commits older than {}",
                commits.last().map_or("", Commit::short)
            ),
            SkipReason::CommitLimit,
            None,
            past_limit,
        );
    }
    let mut queue: Vec<Vec<&Commit>> = Vec::new();
    let mut readable: Vec<&Commit> = Vec::new();
    for commit in &commits {
        if commit.parents > 2 {
            tally.skip(commit.short(), SkipReason::Unsupported, None);
        } else {
            readable.push(commit);
        }
    }
    // Batches newest first; the queue is a stack, so push them in reverse.
    for batch in readable.chunks(BATCH_COMMITS).rev() {
        queue.push(batch.to_vec());
    }

    let mut patch_bytes = 0u64;
    let mut commits_scanned = 0u64;
    let mut blobs: Vec<RawEntry> = Vec::new();
    let mut seen_blobs: HashSet<String> = HashSet::new();
    while let Some(batch) = queue.pop() {
        let unread = |queue: &[Vec<&Commit>], batch: &[&Commit]| {
            batch.len() as u64 + queue.iter().map(|b| b.len() as u64).sum::<u64>()
        };
        let now = Instant::now();
        if now >= deadline || patch_bytes >= limits.patch_bytes {
            let reason = if now >= deadline {
                SkipReason::TimeLimit
            } else {
                SkipReason::BudgetExhausted
            };
            let left = unread(&queue, &batch);
            tally.skip_many(
                &format!("{left} commits from {} on", batch[0].short()),
                reason,
                None,
                left,
            );
            break;
        }
        let timeout = (deadline - now).min(DEFAULT_TIMEOUT);
        let oids: Vec<&str> = batch.iter().map(|c| c.oid.as_str()).collect();

        let mut patch_args = LOG_OPTIONS.to_vec();
        patch_args.extend([
            "--format=%x00C %H %P",
            "-p",
            "-U0",
            "--inter-hunk-context=0",
        ]);
        patch_args.extend(&oids);
        let mut raw_args = LOG_OPTIONS.to_vec();
        raw_args.extend(["--format=%x00C %H", "--raw", "-z", "--no-abbrev"]);
        raw_args.extend(&oids);

        let outcome = match run_log(repo, &patch_args, timeout)? {
            Run::Done(patch) => match run_log(repo, &raw_args, timeout)? {
                Run::Done(raw) => Ok((patch, raw)),
                other => Err(other),
            },
            other => Err(other),
        };
        let (patch, raw) = match outcome {
            Ok(pair) => pair,
            Err(Run::OverCap) if batch.len() > 1 => {
                let (newer, older) = batch.split_at(batch.len() / 2);
                queue.push(older.to_vec());
                queue.push(newer.to_vec());
                continue;
            }
            Err(Run::OverCap) => {
                tally.skip(batch[0].short(), SkipReason::Oversized, None);
                continue;
            }
            Err(Run::TimedOut | Run::Done(_)) => {
                let left = unread(&queue, &batch);
                tally.skip_many(
                    &format!("{left} commits from {} on", batch[0].short()),
                    SkipReason::TimeLimit,
                    None,
                    left,
                );
                break;
            }
        };
        patch_bytes += patch.len() as u64;
        let patches = split_listing(&patch, &batch, false)?;
        let listings = split_listing(&raw, &batch, true)?;
        for ((commit, body), listing) in batch.iter().zip(patches).zip(listings) {
            let sections = parse_patch(body).map_err(|e| ScanFailure(e.describe().into()))?;
            let sections: Vec<_> = sections.into_iter().filter(|s| !s.remerge_only).collect();
            let entries = parse_raw(listing).ok_or_else(|| {
                ScanFailure("a commit's change listing was not in the expected form".into())
            })?;
            if sections.len() != entries.len() {
                // Which lines belong to which file is unknown. A merge's
                // remerge listing is the one known way to get here.
                if commit.parents > 1 {
                    tally.skip(commit.short(), SkipReason::Unsupported, None);
                    continue;
                }
                return Err(ScanFailure(format!(
                    "commit {}'s diff named {} files and its listing {}",
                    commit.short(),
                    sections.len(),
                    entries.len()
                )));
            }
            commits_scanned += 1;
            for (section, entry) in sections.iter().zip(entries) {
                let path = format!("{}:{}", commit.short(), entry.path);
                if section.binary {
                    if entry.has_blob() && seen_blobs.insert(entry.dst_oid.clone()) {
                        blobs.push(RawEntry { path, ..entry });
                    }
                    continue;
                }
                if section.added.is_empty() {
                    continue;
                }
                tally.inputs_scanned += 1;
                for (line_no, bytes) in &section.added {
                    tally.scan_line(&path, *line_no, &body[bytes.clone()]);
                }
            }
        }
    }
    if !blobs.is_empty() {
        let refs: Vec<&RawEntry> = blobs.iter().collect();
        without_lazy_fetch(|| scan_blobs(repo, &refs, &mut tally))?;
    }
    note_scan_completed();
    Ok(RangeScan {
        tally,
        commits_scanned,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// The push gate. `Ok` means every commit the push would send was scanned
/// and is clean, or a person overrode the gate. Run it under the repository
/// lock, just before the push, so GitPulse's own writes cannot move the refs
/// in between.
pub fn guard_push(
    repo: &Path,
    remote: Option<&str>,
    refs: PushRefs<'_>,
    policy: Policy,
) -> Result<(), CommandError> {
    guard_push_within(repo, remote, refs, policy, RangeLimits::DEFAULT)
}

/// [`guard_push`] within `limits` rather than the default ones.
pub fn guard_push_within(
    repo: &Path,
    remote: Option<&str>,
    refs: PushRefs<'_>,
    policy: Policy,
    limits: RangeLimits,
) -> Result<(), CommandError> {
    if policy == Policy::HumanOverride {
        log::warn!(
            target: "leak_scan",
            "push proceeds past the secret scan on a person's explicit override"
        );
        return Ok(());
    }
    let scan = resolve_push(repo, remote, refs).and_then(|range| scan_range(repo, &range, limits));
    match scan {
        Ok(scan) if scan.tally.is_clean() => {
            log::info!(
                target: "leak_scan",
                "push scanned clean by {ENGINE}: {} commits, {} inputs, {} lines, {} bytes in {} ms",
                scan.commits_scanned,
                scan.tally.inputs_scanned,
                scan.tally.lines_scanned,
                scan.tally.bytes_scanned,
                scan.duration_ms
            );
            Ok(())
        }
        Ok(scan) => Err(refusal(Gate::Push, Some(&scan.tally), None)),
        Err(failure) => Err(refusal(Gate::Push, None, Some(&failure))),
    }
}

#[cfg(test)]
#[path = "range_tests.rs"]
mod tests;
