//! A commit message computed from the staged patch.
//!
//! Apple's on-device model and the smaller local servers share one limit: the
//! patch is the wrong thing to hand them. The on-device window is 4,096 tokens
//! for the instructions, the prompt and the reply together, and a real diff
//! spends that on context the model then has to re-derive. clog, git-cliff and
//! git-journal already show the other direction — a changelog is a rendering of
//! structured commits, not a paragraph a model invents. This module is that
//! structure for the commit that has not been written yet.
//!
//! The patch is parsed here. Type, scope and the subject are fixed here when
//! the patch is unambiguous, and the body is always written here. A model, when
//! one is available, is asked only to phrase the subject from the brief. A
//! reply that adds an issue number, a trailer, a breaking-change mark or a
//! different type prefix is declined, and the message this module already
//! wrote is what the composer gets.

use std::collections::BTreeSet;

use super::prompt;

/// Commit subjects stay on one line. 72 is the length git itself teaches.
pub const SUBJECT_MAX_CHARS: usize = 72;

/// Characters of brief handed to a model.
///
/// Far under the on-device window on purpose: the instructions and the reply
/// share those 4,096 tokens, and a small model phrases worse as the window
/// fills. 1,500 characters is a few hundred tokens even at the conservative
/// three characters per token used for the loopback budget.
pub const BRIEF_MAX_CHARS: usize = 1_500;

const FILE_CAP: usize = 64;
const BODY_FILES: usize = 8;
const BRIEF_FILES: usize = 24;
const NAME_CAP: usize = 5;
const MENTION_CAP: usize = 64;
const SMALL_CHANGE_LINES: u32 = 30;

const KNOWN_TYPES: &[&str] = &[
    "feat", "fix", "refactor", "docs", "test", "chore", "perf", "build", "ci", "style", "revert",
];

const GENERIC_DIRS: &[&str] = &[
    "src", "lib", "app", "source", "pkg", "internal", "crates", "packages", "cmd", "bin", "include",
];

/// One staged path, after the patch has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileChange {
    path: String,
    previous: Option<String>,
    kind: Kind,
    additions: u32,
    deletions: u32,
    binary: bool,
    mode_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Test,
    Ci,
    Deps,
    Build,
    Docs,
    Source,
}

#[derive(Debug, Default)]
struct Tally {
    files: usize,
    unparsed: usize,
    added: usize,
    deleted: usize,
    renamed: usize,
    copied: usize,
    tests: usize,
    ci: usize,
    deps: usize,
    build: usize,
    docs: usize,
    binary: usize,
    mode_only: usize,
    style: usize,
    additions: u64,
    deletions: u64,
}

impl Tally {
    fn real_files(&self) -> usize {
        self.files.saturating_sub(self.unparsed)
    }
}

/// The message, and the facts a model is allowed to phrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDraft {
    pub message: String,
    pub subject: String,
    pub body: String,
    /// Frozen `type(scope): ` prefix. Absent when the patch does not justify one.
    pub prefix: Option<String>,
    pub scope: Option<String>,
    /// Issue tokens that actually occur in the patch. A model may repeat these
    /// and no others.
    pub mentions: BTreeSet<String>,
    pub brief: String,
    pub warnings: Vec<String>,
    pub high_confidence: bool,
    pub conventional: bool,
    pub patch_truncated: bool,
    /// Bytes of patch this draft actually saw.
    pub patch_bytes: usize,
    /// The patch itself contains a breaking-change marker, so a model may repeat one.
    pub allows_breaking: bool,
}

/// A local-model reply with the lines that were not allowed to stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedMessage {
    pub text: String,
    pub dropped_lines: usize,
}

/// Builds the message the composer can use with no model at all.
pub fn draft_change(
    diff: &str,
    staged_paths: Option<&[String]>,
    status_error: Option<&str>,
    recent_subjects: &[String],
    branch: &str,
    patch_truncated: bool,
) -> CommitDraft {
    let (mut files, mut tally, mentions, names) = parse_diff(diff);
    let mut warnings = Vec::new();

    if let Some(error) = status_error {
        warnings.push(format!(
            "Staged paths could not be listed ({error}). The file list comes from the patch."
        ));
    } else if let Some(paths) = staged_paths {
        reconcile(&mut files, &mut tally, paths, &mut warnings);
    }

    if patch_truncated {
        warnings.push(
            "The staged patch was cut before the end, so later files are not in this message."
                .into(),
        );
    }
    if tally.unparsed > 0 {
        warnings.push(
            "Part of the patch could not be split into files, so the type was left unset.".into(),
        );
    }
    if !diff.trim().is_empty() && tally.real_files() == 0 && tally.unparsed == 0 {
        warnings
            .push("The patch did not contain a file header, so the message stays general.".into());
    }

    let real = tally.real_files();
    let omitted = real.saturating_sub(files.len());
    let conventional = repo_is_conventional(recent_subjects);
    let unanimous = patch_truncated == false && tally.unparsed == 0 && real > 0;
    let (change_type, high) = if unanimous {
        classify(&tally)
    } else {
        (None, false)
    };
    // Scope is a fact about the paths, separate from whether the type is
    // settled. A low-confidence edit can still name its directory; a cut patch
    // cannot, because the files we did not see may live somewhere else.
    let scope = if patch_truncated {
        None
    } else if real > 0 && tally.deps == real {
        Some("deps".to_string())
    } else if omitted == 0 {
        scope_from(&files)
    } else {
        None
    };

    let prefix = match (conventional, high, change_type) {
        (true, true, Some(kind)) => Some(match &scope {
            Some(scope) => format!("{kind}({scope}): "),
            None => format!("{kind}: "),
        }),
        _ => None,
    };

    let summary = if patch_truncated || real == 0 {
        "update staged changes".to_string()
    } else {
        format!("{} {}", verb(&tally), what(&files, &tally))
    };
    let subject = fit_subject(prefix.as_deref().unwrap_or(""), &summary);
    let body = render_body(&files, &tally, omitted, patch_truncated);
    let message = assemble(&subject, &body);
    let brief = build_brief(
        &files,
        &tally,
        &names,
        &prefix,
        conventional,
        high,
        branch,
        patch_truncated,
    );

    CommitDraft {
        message,
        subject,
        body,
        prefix,
        scope,
        mentions,
        brief,
        warnings,
        high_confidence: high,
        conventional,
        patch_truncated,
        patch_bytes: diff.len(),
        allows_breaking: contains_breaking_marker(diff),
    }
}

