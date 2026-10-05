//! Reader for Markdown task briefs kept in a repository's `tasks/` folder.
//!
//! A brief is interchange, not storage. The task board renders workbench
//! items, and [`crate::workbench::intake`] is the one path that turns a brief
//! into one; this module only reads. It used to write briefs too, and an agent
//! following that path produced files no GitPulse surface ever displayed.
//!
//! Two input shapes are accepted, because both exist in the wild:
//!
//! * YAML frontmatter (`---` … `---`) followed by a `# Task brief v1` body —
//!   what agents and the earlier writer produce;
//! * the body alone — what **Copy saved brief** puts on the clipboard, and
//!   what someone writes by hand.
//!
//! # Frontmatter wins; the body only fills gaps
//!
//! A brief commonly states its metadata twice, once in each half. When they
//! disagree the frontmatter is what a person edits, so it is what counts; the
//! body supplies only the fields the frontmatter left out.
//!
//! # A file that cannot be read is reported, never dropped
//!
//! [`scan_briefs`] returns one entry per candidate file with either a brief or
//! a reason. Silently skipping a malformed file is how a task goes missing with
//! nothing to say why — the same symptom as a task that was never written.

use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Default tasks directory relative to the repository root.
pub const DEFAULT_TASKS_DIR: &str = "tasks";
/// Longest accepted tasks directory, in bytes.
pub const MAX_TASKS_DIR: usize = 256;
/// Largest brief file read, in bytes.
pub const MAX_BRIEF_BYTES: usize = 1024 * 1024;
/// Most candidate files one scan parses; the rest are counted, not read.
pub const MAX_BRIEF_FILES: usize = 500;
/// Longest task key, in bytes. Matches the workbench identifier bound.
pub const MAX_TASK_KEY: usize = 96;
/// Longest title, in characters. The board refuses anything longer.
pub const MAX_TASK_TITLE: usize = 300;
/// Longest description, in bytes, before planned files are folded in.
pub const MAX_TASK_DESCRIPTION: usize = 65_536;
/// Longest raw log, in bytes.
pub const MAX_TASK_LOGS: usize = 256 * 1024;
/// Label bounds: count, and bytes per label.
pub const MAX_TASK_LABELS: usize = 64;
pub const MAX_LABEL_BYTES: usize = 128;
/// Acceptance-criteria bounds: count, and bytes per item.
pub const MAX_TASK_CRITERIA: usize = 128;
pub const MAX_CRITERION_BYTES: usize = 4096;
/// Planned-file bounds: count, and bytes per path.
pub const MAX_TASK_PLANNED_FILES: usize = 256;
pub const MAX_PLANNED_FILE_BYTES: usize = 4096;
/// Linked-repository bounds: count, and bytes per name.
pub const MAX_TASK_REPOSITORIES: usize = 64;
pub const MAX_REPOSITORY_BYTES: usize = 300;
/// Longest owner and kind, in bytes. Workbench bounds.
pub const MAX_OWNER_BYTES: usize = 300;
pub const MAX_KIND_BYTES: usize = 64;

/// The statuses the board has columns for.
pub const STATUSES: [&str; 6] = ["inbox", "backlog", "ready", "in_progress", "review", "done"];
/// The severities the board accepts. `none` is the absence of one.
pub const SEVERITIES: [&str; 4] = ["low", "medium", "high", "critical"];

/// One parsed brief. `None` means the brief did not say, not a default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TaskBrief {
    /// The stable key a brief names itself by (`id:` or `Task:`).
    pub key: Option<String>,
    pub title: String,
    pub status: Option<String>,
    pub priority: Option<u8>,
    pub severity: Option<String>,
    pub kind: Option<String>,
    pub owner: Option<String>,
    /// As written; [`crate::workbench::intake`] decides what it means.
    pub due: Option<String>,
    pub labels: Vec<String>,
    pub repositories: Vec<String>,
    pub planned_files: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub description: String,
    pub logs: Option<String>,
}

/// One candidate file of a scan: a brief, or the reason there is none.
#[derive(Debug, Clone, Serialize)]
pub struct BriefFile {
    /// Repository-relative path, `/`-separated.
    pub file: String,
    /// The key the task is known by: the brief's own, else the file name's.
    pub key: Option<String>,
    pub brief: Option<TaskBrief>,
    pub error: Option<String>,
}