/// The on-device model may rephrase the subject. It may not change the facts.
pub fn accept_on_device_subject(raw: &str, draft: &CommitDraft) -> Result<String, &'static str> {
    let cleaned = prompt::clean_commit_message(raw);
    let subject = cleaned
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .trim_end_matches('.')
        .trim();
    if subject.is_empty() {
        return Err("it was empty");
    }
    if subject
        .chars()
        .any(|ch| ch.is_control() || prompt::is_line_break(ch))
    {
        return Err("it contained a control character");
    }
    if subject.chars().count() > SUBJECT_MAX_CHARS {
        return Err("it was longer than 72 characters");
    }
    if unknown_mention(subject, &draft.mentions).is_some() {
        return Err("it added an issue number the patch does not contain");
    }
    if !draft.allows_breaking && claims_breaking(subject) {
        return Err("it marked a breaking change the patch does not show");
    }
    if let Some(prefix) = &draft.prefix {
        if !subject.starts_with(prefix) {
            return Err("it changed the type prefix");
        }
        if subject[prefix.len()..].trim().is_empty() {
            return Err("it dropped the summary");
        }
        return Ok(subject.to_string());
    }
    if let Some(parsed) = split_conventional(subject) {
        if parsed.breaking {
            return Err("it marked a breaking change the patch does not show");
        }
        if !KNOWN_TYPES.contains(&parsed.kind) {
            return Err("it used an unknown type");
        }
        match (&parsed.scope, &draft.scope) {
            (Some(scope), Some(expected)) if scope.eq_ignore_ascii_case(expected) => {}
            (None, _) => {}
            (Some(_), None) => return Err("it added a scope the patch does not support"),
            (Some(_), Some(_)) => return Err("it changed the scope"),
        }
        if parsed.summary.is_empty() {
            return Err("it dropped the summary");
        }
    }
    Ok(subject.to_string())
}

/// Keeps a loopback model's wording, minus trailers and issue ids the patch
/// does not contain. The subject is the model's; the facts are still checked.
pub fn guard_local_message(raw: &str, draft: &CommitDraft) -> Result<GuardedMessage, &'static str> {
    let cleaned = prompt::clean_commit_message(raw);
    let mut lines: Vec<&str> = cleaned.lines().collect();
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    let Some(first) = lines.first() else {
        return Err("it was empty");
    };
    let subject = first.trim().trim_end_matches('.').trim();
    if subject.is_empty() {
        return Err("it was empty");
    }
    if subject
        .chars()
        .any(|ch| ch.is_control() || prompt::is_line_break(ch))
    {
        return Err("it contained a control character");
    }
    if unknown_mention(subject, &draft.mentions).is_some() {
        return Err("it added an issue number the patch does not contain");
    }
    if !draft.allows_breaking && claims_breaking(subject) {
        return Err("it marked a breaking change the patch does not show");
    }

    let mut dropped = 0usize;
    let mut body: Vec<&str> = Vec::new();
    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            body.push(*line);
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("co-authored-by:") || lower.starts_with("signed-off-by:") {
            dropped += 1;
            continue;
        }
        if !draft.allows_breaking && claims_breaking(trimmed) {
            dropped += 1;
            continue;
        }
        if unknown_mention(trimmed, &draft.mentions).is_some() {
            dropped += 1;
            continue;
        }
        body.push(*line);
    }
    while body.first().is_some_and(|line| line.trim().is_empty()) {
        body.remove(0);
    }
    while body.last().is_some_and(|line| line.trim().is_empty()) {
        body.pop();
    }

    Ok(GuardedMessage {
        text: assemble(subject, &body.join("\n")),
        dropped_lines: dropped,
    })
}

/// Joins a subject and a body the way git expects: one blank line between them.
pub fn assemble(subject: &str, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        subject.to_string()
    } else {
        format!("{subject}\n\n{body}")
    }
}

fn repo_is_conventional(subjects: &[String]) -> bool {
    let samples: Vec<&String> = subjects.iter().take(8).collect();
    if samples.is_empty() {
        return false;
    }
    let conventional = samples
        .iter()
        .filter(|subject| prompt::is_conventional(subject))
        .count();
    conventional * 2 >= samples.len()
}

fn classify(tally: &Tally) -> (Option<&'static str>, bool) {
    let real = tally.real_files();
    if real == 0 {
        return (None, false);
    }
    let all = |count: usize| count == real;
    if all(tally.style) && tally.binary == 0 {
        return (Some("style"), true);
    }
    if all(tally.tests) {
        return (Some("test"), true);
    }
    if all(tally.docs) {
        return (Some("docs"), true);
    }
    if all(tally.ci) {
        return (Some("ci"), true);
    }
    if all(tally.deps) {
        return (Some("chore"), true);
    }
    if all(tally.build) {
        return (Some("build"), true);
    }
    if all(tally.binary) {
        return (Some("chore"), true);
    }
    if all(tally.mode_only) {
        return (Some("chore"), true);
    }
    if tally.renamed + tally.copied == real {
        return (Some("refactor"), true);
    }
    if all(tally.deleted) {
        return (Some("refactor"), true);
    }
    // Tests that arrive with the new source are part of the feature. A commit
    // that is only tests was returned above. Docs, build files, and lockfiles
    // mixed in are a different change and stay untyped.
    if all(tally.added)
        && tally.docs == 0
        && tally.ci == 0
        && tally.build == 0
        && tally.deps == 0
        && tally.binary == 0
    {
        return (Some("feat"), true);
    }
    (None, false)
}

fn verb(tally: &Tally) -> &'static str {
    let real = tally.real_files();
    if real > 0 && tally.style == real && tally.binary == 0 {
        return "format";
    }
    if real > 0 && tally.added == real {
        return "add";
    }
    if real > 0 && tally.deleted == real {
        return "remove";
    }
    if real > 0 && tally.copied == real {
        return "copy";
    }
    if real > 0 && tally.renamed + tally.copied == real {
        return "rename";
    }
    "update"
}

fn what(files: &[FileChange], tally: &Tally) -> String {
    let real = tally.real_files();
    if real == 1 {
        if let Some(file) = files.first() {
            if matches!(file.kind, Kind::Renamed | Kind::Copied) {
                if let Some(previous) = &file.previous {
                    return format!("{} to {}", stem(previous), stem(&file.path));
                }
            }
            return stem(&file.path);
        }
    }
    if real == 0 {
        return "staged changes".to_string();
    }
    format!("{real} files")
}

fn fit_subject(prefix: &str, summary: &str) -> String {
    let summary = summary.trim().trim_end_matches('.').trim();
    let summary = if summary.is_empty() {
        "update staged changes"
    } else {
        summary
    };
    let mut prefix = prefix.to_string();
    if prefix.chars().count() >= SUBJECT_MAX_CHARS {
        prefix = prefix
            .chars()
            .take(SUBJECT_MAX_CHARS.saturating_sub(1))
            .collect();
        prefix = prefix.trim_end().trim_end_matches([':', '(']).to_string();
        if !prefix.is_empty() {
            prefix.push_str(": ");
        }
    }
    let room = SUBJECT_MAX_CHARS.saturating_sub(prefix.chars().count());
    let mut kept = String::new();
    for word in summary.split_whitespace() {
        let extra = word.chars().count() + usize::from(!kept.is_empty());
        if kept.chars().count() + extra > room {
            break;
        }
        if !kept.is_empty() {
            kept.push(' ');
        }
        kept.push_str(word);
    }
    if kept.is_empty() {
        kept = summary.chars().take(room).collect();
        kept = kept.trim_end().trim_end_matches('.').to_string();
    }
    if kept.is_empty() {
        kept = "update".chars().take(room).collect();
    }
    format!("{prefix}{kept}")
}

fn render_body(
    files: &[FileChange],
    tally: &Tally,
    omitted: usize,
    patch_truncated: bool,
) -> String {
    let real = tally.real_files();
    let small = real == 1
        && omitted == 0
        && !patch_truncated
        && tally.binary == 0
        && tally.additions + tally.deletions <= u64::from(SMALL_CHANGE_LINES)
        && !matches!(
            files.first().map(|file| file.kind),
            Some(Kind::Renamed | Kind::Copied)
        );
    // A one-line rename is already the whole subject. Anything larger, or a
    // patch we did not see the end of, keeps the file list so the message
    // cannot pretend the subject was the entire change.
    if small || real == 0 {
        return String::new();
    }
    let mut lines = Vec::new();
    for file in files.iter().take(BODY_FILES) {
        lines.push(body_line(file));
    }
    let hidden = real.saturating_sub(lines.len());
    if hidden > 0 {
        lines.push(format!("- and {hidden} more files"));
    }
    if patch_truncated {
        lines.push("- the patch was cut; later files are not listed".to_string());
    }
    lines.join("\n")
}

fn body_line(file: &FileChange) -> String {
    let path = one_line(&file.path, 160);
    if file.binary {
        return format!("- {} {path} (binary)", kind_verb(file.kind));
    }
    if let Some(previous) = &file.previous {
        return format!(
            "- {} {} -> {path}",
            kind_verb(file.kind),
            one_line(previous, 80)
        );
    }
    if file.mode_only {
        return format!("- update permissions on {path}");
    }
    format!(
        "- {} {path} (+{} -{})",
        kind_verb(file.kind),
        file.additions,
        file.deletions
    )
}

fn kind_verb(kind: Kind) -> &'static str {
    match kind {
        Kind::Added => "add",
        Kind::Modified => "update",
        Kind::Deleted => "remove",
        Kind::Renamed => "rename",
        Kind::Copied => "copy",
    }
}

fn build_brief(
    files: &[FileChange],
    tally: &Tally,
    names: &[String],
    prefix: &Option<String>,
    conventional: bool,
    high: bool,
    branch: &str,
    patch_truncated: bool,
) -> String {
    let mut lines = Vec::new();
    lines.push("Classified staged change.".to_string());
    lines.push(format!(
        "Confidence: {}.",
        if high { "high" } else { "low" }
    ));
    match prefix {
        Some(prefix) => lines.push(format!("Keep this prefix exactly: {prefix}")),
        None => lines.push("No type prefix is fixed.".to_string()),
    }
    lines.push(format!(
        "Conventional history: {}.",
        if conventional { "yes" } else { "no" }
    ));
    let branch = one_line(branch.trim(), 80);
    if !branch.is_empty() {
        lines.push(format!("Branch: {branch}."));
    }
    lines.push(format!(
        "Files: {} (+{} -{}).",
        tally.real_files(),
        tally.additions,
        tally.deletions
    ));
    for file in files.iter().take(BRIEF_FILES) {
        lines.push(one_line(&brief_file(file), 180));
    }
    let listed = files.len().min(BRIEF_FILES);
    if tally.real_files() > listed {
        lines.push(format!(
            "{} further files are not listed.",
            tally.real_files() - listed
        ));
    }
    if !names.is_empty() {
        lines.push(format!("Added names: {}.", names.join(", ")));
    }
    if patch_truncated {
        lines.push("The patch was cut; later files are absent.".to_string());
    }
    cap_lines(&lines, BRIEF_MAX_CHARS)
}

fn brief_file(file: &FileChange) -> String {
    let kind = match file.kind {
        Kind::Added => "A",
        Kind::Modified => "M",
        Kind::Deleted => "D",
        Kind::Renamed => "R",
        Kind::Copied => "C",
    };
    if let Some(previous) = &file.previous {
        return format!("{kind} {previous} -> {}", file.path);
    }
    if file.binary {
        return format!("{kind} {} (binary)", file.path);
    }
    format!(
        "{kind} {} (+{} -{})",
        file.path, file.additions, file.deletions
    )
}