/// Every candidate in a tasks directory, read or explained.
#[derive(Debug, Clone, Serialize)]
pub struct BriefScan {
    pub tasks_dir: String,
    /// False when the directory does not exist — which is not "zero tasks".
    pub directory_exists: bool,
    /// Candidate `.md` files present.
    pub found: usize,
    /// Of those, the ones this scan read (the first [`MAX_BRIEF_FILES`] by name).
    pub read: usize,
    pub truncated: bool,
    pub files: Vec<BriefFile>,
}

/// Validate a task key: the identity a brief or an agent gives a task.
pub fn validate_task_key(raw: &str) -> Result<String, String> {
    let key = raw.trim();
    if key.is_empty() {
        return Err("task key is empty".into());
    }
    if key.len() > MAX_TASK_KEY {
        return Err(format!("task key exceeds {MAX_TASK_KEY} bytes"));
    }
    if key.starts_with('.') || key.contains("..") {
        return Err(format!(
            "task key {key:?} must not start with '.' or contain '..'"
        ));
    }
    if let Some(bad) = key
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(format!(
            "task key {key:?} contains {bad:?}; use letters, digits, '-', '_' or '.'"
        ));
    }
    Ok(key.to_string())
}

/// A key derived from free text: lowercase ASCII words joined by `-`.
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 60 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "task".into()
    } else {
        slug.into()
    }
}

/// Validate a title against the board's rule: one line, at most 300 characters.
pub fn validate_title(raw: &str) -> Result<String, String> {
    let title = raw.trim();
    if title.is_empty() {
        return Err("title is empty".into());
    }
    if let Some(bad) = title.chars().find(|c| c.is_control()) {
        return Err(format!(
            "title contains control character {bad:?}; a title is one line"
        ));
    }
    let count = title.chars().count();
    if count > MAX_TASK_TITLE {
        return Err(format!(
            "title is {count} characters; the board allows {MAX_TASK_TITLE}"
        ));
    }
    Ok(title.into())
}

/// Normalise a status (`In Progress`, `in-progress` → `in_progress`).
pub fn normalize_status(raw: &str) -> Result<String, String> {
    let status = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    if STATUSES.contains(&status.as_str()) {
        Ok(status)
    } else {
        Err(format!(
            "unknown status {raw:?}; expected one of {}",
            STATUSES.join(", ")
        ))
    }
}

/// Parse a priority: `0`–`3`, `p0`–`p3`, a word, or the brief form `1 (High)`.
pub fn parse_priority(raw: &str) -> Result<u8, String> {
    let lower = raw.trim().to_ascii_lowercase();
    let head = lower.split_whitespace().next().unwrap_or("");
    let number = head.strip_prefix('p').unwrap_or(head);
    if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
        return match number.parse::<u8>() {
            Ok(value) if value <= 3 => Ok(value),
            _ => Err(format!(
                "priority {raw:?} is out of range; use 0 (urgent) to 3 (low)"
            )),
        };
    }
    match lower.as_str() {
        "urgent" => Ok(0),
        "high" => Ok(1),
        "normal" | "medium" => Ok(2),
        "low" => Ok(3),
        _ => Err(format!(
            "priority {raw:?} is not 0–3 or urgent/high/normal/low"
        )),
    }
}

/// Parse a severity. `none` is the absence of one.
pub fn parse_severity(raw: &str) -> Result<Option<String>, String> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() || lower == "none" {
        return Ok(None);
    }
    if SEVERITIES.contains(&lower.as_str()) {
        Ok(Some(lower))
    } else {
        Err(format!(
            "unknown severity {raw:?}; expected none, {}",
            SEVERITIES.join(", ")
        ))
    }
}