fn cap_lines(lines: &[String], cap: usize) -> String {
    const NOTE: &str = "Further brief lines were left out.";
    let reserve = NOTE.chars().count() + 1;
    let mut out = String::new();
    let mut cut = false;
    for line in lines {
        let addition = line.chars().count() + usize::from(!out.is_empty());
        let limit = if cut {
            cap
        } else {
            cap.saturating_sub(reserve)
        };
        if out.chars().count() + addition > limit {
            cut = true;
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    if cut {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(NOTE);
    }
    if out.chars().count() > cap {
        out = out.chars().take(cap).collect();
    }
    out
}

fn reconcile(
    files: &mut Vec<FileChange>,
    tally: &mut Tally,
    paths: &[String],
    warnings: &mut Vec<String>,
) {
    let known: BTreeSet<String> = files.iter().map(|file| file.path.clone()).collect();
    let mut missing = Vec::new();
    for path in paths {
        let clean = sanitize_path(path);
        if clean.is_empty() || known.contains(&clean) {
            continue;
        }
        missing.push(clean);
    }
    if !missing.is_empty() {
        warnings.push(format!(
            "Staged but absent from the patch: {}.",
            sample(&missing)
        ));
        for path in missing {
            push_status_only(files, tally, path);
        }
    }

    let staged: BTreeSet<String> = paths.iter().map(|path| sanitize_path(path)).collect();
    if staged.is_empty() && !files.is_empty() {
        warnings.push(
            "The patch has files, but nothing was listed as staged. The message follows the patch."
                .into(),
        );
        return;
    }
    let extra: Vec<String> = files
        .iter()
        .filter(|file| !staged.contains(&file.path))
        .map(|file| file.path.clone())
        .collect();
    if !extra.is_empty() {
        warnings.push(format!(
            "In the patch but not listed as staged: {}.",
            sample(&extra)
        ));
    }
}

fn push_status_only(files: &mut Vec<FileChange>, tally: &mut Tally, path: String) {
    tally.files += 1;
    let role = role_of(&path);
    match role {
        Role::Test => tally.tests += 1,
        Role::Ci => tally.ci += 1,
        Role::Deps => tally.deps += 1,
        Role::Build => tally.build += 1,
        Role::Docs => tally.docs += 1,
        Role::Source => {}
    }
    if files.len() >= FILE_CAP {
        return;
    }
    files.push(FileChange {
        path,
        previous: None,
        kind: Kind::Modified,
        additions: 0,
        deletions: 0,
        binary: false,
        mode_only: false,
    });
}

fn sample(paths: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut text = paths
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        text.push_str(&format!(" (and {} more)", paths.len() - SHOWN));
    }
    text
}

fn scope_from(files: &[FileChange]) -> Option<String> {
    let dirs: Vec<Vec<String>> = files
        .iter()
        .map(|file| {
            parent_dirs(&file.path)
                .into_iter()
                .map(|part| part.to_ascii_lowercase())
                .collect()
        })
        .collect();
    if dirs.is_empty() {
        return None;
    }
    let mut len = 0usize;
    loop {
        let Some(next) = dirs[0].get(len) else {
            break;
        };
        if dirs.iter().all(|dir| dir.get(len) == Some(next)) {
            len += 1;
        } else {
            break;
        }
    }
    for part in dirs[0][..len].iter().rev() {
        if GENERIC_DIRS.iter().any(|generic| generic == part) {
            continue;
        }
        return sanitize_scope(part);
    }
    None
}

fn parent_dirs(path: &str) -> Vec<&str> {
    match path.rsplit_once(['/', '\\']) {
        Some((dir, _)) => dir
            .split(['/', '\\'])
            .filter(|part| !part.is_empty())
            .collect(),
        None => Vec::new(),
    }
}

fn sanitize_scope(raw: &str) -> Option<String> {
    let mut out = String::new();
    for ch in raw.trim_start_matches('.').chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out = out.trim_matches(['.', '-', '_']).to_string();
    if out.chars().count() > 24 {
        out = out.chars().take(24).collect();
        out = out.trim_end_matches(['.', '-', '_']).to_string();
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

struct ParsedSubject<'a> {
    kind: &'a str,
    scope: Option<String>,
    breaking: bool,
    summary: &'a str,
}

fn split_conventional(subject: &str) -> Option<ParsedSubject<'_>> {
    let (head, summary) = subject.split_once(':')?;
    if !summary.starts_with(' ') {
        return None;
    }
    let summary = summary.trim();
    let breaking = head.ends_with('!');
    let head = head.trim_end_matches('!');
    let (kind, scope) = if let Some((kind, rest)) = head.split_once('(') {
        let scope = rest.strip_suffix(')')?;
        if scope.is_empty()
            || scope
                .chars()
                .any(|ch| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.'))
        {
            return None;
        }
        (kind, Some(scope.to_string()))
    } else {
        (head, None)
    };
    if kind.is_empty() || !kind.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    Some(ParsedSubject {
        kind,
        scope,
        breaking,
        summary,
    })
}

fn parse_diff(diff: &str) -> (Vec<FileChange>, Tally, BTreeSet<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut tally = Tally::default();
    let mut names = Vec::new();
    let mut current: Option<Acc> = None;

    for raw in diff.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix("diff --git ") {
            finish(&mut current, &mut files, &mut tally);
            current = Some(start_header(rest));
            continue;
        }
        if line.starts_with("diff --cc ") || line.starts_with("diff --combined ") {
            finish(&mut current, &mut files, &mut tally);
            tally.files += 1;
            tally.unparsed += 1;
            current = None;
            continue;
        }
        let Some(acc) = current.as_mut() else {
            continue;
        };
        if acc.unparsed {
            continue;
        }
        if apply_meta(acc, line) {
            continue;
        }
        if let Some(content) = line.strip_prefix('+') {
            if line.starts_with("+++") {
                continue;
            }
            acc.additions = acc.additions.saturating_add(1);
            acc.has_change = true;
            if !content.trim().is_empty() {
                acc.style_only = false;
            }
            if names.len() < NAME_CAP {
                if let Some(name) = added_ident(content) {
                    if !names.iter().any(|have| have == name) {
                        names.push(name.to_string());
                    }
                }
            }
            continue;
        }
        if let Some(content) = line.strip_prefix('-') {
            if line.starts_with("---") {
                continue;
            }
            acc.deletions = acc.deletions.saturating_add(1);
            acc.has_change = true;
            if !content.trim().is_empty() {
                acc.style_only = false;
            }
        }
    }
    finish(&mut current, &mut files, &mut tally);

    let mut mentions = BTreeSet::new();
    collect_mentions(diff, &mut mentions);
    (files, tally, mentions, names)
}

struct Acc {
    path: String,
    previous: Option<String>,
    kind: Kind,
    additions: u32,
    deletions: u32,
    binary: bool,
    saw_mode: bool,
    has_change: bool,
    style_only: bool,
    unparsed: bool,
}

fn start_header(rest: &str) -> Acc {
    let Some((left, right)) = split_two_tokens(rest) else {
        return Acc {
            path: String::new(),
            previous: None,
            kind: Kind::Modified,
            additions: 0,
            deletions: 0,
            binary: false,
            saw_mode: false,
            has_change: false,
            style_only: true,
            unparsed: true,
        };
    };
    let left = unprefix(&left);
    let right = unprefix(&right);
    let (path, previous, kind) = if right == "/dev/null" {
        (sanitize_path(&left), None, Kind::Deleted)
    } else if left == "/dev/null" {
        (sanitize_path(&right), None, Kind::Added)
    } else if left != right {
        (
            sanitize_path(&right),
            Some(sanitize_path(&left)),
            Kind::Renamed,
        )
    } else {
        (sanitize_path(&right), None, Kind::Modified)
    };
    let unparsed = path.is_empty();
    Acc {
        path,
        previous,
        kind,
        additions: 0,
        deletions: 0,
        binary: false,
        saw_mode: false,
        has_change: false,
        style_only: true,
        unparsed,
    }
}

fn apply_meta(acc: &mut Acc, line: &str) -> bool {
    if let Some(rest) = line.strip_prefix("rename from ") {
        acc.previous = Some(sanitize_path(&token_text(rest)));
        acc.kind = Kind::Renamed;
        return true;
    }
    if let Some(rest) = line.strip_prefix("rename to ") {
        acc.path = sanitize_path(&token_text(rest));
        acc.kind = Kind::Renamed;
        return true;
    }
    if let Some(rest) = line.strip_prefix("copy from ") {
        acc.previous = Some(sanitize_path(&token_text(rest)));
        acc.kind = Kind::Copied;
        return true;
    }
    if let Some(rest) = line.strip_prefix("copy to ") {
        acc.path = sanitize_path(&token_text(rest));
        acc.kind = Kind::Copied;
        return true;
    }
    if line.starts_with("new file mode ") {
        if acc.kind != Kind::Renamed && acc.kind != Kind::Copied {
            acc.kind = Kind::Added;
        }
        return true;
    }
    if line.starts_with("deleted file mode ") {
        if acc.kind != Kind::Renamed && acc.kind != Kind::Copied {
            acc.kind = Kind::Deleted;
        }
        return true;
    }
    if line.starts_with("old mode ") || line.starts_with("new mode ") {
        acc.saw_mode = true;
        return true;
    }
    if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
        acc.binary = true;
        return true;
    }
    // `new file mode` is not on every patch. `/dev/null` on the old side is
    // the add, and on the new side it is the deletion.
    if let Some(rest) = line.strip_prefix("--- ") {
        if is_dev_null(rest) && !matches!(acc.kind, Kind::Renamed | Kind::Copied) {
            acc.kind = Kind::Added;
        }
        return true;
    }
    if let Some(rest) = line.strip_prefix("+++ ") {
        if is_dev_null(rest) && !matches!(acc.kind, Kind::Renamed | Kind::Copied) {
            acc.kind = Kind::Deleted;
        }
        return true;
    }
    line.starts_with("@@")
        || line.starts_with("index ")
        || line.starts_with("similarity ")
        || line.starts_with("dissimilarity ")
        || line.starts_with("\\ ")
}

fn finish(current: &mut Option<Acc>, files: &mut Vec<FileChange>, tally: &mut Tally) {
    let Some(acc) = current.take() else {
        return;
    };
    if acc.unparsed || acc.path.is_empty() || acc.path == "/dev/null" {
        tally.files += 1;
        tally.unparsed += 1;
        return;
    }
    tally.files += 1;
    tally.additions = tally.additions.saturating_add(u64::from(acc.additions));
    tally.deletions = tally.deletions.saturating_add(u64::from(acc.deletions));
    match acc.kind {
        Kind::Added => tally.added += 1,
        Kind::Deleted => tally.deleted += 1,
        Kind::Renamed => tally.renamed += 1,
        Kind::Copied => tally.copied += 1,
        Kind::Modified => {}
    }
    if acc.binary {
        tally.binary += 1;
    }
    let mode_only = acc.saw_mode && !acc.has_change && !acc.binary && acc.kind == Kind::Modified;
    if mode_only {
        tally.mode_only += 1;
    }
    if acc.has_change && acc.style_only && !acc.binary {
        tally.style += 1;
    }
    let role = role_of(&acc.path);
    match role {
        Role::Test => tally.tests += 1,
        Role::Ci => tally.ci += 1,
        Role::Deps => tally.deps += 1,
        Role::Build => tally.build += 1,
        Role::Docs => tally.docs += 1,
        Role::Source => {}
    }
    if files.len() >= FILE_CAP {
        return;
    }
    files.push(FileChange {
        path: acc.path,
        previous: acc.previous,
        kind: acc.kind,
        additions: acc.additions,
        deletions: acc.deletions,
        binary: acc.binary,
        mode_only,
    });
}

fn role_of(path: &str) -> Role {
    if is_test_path(path) {
        Role::Test
    } else if is_ci_path(path) {
        Role::Ci
    } else if is_deps_path(path) {
        Role::Deps
    } else if is_build_path(path) {
        Role::Build
    } else if is_docs_path(path) {
        Role::Docs
    } else {
        Role::Source
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split(['/', '\\']).filter(|part| !part.is_empty())
}

fn is_test_path(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    if name == "test.rs"
        || name == "tests.rs"
        || name == "test.go"
        || name.starts_with("test_")
        || name.ends_with("_test.py")
        || name.ends_with("_test.rs")
        || name.ends_with("_tests.rs")
        || name.ends_with("_test.go")
        || name.ends_with("_spec.rb")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".test.js")
        || name.ends_with(".test.jsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
        || name.ends_with(".spec.js")
        || name.ends_with(".spec.jsx")
        || name.ends_with("test.java")
    {
        return true;
    }
    components(path).any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "test" | "tests" | "__tests__" | "testdata" | "testing"
        )
    })
}

fn is_docs_path(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    if name.ends_with(".md")
        || name.ends_with(".mdx")
        || name.ends_with(".rst")
        || name.ends_with(".adoc")
        || name.ends_with(".asciidoc")
    {
        return true;
    }
    let stem = name.split('.').next().unwrap_or("");
    if matches!(
        stem,
        "readme" | "changelog" | "license" | "licence" | "copying" | "authors" | "contributing"
    ) {
        return true;
    }
    components(path).any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "docs" | "doc" | "documentation"
        )
    })
}

fn is_ci_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = file_name(path).to_ascii_lowercase();
    lower.contains(".github/workflows/")
        || lower.contains(".github/actions/")
        || lower.contains(".circleci/")
        || lower.contains(".buildkite/")
        || name == ".gitlab-ci.yml"
        || name == "jenkinsfile"
        || name == "azure-pipelines.yml"
        || name == ".travis.yml"
        || name == "appveyor.yml"
}

fn is_deps_path(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    matches!(
        name.as_str(),
        "cargo.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "go.sum"
            | "gemfile.lock"
            | "poetry.lock"
            | "uv.lock"
            | "composer.lock"
            | "bun.lock"
            | "bun.lockb"
            | "npm-shrinkwrap.json"
            | "packages.lock.json"
            | "pubspec.lock"
            | "pipfile.lock"
    )
}

fn is_build_path(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    matches!(
        name.as_str(),
        "dockerfile"
            | "makefile"
            | "gnumakefile"
            | "cmakelists.txt"
            | "meson.build"
            | "build.rs"
            | "justfile"
            | "package.json"
            | "cargo.toml"
            | "pyproject.toml"
            | "go.mod"
            | "composer.json"
            | "gemfile"
            | "build.gradle"
            | "build.gradle.kts"
            | "pom.xml"
    ) || name.starts_with("dockerfile.")
        || name.ends_with(".csproj")
        || name.ends_with(".sln")
}

fn stem(path: &str) -> String {
    let name = file_name(path);
    let base = if name.starts_with('.') {
        name.trim_start_matches('.')
    } else {
        name.rsplit_once('.')
            .map(|(stem, _)| stem)
            .filter(|stem| !stem.is_empty())
            .unwrap_or(name)
    };
    let mut words = String::new();
    for ch in base.chars() {
        if ch.is_control() || prompt::is_line_break(ch) {
            words.push(' ');
        } else if ch == '_' || ch == '-' {
            words.push(' ');
        } else {
            words.push(ch.to_ascii_lowercase());
        }
    }
    let collapsed = words.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "file".to_string()
    } else {
        collapsed
    }
}