/// Validate the tasks directory: relative, plain components only.
pub fn validate_tasks_dir(raw: Option<&str>) -> Result<PathBuf, String> {
    let dir = raw.unwrap_or(DEFAULT_TASKS_DIR).trim();
    if dir.is_empty() {
        return Err("tasks directory is empty".into());
    }
    if dir.len() > MAX_TASKS_DIR {
        return Err(format!("tasks directory exceeds {MAX_TASKS_DIR} bytes"));
    }
    if dir.contains('\0') || dir.contains('\\') {
        return Err("tasks directory contains NUL or a backslash".into());
    }
    let path = Path::new(dir);
    if path.is_absolute()
        || !path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Err("tasks directory must be a relative path inside the repository".into());
    }
    Ok(path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Values a brief states; `None` until something states it.
#[derive(Default)]
struct Stated {
    key: Option<String>,
    title: Option<String>,
    status: Option<String>,
    priority: Option<u8>,
    severity: Option<Option<String>>,
    kind: Option<String>,
    owner: Option<Option<String>>,
    due: Option<Option<String>>,
    labels: Option<Vec<String>>,
    repositories: Option<Vec<String>>,
    planned_files: Option<Vec<String>>,
    acceptance_criteria: Option<Vec<String>>,
    description: Option<String>,
}

impl Stated {
    /// Fill every field `self` left unstated from `other`.
    fn fill_from(&mut self, other: Stated) {
        macro_rules! fill {
            ($($field:ident),*) => {$(
                if self.$field.is_none() { self.$field = other.$field; }
            )*};
        }
        fill!(
            key,
            title,
            status,
            priority,
            severity,
            kind,
            owner,
            due,
            labels,
            repositories,
            planned_files,
            acceptance_criteria,
            description
        );
    }

    /// Apply one `key: value` metadata pair. `source` names it in errors.
    fn set(&mut self, key: &str, value: Value, source: &str) -> Result<(), String> {
        let scalar = |value: Value| -> Result<String, String> {
            match value {
                Value::Scalar(text) => Ok(text),
                Value::List(_) => Err(format!(
                    "{source}: `{key}` must be a single value, not a list"
                )),
            }
        };
        let absent = |text: &str| {
            matches!(
                text.trim().to_ascii_lowercase().as_str(),
                "" | "none" | "null" | "~" | "unassigned" | "unspecified"
            )
        };
        match key {
            "id" | "task" => {
                let text = scalar(value)?;
                // The exported form is `Task: <id> (revision 4)`.
                let head = text.split_whitespace().next().unwrap_or("");
                self.key = Some(validate_task_key(head).map_err(|e| format!("{source}: {e}"))?);
            }
            "title" => {
                self.title =
                    Some(validate_title(&scalar(value)?).map_err(|e| format!("{source}: {e}"))?)
            }
            "status" => {
                let text = scalar(value)?;
                if !absent(&text) {
                    self.status =
                        Some(normalize_status(&text).map_err(|e| format!("{source}: {e}"))?);
                }
            }
            "priority" => {
                self.priority =
                    Some(parse_priority(&scalar(value)?).map_err(|e| format!("{source}: {e}"))?)
            }
            "severity" => {
                self.severity =
                    Some(parse_severity(&scalar(value)?).map_err(|e| format!("{source}: {e}"))?)
            }
            "type" | "kind" => {
                let text = scalar(value)?.trim().to_ascii_lowercase();
                if !absent(&text) {
                    if text.len() > MAX_KIND_BYTES || text.chars().any(char::is_control) {
                        return Err(format!(
                            "{source}: type exceeds {MAX_KIND_BYTES} bytes or spans lines"
                        ));
                    }
                    self.kind = Some(text);
                }
            }
            "owner" => {
                let text = scalar(value)?.trim().to_string();
                if text.len() > MAX_OWNER_BYTES || text.chars().any(char::is_control) {
                    return Err(format!(
                        "{source}: owner exceeds {MAX_OWNER_BYTES} bytes or spans lines"
                    ));
                }
                self.owner = Some((!absent(&text)).then_some(text));
            }
            "due" => {
                let text = scalar(value)?.trim().to_string();
                self.due = Some((!absent(&text)).then_some(text));
            }
            "labels" => {
                self.labels = Some(list(
                    value,
                    MAX_TASK_LABELS,
                    MAX_LABEL_BYTES,
                    "labels",
                    source,
                )?)
            }
            "repositories" => {
                self.repositories = Some(list(
                    value,
                    MAX_TASK_REPOSITORIES,
                    MAX_REPOSITORY_BYTES,
                    "repositories",
                    source,
                )?)
            }
            "planned_files" => {
                self.planned_files = Some(list(
                    value,
                    MAX_TASK_PLANNED_FILES,
                    MAX_PLANNED_FILE_BYTES,
                    "planned_files",
                    source,
                )?)
            }
            "acceptance_criteria" => {
                self.acceptance_criteria = Some(list(
                    value,
                    MAX_TASK_CRITERIA,
                    MAX_CRITERION_BYTES,
                    "acceptance_criteria",
                    source,
                )?)
            }
            "description" => self.description = Some(scalar(value)?),
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Scalar(String),
    List(Vec<String>),
}

/// Bound and de-duplicate a list. A scalar is a one-item (or comma) list.
fn list(
    value: Value,
    max_items: usize,
    max_bytes: usize,
    name: &str,
    source: &str,
) -> Result<Vec<String>, String> {
    let items = match value {
        Value::List(items) => items,
        Value::Scalar(text) => text.split(',').map(str::to_string).collect(),
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let item = item.trim();
        if item.is_empty() || item.eq_ignore_ascii_case("none") {
            continue;
        }
        if item.len() > max_bytes || item.chars().any(char::is_control) {
            return Err(format!(
                "{source}: a {name} item exceeds {max_bytes} bytes or spans lines"
            ));
        }
        if seen.insert(item.to_string()) {
            out.push(item.to_string());
        }
    }
    if out.len() > max_items {
        return Err(format!(
            "{source}: {name} has {} items; at most {max_items}",
            out.len()
        ));
    }
    Ok(out)
}

/// Parse a brief from file content.
pub fn parse_brief(content: &str) -> Result<TaskBrief, String> {
    let text = content
        .strip_prefix('\u{feff}')
        .unwrap_or(content)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if text.contains('\0') {
        return Err("brief contains a NUL byte".into());
    }
    if text.trim().is_empty() {
        return Err("brief is empty".into());
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let (mut stated, body) = if lines[0].trim_end() == "---" {
        let close = lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, line)| matches!(line.trim_end(), "---" | "..."))
            .map(|(index, _)| index)
            .ok_or("frontmatter opens with `---` but is never closed by a `---` line")?;
        (frontmatter(&lines[1..close])?, &lines[close + 1..])
    } else {
        (Stated::default(), &lines[..])
    };
    let (from_body, logs) = body_sections(body)?;
    stated.fill_from(from_body);

    let title = stated
        .title
        .ok_or("no title: give `title:` in the frontmatter or a `## Title` section")?;
    let description = stated.description.unwrap_or_default().trim().to_string();
    if description.len() > MAX_TASK_DESCRIPTION {
        return Err(format!("description exceeds {MAX_TASK_DESCRIPTION} bytes"));
    }
    if logs.as_ref().is_some_and(|l| l.len() > MAX_TASK_LOGS) {
        return Err(format!("raw logs exceed {MAX_TASK_LOGS} bytes"));
    }
    Ok(TaskBrief {
        key: stated.key,
        title,
        status: stated.status,
        priority: stated.priority,
        severity: stated.severity.flatten(),
        kind: stated.kind,
        owner: stated.owner.flatten(),
        due: stated.due.flatten(),
        labels: stated.labels.unwrap_or_default(),
        repositories: stated.repositories.unwrap_or_default(),
        planned_files: stated.planned_files.unwrap_or_default(),
        acceptance_criteria: stated.acceptance_criteria.unwrap_or_default(),
        description: if description == "(none)" {
            String::new()
        } else {
            description
        },
        logs,
    })
}

/// The YAML subset briefs use: `key: scalar`, `key: [a, b]`, and block lists.
///
/// Anything outside that subset is an error naming the line, not a guess —
/// a guessed reading of a status or a priority is a task filed in the wrong
/// column with nothing to say so.
fn frontmatter(lines: &[&str]) -> Result<Stated, String> {
    let mut stated = Stated::default();
    let mut seen = HashSet::new();
    let mut index = 0;
    while index < lines.len() {
        let number = index + 2; // 1-based, after the opening `---`.
        let line = lines[index];
        index += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t', '-']) {
            return Err(format!(
                "frontmatter line {number}: indented or list line with no key above it"
            ));
        }
        let (key, rest) = line
            .split_once(':')
            .ok_or_else(|| format!("frontmatter line {number}: expected `key: value`"))?;
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(format!(
                "frontmatter line {number}: {key:?} is not a plain key"
            ));
        }
        let canonical = match key.as_str() {
            "kind" => "type".to_string(),
            "task" => "id".to_string(),
            _ => key.clone(),
        };
        if !seen.insert(canonical) {
            return Err(format!("frontmatter line {number}: `{key}` appears twice"));
        }
        let source = format!("frontmatter line {number}");
        let rest = rest.trim();
        let value = if rest.is_empty() {
            // A block list, or nothing. Indented lines below belong to this key.
            let mut items = Vec::new();
            while index < lines.len() {
                let next = lines[index];
                if !next.trim().is_empty() && !next.starts_with([' ', '\t', '-']) {
                    break;
                }
                index += 1;
                let item = next.trim();
                if item.is_empty() || item.starts_with('#') {
                    continue;
                }
                let Some(item) = item.strip_prefix('-') else {
                    return Err(format!(
                        "frontmatter line {}: expected a `- item` under `{key}`",
                        index + 1
                    ));
                };
                items.push(scalar(item.trim(), index + 1)?);
            }
            Value::List(items)
        } else if rest.starts_with('[') {
            Value::List(flow_list(rest, number)?)
        } else if rest.starts_with(['|', '>']) {
            if is_known_key(&key) {
                return Err(format!(
                    "{source}: block scalars (`|`, `>`) are not supported for `{key}`"
                ));
            }
            while index < lines.len()
                && (lines[index].trim().is_empty() || lines[index].starts_with([' ', '\t']))
            {
                index += 1;
            }
            continue;
        } else {
            Value::Scalar(scalar(rest, number)?)
        };
        stated.set(&key, value, &source)?;
    }
    Ok(stated)
}