fn sanitize_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if ch.is_control() || prompt::is_line_break(ch) {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

fn one_line(text: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if out.chars().count() >= max {
            out.push('…');
            break;
        }
        if ch.is_control() || prompt::is_line_break(ch) {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

fn added_ident(content: &str) -> Option<&str> {
    let content = content.trim_start();
    const PREFIXES: &[&str] = &[
        "pub(crate) async fn ",
        "pub async fn ",
        "pub(crate) fn ",
        "pub fn ",
        "async fn ",
        "fn ",
        "func ",
        "function ",
        "def ",
        "class ",
        "struct ",
        "interface ",
        "trait ",
        "type ",
    ];
    for prefix in PREFIXES {
        if let Some(rest) = content.strip_prefix(prefix) {
            return take_ident(rest);
        }
    }
    None
}

fn take_ident(rest: &str) -> Option<&str> {
    let rest = rest.trim_start();
    let end = rest
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .unwrap_or(rest.len());
    if end == 0 || end > 40 {
        return None;
    }
    let name = &rest[..end];
    if name.chars().next()?.is_ascii_digit() {
        return None;
    }
    Some(name)
}

fn unprefix(path: &str) -> String {
    if path == "/dev/null" {
        return path.to_string();
    }
    for prefix in ["a/", "b/", "i/", "w/", "c/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    path.to_string()
}

fn token_text(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with('"') {
        split_two_tokens(raw)
            .map(|(token, _)| token)
            .unwrap_or_else(|| raw.to_string())
    } else {
        raw.to_string()
    }
}

fn split_two_tokens(input: &str) -> Option<(String, String)> {
    let (first, rest) = take_token(input)?;
    let (second, _) = take_token(rest)?;
    Some((first, second))
}

fn take_token(input: &str) -> Option<(String, &str)> {
    let input = input.trim_start();
    if input.is_empty() {
        return None;
    }
    if input.starts_with('"') {
        return take_quoted(input);
    }
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    Some((input[..end].to_string(), &input[end..]))
}

fn take_quoted(input: &str) -> Option<(String, &str)> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out = Vec::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                return Some((
                    String::from_utf8_lossy(&out).into_owned(),
                    &input[index + 1..],
                ));
            }
            b'\\' => {
                index += 1;
                if index >= bytes.len() {
                    return None;
                }
                match bytes[index] {
                    b'\\' => out.push(b'\\'),
                    b'"' => out.push(b'"'),
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    digit @ b'0'..=b'7' => {
                        let mut value = u32::from(digit - b'0');
                        let mut count = 1;
                        while count < 3
                            && index + 1 < bytes.len()
                            && (b'0'..=b'7').contains(&bytes[index + 1])
                        {
                            index += 1;
                            value = value * 8 + u32::from(bytes[index] - b'0');
                            count += 1;
                        }
                        out.push(value as u8);
                    }
                    other => out.push(other),
                }
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    None
}

/// Prefixes that look like `KEY-123` and are not issue ids.
const ISSUE_PREFIX_DENY: &[&[u8]] = &[
    b"AES", b"ASCII", b"CSS", b"DES", b"HTML", b"IEC", b"IEEE", b"ISO", b"MD", b"RFC", b"RSA",
    b"SHA", b"UTF",
];

fn collect_mentions(text: &str, into: &mut BTreeSet<String>) {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && into.len() < MENTION_CAP {
        if bytes[index] == b'#' && hash_left(bytes, index) {
            let mut end = index + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() && end - index <= 9 {
                end += 1;
            }
            let digits = end - (index + 1);
            if (1..=8).contains(&digits) && hash_right(bytes, end) {
                into.insert(text[index..end].to_string());
                index = end;
                continue;
            }
        }
        if bytes[index].is_ascii_uppercase() && boundary(bytes, index) {
            let mut end = index;
            while end < bytes.len() && bytes[end].is_ascii_uppercase() && end - index < 10 {
                end += 1;
            }
            let letters = end - index;
            if (2..=10).contains(&letters) && end < bytes.len() && bytes[end] == b'-' {
                let mut digit_end = end + 1;
                while digit_end < bytes.len()
                    && bytes[digit_end].is_ascii_digit()
                    && digit_end - (end + 1) < 8
                {
                    digit_end += 1;
                }
                let digits = digit_end - (end + 1);
                if (1..=8).contains(&digits) && hash_right(bytes, digit_end) {
                    let prefix = &bytes[index..end];
                    let denied = ISSUE_PREFIX_DENY
                        .iter()
                        .any(|deny| prefix.eq_ignore_ascii_case(deny));
                    if !denied {
                        into.insert(text[index..digit_end].to_string());
                    }
                    index = digit_end;
                    continue;
                }
            }
        }
        index += 1;
    }
}

fn boundary(bytes: &[u8], index: usize) -> bool {
    index == 0 || !bytes[index - 1].is_ascii_alphanumeric()
}

/// `#12` in `Fixes #12` counts. `#12px` and `&#404;` do not.
fn hash_left(bytes: &[u8], index: usize) -> bool {
    index == 0 || {
        let previous = bytes[index - 1];
        !previous.is_ascii_alphanumeric() && previous != b'&'
    }
}

fn hash_right(bytes: &[u8], end: usize) -> bool {
    end == bytes.len() || !bytes[end].is_ascii_alphanumeric()
}

fn is_dev_null(rest: &str) -> bool {
    let token = token_text(rest.trim());
    let head = token.split_whitespace().next().unwrap_or(token.as_str());
    head == "/dev/null"
}

fn contains_breaking_marker(diff: &str) -> bool {
    contains_ascii_ignore_case(diff.as_bytes(), b"breaking change")
        || contains_ascii_ignore_case(diff.as_bytes(), b"breaking-change")
}

fn contains_ascii_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(hay, needle)| hay.to_ascii_lowercase() == *needle)
    })
}

fn claims_breaking(text: &str) -> bool {
    if split_conventional(text).is_some_and(|parsed| parsed.breaking) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    lower.contains("breaking change") || lower.contains("breaking-change")
}