fn is_known_key(key: &str) -> bool {
    matches!(
        key,
        "id" | "title"
            | "status"
            | "priority"
            | "severity"
            | "type"
            | "kind"
            | "owner"
            | "due"
            | "labels"
            | "repositories"
            | "planned_files"
            | "acceptance_criteria"
            | "description"
    )
}

/// One YAML scalar: double-quoted (with escapes), single-quoted, or plain.
fn scalar(raw: &str, number: usize) -> Result<String, String> {
    let raw = raw.trim();
    let (value, rest) = if let Some(inner) = raw.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = inner.char_indices();
        let mut end = None;
        while let Some((at, ch)) = chars.next() {
            match ch {
                '"' => {
                    end = Some(at + 1);
                    break;
                }
                '\\' => {
                    let (_, escaped) = chars.next().ok_or_else(|| {
                        format!("frontmatter line {number}: string ends inside an escape")
                    })?;
                    match escaped {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'u' => {
                            let hex: String = chars.by_ref().take(4).map(|(_, c)| c).collect();
                            let decoded = u32::from_str_radix(&hex, 16)
                                .ok()
                                .filter(|_| hex.len() == 4)
                                .and_then(char::from_u32)
                                .ok_or_else(|| {
                                    format!("frontmatter line {number}: invalid \\u escape")
                                })?;
                            out.push(decoded);
                        }
                        other => {
                            return Err(format!(
                                "frontmatter line {number}: unsupported escape \\{other}"
                            ))
                        }
                    }
                }
                ch => out.push(ch),
            }
        }
        let end = end.ok_or_else(|| {
            format!("frontmatter line {number}: unterminated double-quoted string")
        })?;
        (out, &inner[end..])
    } else if let Some(inner) = raw.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = inner.char_indices().peekable();
        let mut end = None;
        while let Some((at, ch)) = chars.next() {
            if ch == '\'' {
                if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                    out.push('\'');
                } else {
                    end = Some(at + 1);
                    break;
                }
            } else {
                out.push(ch);
            }
        }
        let end = end.ok_or_else(|| {
            format!("frontmatter line {number}: unterminated single-quoted string")
        })?;
        (out, &inner[end..])
    } else {
        // A plain scalar keeps a ` #`. YAML would read the rest as a comment,
        // but `- Close issue #42` losing its issue number silently is worse
        // than a stray `# note` on a status line failing loudly.
        return Ok(raw.to_string());
    };
    let rest = rest.trim();
    if !rest.is_empty() && !rest.starts_with('#') {
        return Err(format!(
            "frontmatter line {number}: unexpected text after a quoted string"
        ));
    }
    Ok(value)
}