fn unknown_mention(text: &str, allowed: &BTreeSet<String>) -> Option<String> {
    let mut found = BTreeSet::new();
    collect_mentions(text, &mut found);
    found.into_iter().find(|token| !allowed.contains(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subjects(conventional: bool) -> Vec<String> {
        if conventional {
            vec!["chore: seed".into()]
        } else {
            vec!["Add the seed".into(), "Tidy the tree".into()]
        }
    }

    fn readme_diff() -> &'static str {
        "diff --git a/README.md b/README.md\n\
         new file mode 100644\n\
         index 0000000..1111111\n\
         --- /dev/null\n\
         +++ b/README.md\n\
         @@ -0,0 +1 @@\n\
         +# hello\n"
    }

    fn draft(diff: &str) -> CommitDraft {
        draft_change(diff, None, None, &subjects(true), "main", false)
    }

    fn assert_contract(draft: &CommitDraft) {
        assert!(
            draft.subject.chars().count() <= SUBJECT_MAX_CHARS,
            "subject too long: {}",
            draft.subject
        );
        assert!(
            !draft.subject.chars().any(|ch| ch.is_control()),
            "subject has a control character: {:?}",
            draft.subject
        );
        assert!(!draft.subject.ends_with('.'));
        assert!(draft.message.starts_with(&draft.subject));
        assert!(draft.brief.chars().count() <= BRIEF_MAX_CHARS);
        assert!(!draft
            .brief
            .lines()
            .any(|line| line.chars().any(|ch| ch == '\r')));
    }

    #[test]
    fn a_new_readme_is_docs_without_asking_a_model() {
        let draft = draft(readme_diff());
        assert_eq!(draft.subject, "docs: add readme");
        assert_eq!(draft.message, "docs: add readme");
        assert!(
            draft.body.is_empty(),
            "a one-line file does not need a body"
        );
        assert!(draft.high_confidence);
        assert_eq!(draft.prefix.as_deref(), Some("docs: "));
        assert!(draft.brief.contains("Keep this prefix exactly: docs: "));
        assert!(draft.brief.contains("README.md"));
        assert!(
            !draft.brief.contains("+# hello"),
            "the brief must not carry hunks"
        );
        assert_contract(&draft);
    }

    #[test]
    fn a_repository_that_does_not_use_conventional_commits_gets_a_plain_subject() {
        let draft = draft_change(
            readme_diff(),
            Some(&[]),
            None,
            &subjects(false),
            "main",
            false,
        );
        assert_eq!(draft.subject, "add readme");
        assert!(draft.prefix.is_none());
        assert_contract(&draft);
    }

    #[test]
    fn added_sources_under_one_directory_take_that_scope() {
        let diff = "\
diff --git a/src/ai/a.rs b/src/ai/a.rs
new file mode 100644
--- /dev/null
+++ b/src/ai/a.rs
@@ -0,0 +1 @@
+fn added_name() {}
diff --git a/src/ai/b.rs b/src/ai/b.rs
new file mode 100644
--- /dev/null
+++ b/src/ai/b.rs
@@ -0,0 +1 @@
+fn other() {}
";
        let draft = draft(diff);
        assert_eq!(draft.subject, "feat(ai): add 2 files");
        assert_eq!(draft.scope.as_deref(), Some("ai"));
        assert!(draft.brief.contains("Added names: added_name, other."));
        assert!(draft.body.contains("src/ai/a.rs"));
        assert_contract(&draft);
    }

    #[test]
    fn a_generic_src_directory_is_not_a_scope() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
new file mode 100644
--- /dev/null
+++ b/src/a.rs
@@ -0,0 +1 @@
+fn a() {}
diff --git a/src/b.rs b/src/b.rs
new file mode 100644
--- /dev/null
+++ b/src/b.rs
@@ -0,0 +1 @@
+fn b() {}
";
        let draft = draft(diff);
        assert_eq!(draft.subject, "feat: add 2 files");
        assert!(draft.scope.is_none());
    }

    #[test]
    fn modifying_existing_source_does_not_invent_a_fix() {
        let diff = "\
diff --git a/src/auth/login.rs b/src/auth/login.rs
--- a/src/auth/login.rs
+++ b/src/auth/login.rs
@@ -1 +1 @@
-old
+new
";
        let draft = draft(diff);
        assert_eq!(draft.subject, "update login");
        assert!(draft.prefix.is_none());
        assert!(!draft.high_confidence);
        assert!(
            !draft.subject.starts_with("fix"),
            "a small model must not be handed a guessed fix"
        );
    }

    #[test]
    fn whitespace_only_edits_are_style() {
        // Both changed lines are whitespace, so this is formatting, not a fix.
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-\n+    \n";
        let draft = draft(diff);
        assert_eq!(draft.prefix.as_deref(), Some("style: "));
        assert!(draft.subject.starts_with("style: format "));
        assert_contract(&draft);
    }

    #[test]
    fn renames_stay_renames() {
        let diff = "\
diff --git a/src/old_name.rs b/src/new_name.rs
similarity index 100%
rename from src/old_name.rs
rename to src/new_name.rs
";
        let draft = draft(diff);
        // `src` is too generic to be a scope, so the subject names the rename
        // and does not pretend the directory is meaningful.
        assert_eq!(draft.subject, "refactor: rename old name to new name");
        assert!(draft.scope.is_none());
        assert_contract(&draft);
    }

    #[test]
    fn a_binary_asset_is_not_described_as_a_feature() {
        let diff = "\
diff --git a/logo.png b/logo.png
new file mode 100644
index 0000000..abc
Binary files /dev/null and b/logo.png differ
";
        let draft = draft(diff);
        assert_eq!(draft.subject, "chore: add logo");
        assert!(draft.body.contains("binary") || draft.message.contains("logo"));
        assert_contract(&draft);
    }

    #[test]
    fn lockfiles_are_dependency_chores() {
        let diff = "\
diff --git a/Cargo.lock b/Cargo.lock
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1 +1 @@
-old
+new
";
        let draft = draft(diff);
        assert_eq!(draft.subject, "chore(deps): update cargo");
        assert_eq!(draft.scope.as_deref(), Some("deps"));
    }

    #[test]
    fn tests_docs_and_ci_keep_their_own_types() {
        let test_diff = "\
diff --git a/src/ai/commit_brief_test.rs b/src/ai/commit_brief_test.rs
--- a/src/ai/commit_brief_test.rs
+++ b/src/ai/commit_brief_test.rs
@@ -1 +1 @@
-old
+new
";
        assert!(draft(test_diff).subject.starts_with("test"));

        let ci = "\
diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -1 +1 @@
-old
+new
";
        assert!(draft(ci).subject.starts_with("ci"));
    }

    #[test]
    fn a_quoted_path_cannot_inject_a_second_line_or_a_trailer() {
        let diff = "\
diff --git \"a/Fixes #1\\nchore: nope.rs\" \"b/Fixes #1\\nchore: nope.rs\"
new file mode 100644
--- /dev/null
+++ \"b/Fixes #1\\nchore: nope.rs\"
@@ -0,0 +1 @@
+fn injected() {}
";
        let draft = draft(diff);
        assert_eq!(draft.subject.lines().count(), 1);
        // The newline in the path is part of the file name, not a second
        // subject. It must not start its own line.
        assert!(!draft.message.lines().any(|line| line.starts_with("chore:")));
        assert!(draft.mentions.contains("#1"));
        assert_contract(&draft);
    }

    #[test]
    fn a_wide_change_stays_inside_the_subject_and_the_brief() {
        let mut diff = String::new();
        for index in 0..80 {
            diff.push_str(&format!(
                "diff --git a/src/f{index}.rs b/src/f{index}.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/f{index}.rs\n@@ -0,0 +1 @@\n+fn f{index}() {{}}\n"
            ));
        }
        let draft = draft(&diff);
        assert!(draft.body.contains("more files"));
        assert!(draft.brief.contains("not listed") || draft.brief.contains("left out"));
        assert_contract(&draft);
    }

    #[test]
    fn on_device_subjects_keep_a_frozen_prefix_and_reject_inventions() {
        let draft = draft(readme_diff());
        assert_eq!(
            accept_on_device_subject("docs: document the crate", &draft).unwrap(),
            "docs: document the crate"
        );
        assert_eq!(
            accept_on_device_subject("fix: add a readme", &draft).unwrap_err(),
            "it changed the type prefix"
        );
        assert_eq!(
            accept_on_device_subject(&format!("docs: {}", "x".repeat(80)), &draft).unwrap_err(),
            "it was longer than 72 characters"
        );
        assert_eq!(
            accept_on_device_subject("docs: add readme\n\nFixes #404", &draft).unwrap(),
            "docs: add readme",
            "a body the model was not asked for is ignored"
        );
        let with_ticket = accept_on_device_subject("docs: add readme for #404", &draft);
        assert_eq!(
            with_ticket.unwrap_err(),
            "it added an issue number the patch does not contain"
        );
    }

    #[test]
    fn a_ticket_that_is_in_the_patch_may_be_repeated() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1 +1 @@
-old
+// Fixes #12 and ABC-7
";
        let draft = draft(diff);
        assert!(draft.mentions.contains("#12"));
        assert!(draft.mentions.contains("ABC-7"));
        let accepted = accept_on_device_subject("update a for #12", &draft).unwrap();
        assert!(accepted.contains("#12"));
    }

    #[test]
    fn an_unfixed_prefix_may_gain_a_known_type_but_not_a_breaking_mark() {
        let diff = "\
diff --git a/src/auth/login.rs b/src/auth/login.rs
--- a/src/auth/login.rs
+++ b/src/auth/login.rs
@@ -1 +1 @@
-old
+new
";
        let draft = draft(diff);
        assert!(draft.prefix.is_none());
        assert_eq!(
            accept_on_device_subject("fix(auth): guard a nil login", &draft).unwrap(),
            "fix(auth): guard a nil login"
        );
        assert_eq!(
            accept_on_device_subject("feat!: drop login", &draft).unwrap_err(),
            "it marked a breaking change the patch does not show"
        );
        assert_eq!(
            accept_on_device_subject("feat(billing): guard a nil login", &draft).unwrap_err(),
            "it changed the scope"
        );
    }

    #[test]
    fn local_replies_lose_invented_trailers_and_keep_the_subject() {
        let draft = draft(readme_diff());
        let guarded = guard_local_message(
            "feat: add a readme\n\nFixes #404\n\nCo-authored-by: Mallory <m@example.com>\n",
            &draft,
        )
        .unwrap();
        assert_eq!(guarded.text, "feat: add a readme");
        assert_eq!(guarded.dropped_lines, 2);

        let rejected = guard_local_message("feat: add a readme for #404", &draft);
        assert_eq!(
            rejected.unwrap_err(),
            "it added an issue number the patch does not contain"
        );
    }

    #[test]
    fn a_cut_patch_does_not_claim_a_precise_type() {
        let draft = draft_change(
            readme_diff(),
            Some(&[]),
            None,
            &subjects(true),
            "main",
            true,
        );
        assert!(draft.prefix.is_none());
        assert!(draft.subject.contains("staged changes") || !draft.high_confidence);
        assert!(draft.warnings.iter().any(|warning| warning.contains("cut")));
        assert_contract(&draft);
    }

    #[test]
    fn status_disagreements_are_named_rather_than_averaged_away() {
        let draft = draft_change(
            readme_diff(),
            Some(&["src/missing.rs".into()]),
            None,
            &subjects(true),
            "main",
            false,
        );
        assert!(draft
            .warnings
            .iter()
            .any(|warning| warning.contains("src/missing.rs")));
        assert!(
            !draft.high_confidence,
            "a file the patch did not show removes the right to a fixed type"
        );
    }

    #[test]
    fn a_new_file_is_an_add_even_without_new_file_mode() {
        // `--- /dev/null` is the signal. `new file mode` is not always there.
        let diff = "\
diff --git a/README.md b/README.md
--- /dev/null
+++ b/README.md
@@ -0,0 +1 @@
+# hello
";
        assert_eq!(draft(diff).subject, "docs: add readme");
    }

    #[test]
    fn a_deletion_marked_only_by_dev_null_is_a_removal() {
        let diff = "\
diff --git a/src/old.rs b/src/old.rs
--- a/src/old.rs
+++ /dev/null
@@ -1 +0,0 @@
-fn old() {}
";
        let draft = draft(diff);
        assert!(
            draft.subject.contains("remove"),
            "deletion described as something else: {}",
            draft.subject
        );
        assert!(!draft.subject.contains("update"), "{}", draft.subject);
    }

    #[test]
    fn a_copied_file_is_not_called_a_rename() {
        let diff = "\
diff --git a/src/old.rs b/src/new.rs
similarity index 100%
copy from src/old.rs
copy to src/new.rs
";
        let draft = draft(diff);
        assert!(
            draft.subject.contains("copy"),
            "copy described as a rename: {}",
            draft.subject
        );
        assert!(!draft.subject.contains("rename"), "{}", draft.subject);
    }

    #[test]
    fn adding_source_together_with_its_tests_is_a_feature() {
        let diff = "\
diff --git a/src/ai/parser.rs b/src/ai/parser.rs
new file mode 100644
--- /dev/null
+++ b/src/ai/parser.rs
@@ -0,0 +1 @@
+fn parse() {}
diff --git a/src/ai/parser_test.rs b/src/ai/parser_test.rs
new file mode 100644
--- /dev/null
+++ b/src/ai/parser_test.rs
@@ -0,0 +1 @@
+fn test_parse() {}
";
        let draft = draft(diff);
        assert!(
            draft.subject.starts_with("feat"),
            "a feature with its tests was left untyped: {}",
            draft.subject
        );
    }

    #[test]
    fn pytest_modules_and_tests_rs_count_as_tests() {
        let tests_rs = "\
diff --git a/src/tests.rs b/src/tests.rs
--- a/src/tests.rs
+++ b/src/tests.rs
@@ -1 +1 @@
-old
+new
";
        assert!(
            draft(tests_rs).subject.starts_with("test"),
            "{}",
            draft(tests_rs).subject
        );
        let pytest = "\
diff --git a/pkg/test_parser.py b/pkg/test_parser.py
--- a/pkg/test_parser.py
+++ b/pkg/test_parser.py
@@ -1 +1 @@
-old
+new
";
        assert!(
            draft(pytest).subject.starts_with("test"),
            "{}",
            draft(pytest).subject
        );
    }

    #[test]
    fn unicode_line_separators_cannot_open_a_second_line() {
        let draft = draft_change(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            None,
            None,
            &subjects(true),
            "main\u{2028}injected",
            false,
        );
        assert!(
            !draft.brief.contains('\u{2028}'),
            "branch separator reached the brief:\n{}",
            draft.brief
        );
        assert!(
            !draft.subject.contains('\u{2028}'),
            "separator reached the subject: {}",
            draft.subject
        );
        let rejected = accept_on_device_subject("update a\u{2028}injected", &draft);
        assert!(rejected.is_err(), "separator accepted: {rejected:?}");
    }

    #[test]
    fn a_local_reply_cannot_invent_a_breaking_change() {
        let draft = draft(readme_diff());
        assert!(guard_local_message("feat!: add a readme", &draft).is_err());
        let guarded = guard_local_message(
            "feat: add a readme\n\nBREAKING CHANGE: drops the parser\n",
            &draft,
        )
        .unwrap();
        assert!(
            !guarded.text.to_ascii_lowercase().contains("breaking"),
            "breaking footer kept: {}",
            guarded.text
        );
        assert!(guarded.dropped_lines >= 1);
    }

    #[test]
    fn technical_tokens_are_not_issue_ids_the_model_may_repeat() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1 +1 @@