/// A flow list: `[a, "b, c", 'd']`.
fn flow_list(raw: &str, number: usize) -> Result<Vec<String>, String> {
    let inner = raw
        .strip_prefix('[')
        .and_then(|rest| {
            let rest = rest.trim_end();
            let rest = match rest.rfind(" #") {
                Some(cut) if rest[..cut].trim_end().ends_with(']') => rest[..cut].trim_end(),
                _ => rest,
            };
            rest.strip_suffix(']')
        })
        .ok_or_else(|| {
            format!("frontmatter line {number}: a `[` list must close with `]` on the same line")
        })?;
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in inner.chars() {
        match (quote, ch) {
            (None, ',') => {
                items.push(std::mem::take(&mut current));
                continue;
            }
            (None, '"' | '\'') => quote = Some(ch),
            (Some(open), c) if c == open => quote = None,
            (None, '[' | ']') => {
                return Err(format!(
                    "frontmatter line {number}: nested lists are not supported"
                ))
            }
            _ => {}
        }
        current.push(ch);
    }
    if quote.is_some() {
        return Err(format!(
            "frontmatter line {number}: unterminated quote in a list"
        ));
    }
    items.push(current);
    items
        .into_iter()
        .filter(|item| !item.trim().is_empty())
        .map(|item| scalar(&item, number))
        .collect()
}