-old
+// UTF-8, SHA-1, and &#404; and #12px
";
        let draft = draft(diff);
        assert!(
            !draft
                .mentions
                .iter()
                .any(|token| token.contains("UTF") || token.contains("SHA")),
            "encoding names became issues: {:?}",
            draft.mentions
        );
        assert!(!draft.mentions.contains("#404"), "{:?}", draft.mentions);
        assert!(!draft.mentions.contains("#12"), "{:?}", draft.mentions);
        // Naming the encoding is ordinary prose. Citing a ticket the patch
        // does not contain is not.
        assert_eq!(
            guard_local_message("fix: handle UTF-8", &draft)
                .unwrap()
                .text,
            "fix: handle UTF-8"
        );
        assert!(guard_local_message("fix: closes #404", &draft).is_err());
    }

    #[test]
    fn an_unfixed_scope_matches_without_regard_to_ascii_case() {
        let diff = "\
diff --git a/src/auth/login.rs b/src/auth/login.rs
--- a/src/auth/login.rs
+++ b/src/auth/login.rs
@@ -1 +1 @@
-old
+new
";
        let draft = draft(diff);
        assert_eq!(
            accept_on_device_subject("fix(AUTH): guard a nil login", &draft).unwrap(),
            "fix(AUTH): guard a nil login"
        );
    }

    #[test]
    fn adversarial_patches_cannot_break_the_bounds() {
        let long_name = "n".repeat(8_000);
        let huge_line = "+".to_string() + &"y".repeat(100_000);
        let mut many = String::new();
        for index in 0..120 {
            many.push_str("diff --git a/src/f");
            many.push_str(&index.to_string());
            many.push_str(".rs b/src/f");
            many.push_str(&index.to_string());
            many.push_str(".rs\n+fn f() {}\n");
        }
        let cases = [
            "",
            "not a diff at all",
            "\u{0}\u{0}",
            "diff --git ",
            "diff --cc conflicted",
            "diff --git a/x b/x\n",
            &format!("diff --git a/{long_name} b/{long_name}\n{huge_line}\n"),
            "diff --git \"a/foo\\nbar.rs\" \"b/foo\\nbar.rs\"\nnew file mode 100644\n+line\n",
            "diff --git \"a/caf\\303\\251.rs\" \"b/caf\\303\\251.rs\"\n--- a/caf\\303\\251.rs\n+++ b/caf\\303\\251.rs\n@@ -1 +1 @@\n-old\n+new\n",
            many.as_str(),
        ];
        for diff in cases {
            let draft = draft_change(
                diff,
                Some(&[]),
                None,
                &subjects(true),
                "main\ninjected",
                false,
            );
            assert_contract(&draft);
            assert!(
                !draft.subject.contains("injected"),
                "branch text reached the subject: {}",
                draft.subject
            );
            assert!(
                draft
                    .brief
                    .lines()
                    .all(|line| !line.starts_with("injected")),
                "branch injection became its own brief line:\n{}",
                draft.brief
            );
        }
    }
}