/// Read the Markdown body: titled sections plus `Key: value` metadata lines.
fn body_sections(lines: &[&str]) -> Result<(Stated, Option<String>), String> {
    #[derive(Clone, Copy, PartialEq)]
    enum Section {
        Preamble,
        Title,
        Repositories,
        Description,
        Criteria,
        PlannedFiles,
        Logs,
    }
    let mut stated = Stated::default();
    let mut section = Section::Preamble;
    let mut title: Option<String> = None;
    let mut first_heading: Option<String> = None;
    let mut description: Vec<String> = Vec::new();
    let mut criteria: Vec<String> = Vec::new();
    let mut planned: Vec<String> = Vec::new();
    let mut repositories: Vec<String> = Vec::new();
    let mut logs: Vec<String> = Vec::new();
    // Raw logs are the first fenced block of their section, verbatim. Text
    // after it closes is commentary, not evidence.
    let mut logs_fenced = false;
    let mut logs_done = false;
    let mut fence: Option<(char, usize)> = None;

    for (offset, &line) in lines.iter().enumerate() {
        let source = format!("body line {}", offset + 1);
        let trimmed = line.trim();
        // Fences first: nothing inside one is a heading or a metadata line.
        let marker = trimmed
            .chars()
            .next()
            .filter(|c| matches!(c, '`' | '~'))
            .map(|c| (c, trimmed.chars().take_while(|x| *x == c).count()))
            .filter(|(_, n)| *n >= 3);
        if let Some((open_char, open_len)) = fence {
            let closes = marker.is_some_and(|(c, n)| {
                c == open_char && n >= open_len && trimmed.chars().all(|x| x == c)
            });
            if closes {
                fence = None;
            }
            match section {
                Section::Logs if logs_fenced && !logs_done && !closes => {
                    logs.push(line.to_string())
                }
                Section::Logs => logs_done |= logs_fenced && closes,
                Section::Description | Section::Preamble | Section::Title => {
                    description.push(line.to_string())
                }
                _ => {}
            }
            continue;
        }
        if let Some(open) = marker {
            fence = Some(open);
            match section {
                Section::Logs if !logs_fenced && logs.iter().all(|l| l.trim().is_empty()) => {
                    logs.clear();
                    logs_fenced = true;
                }
                Section::Logs => {}
                Section::Description | Section::Preamble | Section::Title => {
                    description.push(line.to_string())
                }
                _ => {}
            }
            continue;
        }

        if let Some(heading) = heading(trimmed) {
            let name = heading.to_ascii_lowercase();
            let next = match name.as_str() {
                "title" => Some(Section::Title),
                "repositories" => Some(Section::Repositories),
                "description" => Some(Section::Description),
                "acceptance criteria" | "checklist" | "subtasks" => Some(Section::Criteria),
                "planned files" | "files" => Some(Section::PlannedFiles),
                "raw logs" | "logs" => Some(Section::Logs),
                "task brief v1" | "gitpulse task" | "unsaved gitpulse task draft" => {
                    Some(Section::Preamble)
                }
                _ => None,
            };
            match next {
                Some(next) => {
                    section = next;
                    continue;
                }
                None if first_heading.is_none()
                    && section == Section::Preamble
                    && trimmed.starts_with("# ") =>
                {
                    // A hand-written brief's top heading is its title.
                    first_heading = Some(heading.to_string());
                    continue;
                }
                None if section == Section::Logs => continue,
                None => {
                    // Any other heading is part of the description — losing a
                    // `## Context` section silently is worse than keeping it.
                    section = Section::Description;
                    description.push(line.to_string());
                    continue;
                }
            }
        }

        match section {
            Section::Title if title.is_none() => {
                if !trimmed.is_empty() {
                    title = Some(trimmed.to_string());
                }
            }
            Section::Title | Section::Preamble => {
                if let Some((key, value)) = metadata(trimmed) {
                    stated.set(key, Value::Scalar(value.to_string()), &source)?;
                } else if !trimmed.is_empty() && !is_ignored_metadata(trimmed) {
                    description.push(line.to_string());
                }
            }
            Section::Repositories => {
                if let Some(item) = bullet(trimmed) {
                    let name = item.split(" [").next().unwrap_or(item);
                    let name = name.split(" — ").next().unwrap_or(name).trim();
                    if !name.is_empty() && name != "None linked yet." {
                        repositories.push(name.to_string());
                    }
                }
            }
            Section::Description => description.push(line.to_string()),
            Section::Criteria => {
                if let Some(item) = bullet(trimmed) {
                    let item = ["[ ]", "[x]", "[X]"]
                        .iter()
                        .find_map(|box_| item.strip_prefix(box_))
                        .unwrap_or(item)
                        .trim();
                    if !item.is_empty() && item != "No acceptance criteria recorded." {
                        criteria.push(item.to_string());
                    }
                } else if !trimmed.is_empty() {
                    // A wrapped criterion continues the one above it.
                    match criteria.last_mut() {
                        Some(last) => {
                            last.push(' ');
                            last.push_str(trimmed);
                        }
                        None => criteria.push(trimmed.to_string()),
                    }
                }
            }
            Section::PlannedFiles => {
                if let Some(item) = bullet(trimmed) {
                    let item = item.trim_matches('`').trim();
                    if !item.is_empty() {
                        planned.push(item.to_string());
                    }
                }
            }
            Section::Logs => {
                if !logs_fenced
                    && !trimmed.starts_with("Pasted evidence")
                    && !(trimmed.is_empty() && logs.is_empty())
                {
                    logs.push(line.to_string());
                }
            }
        }
    }
    if fence.is_some() && section == Section::Logs && logs_fenced && !logs_done {
        return Err("raw logs open a code fence that is never closed".into());
    }

    if let Some(title) = title.or(first_heading) {
        stated.title = Some(validate_title(&title)?);
    }
    let text = description.join("\n");
    if !text.trim().is_empty() {
        stated.description = Some(text);
    }
    let lists = [
        (
            &mut stated.acceptance_criteria,
            criteria,
            MAX_TASK_CRITERIA,
            MAX_CRITERION_BYTES,
            "acceptance criteria",
        ),
        (
            &mut stated.planned_files,
            planned,
            MAX_TASK_PLANNED_FILES,
            MAX_PLANNED_FILE_BYTES,
            "planned files",
        ),
        (
            &mut stated.repositories,
            repositories,
            MAX_TASK_REPOSITORIES,
            MAX_REPOSITORY_BYTES,
            "repositories",
        ),
    ];
    for (slot, items, max_items, max_bytes, name) in lists {
        if !items.is_empty() {
            *slot = Some(list(
                Value::List(items),
                max_items,
                max_bytes,
                name,
                "body",
            )?);
        }
    }
    let logs = logs.join("\n");
    let logs = logs.trim_matches('\n');
    Ok((stated, (!logs.trim().is_empty()).then(|| logs.to_string())))
}

fn heading(trimmed: &str) -> Option<&str> {
    let level = trimmed.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &trimmed[level..];
    (rest.starts_with(' ') || rest.is_empty()).then(|| rest.trim())
}

fn bullet(trimmed: &str) -> Option<&str> {
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        return Some(rest.trim());
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = trimmed[digits..]
            .strip_prefix(". ")
            .or_else(|| trimmed[digits..].strip_prefix(") "))
        {
            return Some(rest.trim());
        }
    }
    None
}

/// A body metadata line this reader understands, as `(key, value)`.
fn metadata(trimmed: &str) -> Option<(&'static str, &str)> {
    let (key, value) = trimmed.split_once(':')?;
    let key = key.trim().to_ascii_lowercase();
    let mapped = match key.as_str() {
        "task" => "task",
        "type" | "kind" => "type",
        "status" => "status",
        "priority" => "priority",
        "severity" => "severity",
        "owner" => "owner",
        "due" | "due date" | "due (unix seconds)" => "due",
        "labels" => "labels",
        _ => return None,
    };
    Some((mapped, value.trim()))
}

/// Export bookkeeping lines that carry nothing a task needs.
fn is_ignored_metadata(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();
    [
        "updated (unix seconds):",
        "enhancement field locks:",
        "home workspace:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// Read every candidate brief in a repository's tasks directory.
///
/// Fails as a whole only when the directory itself cannot be trusted or read:
/// it resolves outside the repository, or `read_dir` fails. Each file's own
/// failure — unreadable, oversized, not UTF-8, a symlink, malformed — is an
/// entry with a reason. A missing directory is `directory_exists: false`.
pub fn scan_briefs(repo_path: &str, tasks_dir: Option<&str>) -> Result<BriefScan, String> {
    let relative = validate_tasks_dir(tasks_dir)?;
    let display_dir = relative.to_string_lossy().replace('\\', "/");
    let repo = Path::new(repo_path);
    if !repo.is_absolute() {
        return Err("repository path must be absolute".into());
    }
    let repo = repo
        .canonicalize()
        .map_err(|e| format!("cannot resolve repository {repo_path}: {e}"))?;
    let dir = repo.join(&relative);
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BriefScan {
                tasks_dir: display_dir,
                directory_exists: false,
                found: 0,
                read: 0,
                truncated: false,
                files: Vec::new(),
            })
        }
        Err(e) => return Err(format!("cannot inspect {display_dir}: {e}")),
        Ok(_) => {}
    }
    let resolved = dir
        .canonicalize()
        .map_err(|e| format!("cannot resolve {display_dir}: {e}"))?;
    if !resolved.starts_with(&repo) {
        return Err(format!(
            "{display_dir} resolves outside the repository; refusing to read it"
        ));
    }
    if !resolved.is_dir() {
        return Err(format!("{display_dir} is not a directory"));
    }
    let mut names = Vec::new();
    let mut files = Vec::new();
    for entry in
        std::fs::read_dir(&resolved).map_err(|e| format!("cannot list {display_dir}: {e}"))?
    {
        let entry = entry.map_err(|e| format!("cannot list {display_dir}: {e}"))?;
        let raw = entry.file_name();
        let Some(name) = raw.to_str() else {
            files.push(BriefFile {
                file: format!("{display_dir}/{}", raw.to_string_lossy()),
                key: None,
                brief: None,
                error: Some("file name is not valid UTF-8".into()),
            });
            continue;
        };
        let is_brief = name.len() > 3
            && name[name.len() - 3..].eq_ignore_ascii_case(".md")
            && !name.starts_with('.');
        if is_brief {
            names.push(name.to_string());
        }
    }
    names.sort();
    let found = names.len() + files.len();
    let truncated = names.len() > MAX_BRIEF_FILES;
    names.truncate(MAX_BRIEF_FILES);

    for name in &names {
        let file = format!("{display_dir}/{name}");
        let stem = &name[..name.len() - 3];
        let path = resolved.join(name);
        // `O_NOFOLLOW` is the race-free guard, but it exists on unix only; this
        // check is what refuses a link on every platform.
        let linked =
            std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink());
        let parsed = if linked {
            Err(LINK_REFUSAL.to_string())
        } else {
            match crate::fs_entry::read_bounded_nofollow(&path, MAX_BRIEF_BYTES) {
                Ok(bytes) => String::from_utf8(bytes)
                    .map_err(|_| "file is not valid UTF-8".to_string())
                    .and_then(|text| parse_brief(&text)),
                Err(error) => Err(read_failure(error)),
            }
        };
        files.push(match parsed {
            Ok(brief) => {
                let key = match &brief.key {
                    Some(key) => Ok(key.clone()),
                    None => validate_task_key(stem).or_else(|_| Ok::<_, String>(slugify(stem))),
                };
                match key {
                    Ok(key) => BriefFile {
                        file,
                        key: Some(key),
                        brief: Some(brief),
                        error: None,
                    },
                    Err(error) => BriefFile {
                        file,
                        key: None,
                        brief: None,
                        error: Some(error),
                    },
                }
            }
            Err(error) => BriefFile {
                file,
                key: None,
                brief: None,
                error: Some(error),
            },
        });
    }
    // Two files claiming one key would silently overwrite each other on the
    // board; the first by name keeps it and the rest say so.
    let mut owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for entry in &mut files {
        if let Some(key) = entry.key.clone() {
            if let Some(owner) = owners.get(&key) {
                entry.error = Some(format!("task key {key:?} is already used by {owner}"));
                entry.brief = None;
            } else {
                owners.insert(key, entry.file.clone());
            }
        }
    }
    let read = names.len();
    Ok(BriefScan {
        tasks_dir: display_dir,
        directory_exists: true,
        found,
        read,
        truncated,
        files,
    })
}

const LINK_REFUSAL: &str = "is a symbolic link; briefs must be regular files inside the repository";

fn read_failure(error: crate::fs_entry::BoundedRead) -> String {
    use crate::fs_entry::BoundedRead;
    match error {
        #[cfg(unix)]
        BoundedRead::Io(e) if e.raw_os_error() == Some(libc::ELOOP) => LINK_REFUSAL.into(),
        BoundedRead::Io(e) => format!("cannot read: {e}"),
        BoundedRead::NotRegular => "is not a regular file".into(),
        BoundedRead::TooLarge | BoundedRead::Grew => {
            format!(
                "is larger than the {} KiB brief limit",
                MAX_BRIEF_BYTES / 1024
            )
        }
    }
}

#[cfg(test)]
#[path = "file_tasks_tests.rs"]
mod tests;
