//! File-based tasks and GitPulse task format engine.
//!
//! Provides creation, parsing, formatting, listing, and inspection of
//! tasks in a repository's `tasks` folder following the GitPulse tasks format
//! (Markdown Task Brief v1 with YAML frontmatter).
//!
//! Every task creation or modification is policy-gated via
//! [`crate::harness::guard_file`] and recorded to the durable ledger.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use serde::{Deserialize, Serialize};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Maximum allowed length of a task title.
pub const MAX_TASK_TITLE: usize = 1_200;
/// Maximum allowed length of a task description.
pub const MAX_TASK_DESCRIPTION: usize = 65_536;
/// Maximum allowed length of a task ID.
pub const MAX_TASK_ID: usize = 128;
/// Maximum allowed length of a task directory path.
pub const MAX_TASKS_DIR: usize = 256;
/// Maximum allowed labels per task.
pub const MAX_TASK_LABELS: usize = 64;
/// Maximum allowed planned files per task.
pub const MAX_TASK_PLANNED_FILES: usize = 128;
/// Maximum allowed acceptance criteria per task.
pub const MAX_TASK_CRITERIA: usize = 128;
/// Maximum allowed repositories per task.
pub const MAX_TASK_REPOSITORIES: usize = 64;
/// Maximum task listing limit.
pub const MAX_LIST_LIMIT: usize = 500;
/// Default task listing limit.
pub const DEFAULT_LIST_LIMIT: usize = 50;
/// Default tasks directory relative to repository root.
pub const DEFAULT_TASKS_DIR: &str = "tasks";
/// Maximum raw logs payload in bytes (256 KiB).
pub const MAX_TASK_LOGS: usize = 256 * 1024;

/// Structured task details in the GitPulse task format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskFileDetails {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: u32,
    pub severity: String,
    pub kind: String,
    pub owner: String,
    pub due: String,
    pub labels: Vec<String>,
    pub repositories: Vec<String>,
    pub planned_files: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<String>,
}

impl Default for TaskFileDetails {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            status: "inbox".to_string(),
            priority: 2,
            severity: "none".to_string(),
            kind: "feature".to_string(),
            owner: "unassigned".to_string(),
            due: "none".to_string(),
            labels: Vec::new(),
            repositories: Vec::new(),
            planned_files: Vec::new(),
            acceptance_criteria: Vec::new(),
            description: String::new(),
            logs: None,
        }
    }
}


/// Request to create a new task file in GitPulse tasks format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTaskRequest {
    pub repo_path: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<u32>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub repositories: Option<Vec<String>>,
    #[serde(default)]
    pub planned_files: Option<Vec<String>>,
    #[serde(default)]
    pub acceptance_criteria: Option<Vec<String>>,
    #[serde(default)]
    pub logs: Option<String>,
    #[serde(default)]
    pub tasks_dir: Option<String>,
    #[serde(default)]
    pub overwrite: Option<bool>,
}

/// Summary of a task file for listings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: u32,
    pub priority_label: String,
    pub severity: String,
    pub kind: String,
    pub owner: String,
    pub due: String,
    pub labels: Vec<String>,
    pub file_path: String,
    pub criteria_count: usize,
    pub planned_files_count: usize,
}

/// Result of adding a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAddResult {
    pub ok: bool,
    pub task_id: String,
    pub file_path: String,
    pub absolute_path: String,
    pub title: String,
    pub status: String,
    pub priority: u32,
    pub content: String,
    pub verdict: crate::harness::PolicyVerdict,
}

/// Result of listing tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskListResult {
    pub ok: bool,
    pub tasks_dir: String,
    pub returned: usize,
    pub total: usize,
    pub truncated: bool,
    pub tasks: Vec<TaskSummary>,
}

/// Result of reading a single task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGetResult {
    pub ok: bool,
    pub file_path: String,
    pub task: TaskFileDetails,
    pub content: String,
}

/// Human-readable priority label.
pub fn priority_label(priority: u32) -> &'static str {
    match priority {
        0 => "Urgent",
        1 => "High",
        2 => "Normal",
        3 => "Low",
        _ => "Normal",
    }
}

/// Validate and clean task title.
pub fn validate_title(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Task title cannot be empty".to_string());
    }
    if trimmed.chars().count() > MAX_TASK_TITLE {
        return Err(format!(
            "Task title exceeds maximum length of {MAX_TASK_TITLE} characters (got {})",
            trimmed.chars().count()
        ));
    }
    for ch in trimmed.chars() {
        let code = ch as u32;
        if (code < 32 && ch != '\t' && ch != '\n' && ch != '\r') || code == 127 {
            return Err("Task title contains prohibited control characters".to_string());
        }
    }
    Ok(trimmed.to_string())
}

/// Validate and clean description.
pub fn validate_description(raw: Option<&str>) -> Result<String, String> {
    let Some(text) = raw else {
        return Ok(String::new());
    };
    if text.contains('\0') {
        return Err("Task description contains NUL byte".to_string());
    }
    if text.chars().count() > MAX_TASK_DESCRIPTION {
        return Err(format!(
            "Task description exceeds maximum length of {MAX_TASK_DESCRIPTION} characters"
        ));
    }
    Ok(text.to_string())
}

/// Validate and clean optional raw logs.
pub fn validate_logs(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(text) = raw else {
        return Ok(None);
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > MAX_TASK_LOGS {
        return Err(format!(
            "Task logs exceed maximum length of {MAX_TASK_LOGS} bytes"
        ));
    }
    Ok(Some(trimmed.to_string()))
}

/// Validate and clean task ID.
pub fn validate_task_id(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Task ID cannot be empty".to_string());
    }
    if trimmed.len() > MAX_TASK_ID {
        return Err(format!(
            "Task ID exceeds maximum length of {MAX_TASK_ID} characters"
        ));
    }
    if trimmed.contains("..") || trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err("Task ID contains invalid path traversal characters".to_string());
    }
    for ch in trimmed.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_' && ch != '.' {
            return Err(format!("Task ID contains invalid character: {ch:?}"));
        }
    }
    Ok(trimmed.to_string())
}

/// Generate a URL-friendly slug from title.
pub fn slugify_title(title: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 60 {
            break;
        }
    }
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "task".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Normalize tasks status.
pub fn normalize_status(raw: Option<&str>) -> Result<String, String> {
    let Some(status) = raw else {
        return Ok("inbox".to_string());
    };
    let clean = status.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match clean.as_str() {
        "inbox" | "backlog" | "ready" | "in_progress" | "review" | "done" => Ok(clean),
        _ => Err(format!(
            "Invalid status '{clean}'. Must be one of: inbox, backlog, ready, in_progress, review, done"
        )),
    }
}

/// Normalize priority (0..=3).
pub fn normalize_priority(raw: Option<u32>) -> Result<u32, String> {
    match raw {
        None => Ok(2), // normal
        Some(p) if p <= 3 => Ok(p),
        Some(p) => Err(format!("Invalid priority {p}. Must be 0 (urgent), 1 (high), 2 (normal), or 3 (low)")),
    }
}

/// Validate tasks directory.
pub fn validate_tasks_dir(raw: Option<&str>) -> Result<PathBuf, String> {
    let dir_str = raw.unwrap_or(DEFAULT_TASKS_DIR).trim();
    if dir_str.is_empty() {
        return Err("Tasks directory cannot be empty".to_string());
    }
    if dir_str.len() > MAX_TASKS_DIR {
        return Err(format!("Tasks directory path exceeds limit of {MAX_TASKS_DIR} characters"));
    }
    if dir_str.contains('\0') {
        return Err("Tasks directory path contains NUL byte".to_string());
    }
    let path = Path::new(dir_str);
    if path.is_absolute() {
        return Err("Tasks directory must be a relative path".to_string());
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => {}
            _ => return Err("Tasks directory must not contain path traversal (.., /, \\)".to_string()),
        }
    }
    Ok(path.to_path_buf())
}

/// Clean a list of strings with caps and boundaries.
fn clean_string_list(
    items: Option<Vec<String>>,
    max_items: usize,
    max_item_len: usize,
    name: &str,
) -> Result<Vec<String>, String> {
    let Some(list) = items else {
        return Ok(Vec::new());
    };
    if list.len() > max_items {
        return Err(format!("{name} exceeds maximum item count of {max_items} (got {})", list.len()));
    }
    let mut cleaned = Vec::with_capacity(list.len());
    for item in list {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.contains('\0') {
            return Err(format!("{name} item contains NUL byte"));
        }
        if trimmed.chars().count() > max_item_len {
            return Err(format!("{name} item exceeds length of {max_item_len} characters"));
        }
        cleaned.push(trimmed.to_string());
    }
    Ok(cleaned)
}

/// Format a task into the canonical GitPulse task markdown format with YAML frontmatter.
pub fn format_gitpulse_task(task: &TaskFileDetails) -> String {
    let mut out = String::new();

    // Frontmatter (YAML-compatible)
    out.push_str("---\n");
    out.push_str(&format!("id: \"{}\"\n", escape_yaml(&task.id)));
    out.push_str(&format!("title: \"{}\"\n", escape_yaml(&task.title)));
    out.push_str(&format!("status: {}\n", task.status));
    out.push_str(&format!("priority: {}\n", task.priority));
    out.push_str(&format!("severity: {}\n", task.severity));
    out.push_str(&format!("type: {}\n", task.kind));
    out.push_str(&format!("owner: \"{}\"\n", escape_yaml(&task.owner)));
    out.push_str(&format!("due: \"{}\"\n", escape_yaml(&task.due)));

    if !task.labels.is_empty() {
        out.push_str("labels:\n");
        for label in &task.labels {
            out.push_str(&format!("  - \"{}\"\n", escape_yaml(label)));
        }
    } else {
        out.push_str("labels: []\n");
    }

    if !task.repositories.is_empty() {
        out.push_str("repositories:\n");
        for repo in &task.repositories {
            out.push_str(&format!("  - \"{}\"\n", escape_yaml(repo)));
        }
    } else {
        out.push_str("repositories: []\n");
    }

    if !task.planned_files.is_empty() {
        out.push_str("planned_files:\n");
        for file in &task.planned_files {
            out.push_str(&format!("  - \"{}\"\n", escape_yaml(file)));
        }
    } else {
        out.push_str("planned_files: []\n");
    }

    if !task.acceptance_criteria.is_empty() {
        out.push_str("acceptance_criteria:\n");
        for crit in &task.acceptance_criteria {
            out.push_str(&format!("  - \"{}\"\n", escape_yaml(crit)));
        }
    } else {
        out.push_str("acceptance_criteria: []\n");
    }
    out.push_str("---\n\n");

    // Markdown Task Brief v1 body
    out.push_str("# Task brief v1\n\n");
    out.push_str("## Title\n");
    out.push_str(&task.title);
    out.push_str("\n\n");

    out.push_str(&format!("Task: {}\n", task.id));
    out.push_str(&format!("Type: {}\n", if task.kind.is_empty() { "Unspecified" } else { &task.kind }));
    out.push_str(&format!("Status: {}\n", task.status));
    out.push_str(&format!("Priority: {} ({})\n", task.priority, priority_label(task.priority)));
    out.push_str(&format!("Severity: {}\n", if task.severity.is_empty() { "None" } else { &task.severity }));
    out.push_str(&format!("Owner: {}\n", if task.owner.is_empty() { "Unassigned" } else { &task.owner }));
    out.push_str(&format!("Due: {}\n", if task.due.is_empty() { "None" } else { &task.due }));
    out.push_str(&format!(
        "Labels: {}\n\n",
        if task.labels.is_empty() { "None".to_string() } else { task.labels.join(", ") }
    ));

    out.push_str("## Repositories\n");
    if task.repositories.is_empty() {
        out.push_str("None linked yet.\n");
    } else {
        for repo in &task.repositories {
            out.push_str(&format!("- {repo}\n"));
        }
    }
    out.push('\n');

    out.push_str("## Description\n");
    if task.description.trim().is_empty() {
        out.push_str("(none)\n");
    } else {
        out.push_str(task.description.trim());
        out.push('\n');
    }
    out.push('\n');

    out.push_str("## Acceptance criteria\n");
    if task.acceptance_criteria.is_empty() {
        out.push_str("No acceptance criteria recorded.\n");
    } else {
        for crit in &task.acceptance_criteria {
            out.push_str(&format!("- [ ] {crit}\n"));
        }
    }

    if !task.planned_files.is_empty() {
        out.push_str("\n## Planned files\n");
        for file in &task.planned_files {
            out.push_str(&format!("- {file}\n"));
        }
    }

    if let Some(logs) = &task.logs {
        let trimmed_logs = logs.trim();
        if !trimmed_logs.is_empty() {
            out.push_str("\n## Raw logs\nPasted evidence. Keep stack frames, timestamps, error codes and quoted text exactly as written.\n\n");
            out.push_str(&fence_logs(trimmed_logs));
            out.push('\n');
        }
    }

    out
}

fn fence_logs(text: &str) -> String {
    let mut ticks = 3;
    let mut run = 0;
    for ch in text.chars() {
        if ch == '`' {
            run += 1;
            if run + 1 > ticks {
                ticks = run + 1;
            }
        } else {
            run = 0;
        }
    }
    let mark = "`".repeat(ticks);
    format!("{mark}\n{text}\n{mark}")
}

fn escape_yaml(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Parse a GitPulse task from file content.
pub fn parse_gitpulse_task(content: &str) -> Result<TaskFileDetails, String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("Task file content is empty".to_string());
    }

    let mut task = TaskFileDetails::default();
    let body_text;

    // Check if YAML frontmatter exists
    if let Some(rest) = trimmed.strip_prefix("---") {
        if let Some(end_idx) = rest.find("\n---") {
            let frontmatter = &rest[..end_idx];
            body_text = rest[end_idx + 4..].trim();
            parse_yaml_frontmatter(frontmatter, &mut task);
        } else {
            body_text = trimmed;
        }
    } else {
        body_text = trimmed;
    }

    // Parse body markdown sections (for title, description, criteria, planned_files, logs)
    parse_markdown_sections(body_text, &mut task);

    if task.title.is_empty() {
        return Err("Could not extract task title from content".to_string());
    }
    if task.id.is_empty() {
        task.id = slugify_title(&task.title);
    }

    Ok(task)
}

fn parse_yaml_frontmatter(frontmatter: &str, task: &mut TaskFileDetails) {
    let mut current_list: Option<&mut Vec<String>> = None;

    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(list) = current_list.as_mut() {
            if let Some(stripped) = trimmed.strip_prefix('-') {
                let item = stripped.trim().trim_matches('"').trim_matches('\'').trim();
                if !item.is_empty() && !list.contains(&item.to_string()) {
                    list.push(item.to_string());
                }
                continue;
            } else if !trimmed.contains(':') {
                continue;
            }
        }
        current_list = None;

        if let Some((key, val)) = line.split_once(':') {
            let k = key.trim().to_ascii_lowercase();
            let v = val.trim().trim_matches('"').trim_matches('\'').trim();

            match k.as_str() {
                "id" if !v.is_empty() => task.id = v.to_string(),
                "title" if !v.is_empty() => task.title = v.to_string(),
                "status" if !v.is_empty() => {
                    let s = v.to_ascii_lowercase().replace(['-', ' '], "_");
                    if !s.is_empty() {
                        task.status = s;
                    }
                }
                "priority" => {
                    if let Some(digit) = v.chars().find(|c| c.is_ascii_digit()).and_then(|c| c.to_digit(10)) {
                        task.priority = digit.min(3);
                    } else {
                        let lower = v.to_ascii_lowercase();
                        if lower.contains("urgent") {
                            task.priority = 0;
                        } else if lower.contains("high") {
                            task.priority = 1;
                        } else if lower.contains("normal") || lower.contains("medium") {
                            task.priority = 2;
                        } else if lower.contains("low") {
                            task.priority = 3;
                        }
                    }
                }
                "severity" if !v.is_empty() => task.severity = v.to_ascii_lowercase(),
                "type" | "kind" if !v.is_empty() => task.kind = v.to_ascii_lowercase(),
                "owner" if !v.is_empty() => task.owner = v.to_string(),
                "due" if !v.is_empty() => task.due = v.to_string(),
                "labels" => {
                    if v == "[]" {
                        task.labels.clear();
                    } else if !v.is_empty() {
                        let parsed = v.trim_matches('[').trim_matches(']');
                        for part in parsed.split(',') {
                            let item = part.trim().trim_matches('"').trim_matches('\'').trim();
                            if !item.is_empty() && !task.labels.contains(&item.to_string()) {
                                task.labels.push(item.to_string());
                            }
                        }
                    } else {
                        current_list = Some(&mut task.labels);
                    }
                }
                "repositories" => {
                    if v == "[]" {
                        task.repositories.clear();
                    } else if !v.is_empty() {
                        let parsed = v.trim_matches('[').trim_matches(']');
                        for part in parsed.split(',') {
                            let item = part.trim().trim_matches('"').trim_matches('\'').trim();
                            if !item.is_empty() && !task.repositories.contains(&item.to_string()) {
                                task.repositories.push(item.to_string());
                            }
                        }
                    } else {
                        current_list = Some(&mut task.repositories);
                    }
                }
                "planned_files" => {
                    if v == "[]" {
                        task.planned_files.clear();
                    } else if !v.is_empty() {
                        let parsed = v.trim_matches('[').trim_matches(']');
                        for part in parsed.split(',') {
                            let item = part.trim().trim_matches('"').trim_matches('\'').trim();
                            if !item.is_empty() && !task.planned_files.contains(&item.to_string()) {
                                task.planned_files.push(item.to_string());
                            }
                        }
                    } else {
                        current_list = Some(&mut task.planned_files);
                    }
                }
                "acceptance_criteria" => {
                    if v == "[]" {
                        task.acceptance_criteria.clear();
                    } else if !v.is_empty() {
                        let parsed = v.trim_matches('[').trim_matches(']');
                        for part in parsed.split(',') {
                            let item = part.trim().trim_matches('"').trim_matches('\'').trim();
                            if !item.is_empty() && !task.acceptance_criteria.contains(&item.to_string()) {
                                task.acceptance_criteria.push(item.to_string());
                            }
                        }
                    } else {
                        current_list = Some(&mut task.acceptance_criteria);
                    }
                }
                _ => {}
            }
        }
    }
}

fn parse_kv_metadata_line(k: &str, val: &str, task: &mut TaskFileDetails) {
    let key = k.trim().to_ascii_lowercase();
    let val = val.trim();
    match key.as_str() {
        "task" if task.id.is_empty() => {
            let clean = val.split_whitespace().next().unwrap_or(val).trim();
            task.id = clean.to_string();
        }
        "type" | "kind" if task.kind.is_empty() || task.kind == "feature" => task.kind = val.to_ascii_lowercase(),
        "status" => {
            let s = val.to_ascii_lowercase().replace(['-', ' '], "_");
            if s != "unspecified" && !s.is_empty() {
                task.status = s;
            }
        }
        "priority" => {
            if let Some(digit) = val.chars().find(|c| c.is_ascii_digit()).and_then(|c| c.to_digit(10)) {
                task.priority = digit.min(3);
            } else {
                let lower = val.to_ascii_lowercase();
                if lower.contains("urgent") {
                    task.priority = 0;
                } else if lower.contains("high") {
                    task.priority = 1;
                } else if lower.contains("normal") || lower.contains("medium") {
                    task.priority = 2;
                } else if lower.contains("low") {
                    task.priority = 3;
                }
            }
        }
        "severity" if task.severity.is_empty() || task.severity == "none" => {
            let s = val.to_ascii_lowercase();
            if s != "none" {
                task.severity = s;
            }
        }
        "owner" if task.owner.is_empty() || task.owner == "unassigned" => {
            if val != "Unassigned" && !val.is_empty() {
                task.owner = val.to_string();
            }
        }
        k if (k == "due" || k.starts_with("due")) && (task.due.is_empty() || task.due == "none") => {
            if val != "None" && !val.is_empty() {
                task.due = val.to_string();
            }
        }
        "labels" if val != "None" && !val.is_empty() => {
            for part in val.split(',') {
                let l = part.trim();
                if !l.is_empty() && !task.labels.contains(&l.to_string()) {
                    task.labels.push(l.to_string());
                }
            }
        }
        _ => {}
    }
}

fn parse_markdown_sections(body: &str, task: &mut TaskFileDetails) {
    #[derive(PartialEq)]
    enum Section {
        None,
        Title,
        Metadata,
        Repositories,
        Description,
        Criteria,
        PlannedFiles,
        Logs,
    }

    let mut current_section = Section::None;
    let mut active_fence: Option<(char, usize)> = None;
    let mut log_lines = Vec::new();
    let mut desc_lines = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();

        if let Some((f_char, fence_len)) = active_fence {
            let leading = trimmed.chars().take_while(|c| *c == f_char).count();
            if leading >= fence_len && trimmed[leading..].trim().is_empty() {
                active_fence = None;
                if current_section == Section::Logs {
                    continue;
                } else if current_section == Section::Description {
                    desc_lines.push(line);
                    continue;
                }
            } else if current_section == Section::Logs {
                log_lines.push(line);
                continue;
            } else if current_section == Section::Description {
                desc_lines.push(line);
                continue;
            }
        } else {
            let fence_char = if trimmed.starts_with("```") {
                Some('`')
            } else if trimmed.starts_with("~~~") {
                Some('~')
            } else {
                None
            };

            if let Some(ch) = fence_char {
                let count = trimmed.chars().take_while(|c| *c == ch).count();
                if count >= 3 {
                    active_fence = Some((ch, count));
                    if current_section == Section::Logs {
                        continue;
                    } else if current_section == Section::Description {
                        desc_lines.push(line);
                        continue;
                    }
                }
            }
        }

        if active_fence.is_some() {
            continue;
        }

        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim().to_ascii_lowercase();
            match heading.as_str() {
                "title" => {
                    current_section = Section::Title;
                    continue;
                }
                "repositories" => {
                    current_section = Section::Repositories;
                    continue;
                }
                "description" => {
                    current_section = Section::Description;
                    continue;
                }
                "acceptance criteria" | "checklist" | "subtasks" => {
                    current_section = Section::Criteria;
                    continue;
                }
                "planned files" | "files" => {
                    current_section = Section::PlannedFiles;
                    continue;
                }
                "raw logs" | "logs" => {
                    current_section = Section::Logs;
                    continue;
                }
                "task brief v1" | "gitpulse task" | "unsaved gitpulse task draft" => {
                    current_section = Section::Metadata;
                    continue;
                }
                _ => {}
            }
        }

        match current_section {
            Section::Title => {
                if !trimmed.is_empty() {
                    if let Some((k, v)) = trimmed.split_once(':') {
                        parse_kv_metadata_line(k, v, task);
                    } else if task.title.is_empty() {
                        task.title = trimmed.to_string();
                    }
                }
            }
            Section::Metadata | Section::None => {
                if let Some((k, v)) = trimmed.split_once(':') {
                    parse_kv_metadata_line(k, v, task);
                }
            }
            Section::Repositories => {
                if trimmed.starts_with('-') || trimmed.starts_with('*') {
                    let repo = trimmed[1..].trim();
                    if !repo.is_empty() && repo != "None linked yet." {
                        let clean = repo
                            .split(" — ")
                            .next()
                            .unwrap_or(repo)
                            .split(" - ")
                            .next()
                            .unwrap_or(repo)
                            .trim();
                        let clean = clean.split('[').next().unwrap_or(clean).trim();
                        if !clean.is_empty() && !task.repositories.contains(&clean.to_string()) {
                            task.repositories.push(clean.to_string());
                        }
                    }
                }
            }
            Section::Description => {
                desc_lines.push(line);
            }
            Section::Criteria => {
                if trimmed.starts_with("- [ ]") || trimmed.starts_with("- [x]") || trimmed.starts_with("- [X]") {
                    let crit = trimmed[5..].trim();
                    if !crit.is_empty() && !task.acceptance_criteria.contains(&crit.to_string()) {
                        task.acceptance_criteria.push(crit.to_string());
                    }
                } else if trimmed.starts_with('-') || trimmed.starts_with('*') {
                    let crit = trimmed[1..].trim();
                    if !crit.is_empty() && crit != "No acceptance criteria recorded." && !task.acceptance_criteria.contains(&crit.to_string()) {
                        task.acceptance_criteria.push(crit.to_string());
                    }
                }
            }
            Section::PlannedFiles => {
                if trimmed.starts_with('-') || trimmed.starts_with('*') {
                    let file = trimmed[1..].trim();
                    if !file.is_empty() && !task.planned_files.contains(&file.to_string()) {
                        task.planned_files.push(file.to_string());
                    }
                }
            }
            Section::Logs => {
                if !trimmed.starts_with("Pasted evidence") && (!trimmed.is_empty() || !log_lines.is_empty()) {
                    log_lines.push(line);
                }
            }
        }
    }

    if task.description.is_empty() {
        let desc = desc_lines.join("\n").trim().to_string();
        if desc != "(none)" && !desc.is_empty() {
            task.description = desc;
        }
    }

    if task.logs.is_none() && !log_lines.is_empty() {
        let log_content = log_lines.join("\n").trim().to_string();
        if !log_content.is_empty() {
            task.logs = Some(log_content);
        }
    }
}

/// Add a task to the repository's tasks folder with policy evaluation and ledger recording.
pub fn add_task_file(req: NewTaskRequest) -> Result<TaskAddResult, String> {
    let repo_path = Path::new(&req.repo_path);
    if !repo_path.exists() || !repo_path.is_dir() {
        return Err(format!("Repository path '{}' does not exist or is not a directory", req.repo_path));
    }

    let title = validate_title(&req.title)?;
    let description = validate_description(req.description.as_deref())?;
    let logs = validate_logs(req.logs.as_deref())?;

    let task_id = match req.task_id.as_deref() {
        Some(custom_id) => validate_task_id(custom_id)?,
        None => {
            let slug = slugify_title(&title);
            let candidate_id = format!("gp-{slug}");
            validate_task_id(&candidate_id)?
        }
    };

    let status = normalize_status(req.status.as_deref())?;
    let priority = normalize_priority(req.priority)?;
    let severity = req.severity.unwrap_or_else(|| "none".to_string()).trim().to_ascii_lowercase();
    let kind = req.kind.unwrap_or_else(|| "feature".to_string()).trim().to_ascii_lowercase();
    let owner = req.owner.unwrap_or_else(|| "unassigned".to_string()).trim().to_string();
    let due = req.due.unwrap_or_else(|| "none".to_string()).trim().to_string();

    let labels = clean_string_list(req.labels, MAX_TASK_LABELS, 128, "labels")?;
    let repositories = clean_string_list(req.repositories, MAX_TASK_REPOSITORIES, 300, "repositories")?;
    let planned_files = clean_string_list(req.planned_files, MAX_TASK_PLANNED_FILES, 4096, "planned_files")?;
    let acceptance_criteria = clean_string_list(req.acceptance_criteria, MAX_TASK_CRITERIA, 4096, "acceptance_criteria")?;

    let tasks_dir_rel = validate_tasks_dir(req.tasks_dir.as_deref())?;
    let filename = format!("{task_id}.md");
    let rel_file_path = tasks_dir_rel.join(&filename);
    let rel_file_path_str = rel_file_path.to_string_lossy().into_owned();

    let target_dir = repo_path.join(&tasks_dir_rel);
    let target_file = repo_path.join(&rel_file_path);

    let overwrite = req.overwrite.unwrap_or(false);
    if target_file.exists() && !overwrite {
        return Err(format!(
            "Task file already exists: {rel_file_path_str}. Specify overwrite: true to replace it."
        ));
    }

    // Policy check via harness guard_file
    let verdict = crate::harness::guard_file(&req.repo_path, &rel_file_path_str, "write")?;
    if verdict.status.blocks() {
        return Err(format!(
            "Task file creation refused by repository policy: {} (rule: {})",
            verdict.reason, verdict.rule
        ));
    }

    let task_details = TaskFileDetails {
        id: task_id.clone(),
        title: title.clone(),
        status: status.clone(),
        priority,
        severity,
        kind,
        owner,
        due,
        labels,
        repositories,
        planned_files,
        acceptance_criteria,
        description,
        logs,
    };

    let content = format_gitpulse_task(&task_details);

    // Create target directory if needed
    if !target_dir.exists() {
        fs::create_dir_all(&target_dir)
            .map_err(|e| format!("Failed to create tasks directory '{}': {e}", target_dir.display()))?;
    }

    if overwrite {
        // Atomic overwrite via unique temporary file + rename
        let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_filename = format!(".{task_id}.tmp.{}.{counter}", std::process::id());
        let tmp_path = target_dir.join(tmp_filename);

        fs::write(&tmp_path, content.as_bytes())
            .map_err(|e| format!("Failed to write temporary task file: {e}"))?;

        if let Err(e) = fs::rename(&tmp_path, &target_file) {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("Failed to commit task file '{}': {e}", target_file.display()));
        }
    } else {
        // Atomic exclusive creation: fails if file already exists with zero race condition
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target_file)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(format!(
                    "Task file already exists: {rel_file_path_str}. Specify overwrite: true to replace it."
                ));
            }
            Err(e) => {
                return Err(format!(
                    "Failed to create task file '{}': {e}",
                    target_file.display()
                ));
            }
        };

        file.write_all(content.as_bytes())
            .map_err(|e| format!("Failed to write task content to '{}': {e}", target_file.display()))?;
        file.flush()
            .map_err(|e| format!("Failed to flush task file '{}': {e}", target_file.display()))?;
    }

    Ok(TaskAddResult {
        ok: true,
        task_id,
        file_path: rel_file_path_str,
        absolute_path: target_file.to_string_lossy().into_owned(),
        title,
        status,
        priority,
        content,
        verdict,
    })
}

/// List all task files in the repository's tasks folder.
pub fn list_task_files(
    repo_path: &str,
    tasks_dir: Option<&str>,
    status_filter: Option<&str>,
    limit: usize,
) -> Result<TaskListResult, String> {
    let repo = Path::new(repo_path);
    if !repo.exists() || !repo.is_dir() {
        return Err(format!("Repository path '{repo_path}' does not exist or is not a directory"));
    }

    let tasks_dir_rel = validate_tasks_dir(tasks_dir)?;
    let tasks_dir_abs = repo.join(&tasks_dir_rel);
    let tasks_dir_str = tasks_dir_rel.to_string_lossy().into_owned();

    if !tasks_dir_abs.exists() || !tasks_dir_abs.is_dir() {
        return Ok(TaskListResult {
            ok: true,
            tasks_dir: tasks_dir_str,
            returned: 0,
            total: 0,
            truncated: false,
            tasks: Vec::new(),
        });
    }

    let filter_status = status_filter
        .map(|s| s.trim().to_ascii_lowercase().replace(['-', ' '], "_"))
        .filter(|s| !s.is_empty());

    let mut all_tasks = Vec::new();
    let entries = fs::read_dir(&tasks_dir_abs)
        .map_err(|e| format!("Failed to read tasks directory '{}': {e}", tasks_dir_abs.display()))?;

    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if filename.starts_with('.') {
                continue;
            }
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(task) = parse_gitpulse_task(&content) {
                    if let Some(ref desired_status) = filter_status {
                        if &task.status != desired_status {
                            continue;
                        }
                    }
                    let rel_path = tasks_dir_rel.join(filename).to_string_lossy().into_owned();
                    all_tasks.push(TaskSummary {
                        id: task.id,
                        title: task.title,
                        status: task.status,
                        priority: task.priority,
                        priority_label: priority_label(task.priority).to_string(),
                        severity: task.severity,
                        kind: task.kind,
                        owner: task.owner,
                        due: task.due,
                        labels: task.labels,
                        file_path: rel_path,
                        criteria_count: task.acceptance_criteria.len(),
                        planned_files_count: task.planned_files.len(),
                    });
                }
            }
        }
    }

    // Sort by priority (0=urgent first), then by title
    all_tasks.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.title.cmp(&b.title))
    });

    let total = all_tasks.len();
    let cap = limit.clamp(1, MAX_LIST_LIMIT);
    let truncated = total > cap;
    let tasks: Vec<TaskSummary> = all_tasks.into_iter().take(cap).collect();
    let returned = tasks.len();

    Ok(TaskListResult {
        ok: true,
        tasks_dir: tasks_dir_str,
        returned,
        total,
        truncated,
        tasks,
    })
}

/// Read and parse a specific task file by ID or relative path.
pub fn get_task_file(
    repo_path: &str,
    tasks_dir: Option<&str>,
    task_id_or_path: &str,
) -> Result<TaskGetResult, String> {
    let repo = Path::new(repo_path);
    if !repo.exists() || !repo.is_dir() {
        return Err(format!("Repository path '{repo_path}' does not exist or is not a directory"));
    }

    let raw = task_id_or_path.trim().trim_start_matches("./");
    if raw.is_empty() || raw.contains("..") || raw.contains('\0') || raw.contains('\\') {
        return Err("Invalid task ID or path: contains forbidden path traversal characters".to_string());
    }

    let tasks_dir_rel = validate_tasks_dir(tasks_dir)?;
    let tasks_dir_prefix = format!("{}/", tasks_dir_rel.to_string_lossy());
    let file_stem = raw
        .strip_prefix(&tasks_dir_prefix)
        .unwrap_or(raw)
        .trim_start_matches('/');

    if file_stem.is_empty() || file_stem.contains("..") || file_stem.contains('\0') || file_stem.contains('\\') {
        return Err("Invalid task ID or path: contains forbidden path traversal characters".to_string());
    }

    let candidate_rel = if file_stem.ends_with(".md") {
        tasks_dir_rel.join(file_stem)
    } else {
        tasks_dir_rel.join(format!("{file_stem}.md"))
    };

    let target_file = repo.join(&candidate_rel);
    if !target_file.exists() || !target_file.is_file() {
        return Err(format!(
            "Task file not found at '{}' in repository '{repo_path}'",
            candidate_rel.display()
        ));
    }

    if let (Ok(canon_repo), Ok(canon_file)) = (repo.canonicalize(), target_file.canonicalize()) {
        if !canon_file.starts_with(&canon_repo) {
            return Err("Task file resolves outside repository root".to_string());
        }
    }

    let content = fs::read_to_string(&target_file)
        .map_err(|e| format!("Failed to read task file '{}': {e}", target_file.display()))?;

    let task = parse_gitpulse_task(&content)?;

    Ok(TaskGetResult {
        ok: true,
        file_path: candidate_rel.to_string_lossy().into_owned(),
        task,
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse_round_trip() {
        let task = TaskFileDetails {
            id: "gp-auth-flow".to_string(),
            title: "Implement secure OAuth login flow".to_string(),
            status: "ready".to_string(),
            priority: 1,
            severity: "high".to_string(),
            kind: "feature".to_string(),
            owner: "@ada".to_string(),
            due: "2026-10-20".to_string(),
            labels: vec!["security".to_string(), "backend".to_string()],
            repositories: vec!["GitPulse".to_string()],
            planned_files: vec!["src/auth.rs".to_string(), "src/login.rs".to_string()],
            acceptance_criteria: vec![
                "Validates PKCE challenge".to_string(),
                "Persists refresh token securely".to_string(),
            ],
            description: "Replace legacy basic auth with hardened PKCE OAuth2 flow.".to_string(),
            logs: Some("Error 401: Unauthorized session token expired".to_string()),
        };

        let formatted = format_gitpulse_task(&task);
        assert!(formatted.contains("# Task brief v1"));
        assert!(formatted.contains("Task: gp-auth-flow"));
        assert!(formatted.contains("Priority: 1 (High)"));
        assert!(formatted.contains("- [ ] Validates PKCE challenge"));
        assert!(formatted.contains("- src/auth.rs"));
        assert!(formatted.contains("Error 401"));

        let parsed = parse_gitpulse_task(&formatted).expect("parsed");
        assert_eq!(parsed.id, task.id);
        assert_eq!(parsed.title, task.title);
        assert_eq!(parsed.status, task.status);
        assert_eq!(parsed.priority, task.priority);
        assert_eq!(parsed.severity, task.severity);
        assert_eq!(parsed.kind, task.kind);
        assert_eq!(parsed.owner, task.owner);
        assert_eq!(parsed.due, task.due);
        assert_eq!(parsed.labels, task.labels);
        assert_eq!(parsed.repositories, task.repositories);
        assert_eq!(parsed.planned_files, task.planned_files);
        assert_eq!(parsed.acceptance_criteria, task.acceptance_criteria);
        assert_eq!(parsed.description, task.description);
        assert_eq!(parsed.logs, task.logs);
    }

    #[test]
    fn parse_pure_markdown_without_frontmatter() {
        let md = r#"# Task brief v1

## Title
Fix race condition in debounce timer

Task: gp-fix-timer
Type: bug
Status: in_progress
Priority: 0 (Urgent)
Severity: critical
Owner: @linus
Due: 2026-10-06
Labels: runtime, bug

## Repositories
- GitPulse

## Description
A race condition occurs when concurrent timer events fire before cancel.

## Acceptance criteria
- [ ] Mutex guards state transition
- [ ] Stress test with 100 concurrent triggers

## Planned files
- src/watcher/debouncer.rs
"#;

        let parsed = parse_gitpulse_task(md).expect("parse markdown");
        assert_eq!(parsed.id, "gp-fix-timer");
        assert_eq!(parsed.title, "Fix race condition in debounce timer");
        assert_eq!(parsed.status, "in_progress");
        assert_eq!(parsed.priority, 0);
        assert_eq!(parsed.severity, "critical");
        assert_eq!(parsed.kind, "bug");
        assert_eq!(parsed.owner, "@linus");
        assert_eq!(parsed.labels, vec!["runtime", "bug"]);
        assert_eq!(parsed.repositories, vec!["GitPulse"]);
        assert_eq!(parsed.acceptance_criteria.len(), 2);
        assert_eq!(parsed.planned_files, vec!["src/watcher/debouncer.rs"]);
    }

    #[test]
    fn add_and_read_task_file_in_temp_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let init = std::process::Command::new("git")
            .args(["init", "-q", &repo_path])
            .output()
            .expect("git init");
        assert!(init.status.success());
        crate::test_support::trust_repo(dir.path());

        let req = NewTaskRequest {
            repo_path: repo_path.clone(),
            title: "Add offline caching for network requests".to_string(),
            description: Some("Cache GET requests with TTL in IndexedDB".to_string()),
            task_id: Some("gp-cache-layer".to_string()),
            status: Some("ready".to_string()),
            priority: Some(1),
            severity: Some("medium".to_string()),
            kind: Some("feature".to_string()),
            owner: Some("dev".to_string()),
            due: Some("2026-11-01".to_string()),
            labels: Some(vec!["offline".to_string(), "storage".to_string()]),
            repositories: Some(vec!["GitPulse".to_string()]),
            planned_files: Some(vec!["src/cache.rs".to_string()]),
            acceptance_criteria: Some(vec!["TTL is respected".to_string()]),
            logs: None,
            tasks_dir: Some("tasks".to_string()),
            overwrite: Some(false),
        };

        let result = add_task_file(req).expect("add task");
        assert!(result.ok);
        assert_eq!(result.task_id, "gp-cache-layer");
        assert_eq!(result.file_path, "tasks/gp-cache-layer.md");
        assert!(Path::new(&result.absolute_path).exists());

        // Read it back
        let read = get_task_file(&repo_path, Some("tasks"), "gp-cache-layer").expect("get task");
        assert_eq!(read.task.title, "Add offline caching for network requests");
        assert_eq!(read.task.status, "ready");
        assert_eq!(read.task.priority, 1);

        // List it
        let list = list_task_files(&repo_path, Some("tasks"), None, 50).expect("list tasks");
        assert_eq!(list.returned, 1);
        assert_eq!(list.tasks[0].id, "gp-cache-layer");
        assert_eq!(list.tasks[0].criteria_count, 1);

        // Overwrite without flag fails
        let dup_req = NewTaskRequest {
            repo_path: repo_path.clone(),
            title: "Another title".to_string(),
            description: None,
            task_id: Some("gp-cache-layer".to_string()),
            status: None,
            priority: None,
            severity: None,
            kind: None,
            owner: None,
            due: None,
            labels: None,
            repositories: None,
            planned_files: None,
            acceptance_criteria: None,
            logs: None,
            tasks_dir: Some("tasks".to_string()),
            overwrite: Some(false),
        };
        let dup_err = add_task_file(dup_req).unwrap_err();
        assert!(dup_err.contains("already exists"));

        // Overwrite with flag succeeds
        let overwrite_req = NewTaskRequest {
            repo_path,
            title: "Updated title".to_string(),
            description: None,
            task_id: Some("gp-cache-layer".to_string()),
            status: None,
            priority: None,
            severity: None,
            kind: None,
            owner: None,
            due: None,
            labels: None,
            repositories: None,
            planned_files: None,
            acceptance_criteria: None,
            logs: None,
            tasks_dir: Some("tasks".to_string()),
            overwrite: Some(true),
        };
        let overwrite_res = add_task_file(overwrite_req).expect("overwrite");
        assert_eq!(overwrite_res.title, "Updated title");
    }

    #[test]
    fn path_traversal_attempts_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_str().unwrap().to_string();

        let req = NewTaskRequest {
            repo_path: repo_path.clone(),
            title: "Exploit attempt".to_string(),
            description: None,
            task_id: Some("../../../escaped".to_string()),
            status: None,
            priority: None,
            severity: None,
            kind: None,
            owner: None,
            due: None,
            labels: None,
            repositories: None,
            planned_files: None,
            acceptance_criteria: None,
            logs: None,
            tasks_dir: Some("tasks".to_string()),
            overwrite: Some(false),
        };
        assert!(add_task_file(req).is_err());

        let req_dir = NewTaskRequest {
            repo_path,
            title: "Exploit attempt dir".to_string(),
            description: None,
            task_id: Some("legit".to_string()),
            status: None,
            priority: None,
            severity: None,
            kind: None,
            owner: None,
            due: None,
            labels: None,
            repositories: None,
            planned_files: None,
            acceptance_criteria: None,
            logs: None,
            tasks_dir: Some("../../../etc".to_string()),
            overwrite: Some(false),
        };
        assert!(add_task_file(req_dir).is_err());
    }

    #[test]
    fn title_slug_auto_generation_when_id_omitted() {
        let slug = slugify_title("Refactor Database Connection Pool & Cleanup!");
        assert_eq!(slug, "refactor-database-connection-pool-cleanup");
    }

    #[test]
    fn concurrent_task_creation_stress() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let init = std::process::Command::new("git")
            .args(["init", "-q", &repo_path])
            .output()
            .expect("git init");
        assert!(init.status.success());
        crate::test_support::trust_repo(dir.path());

        let handles: Vec<_> = (0..20)
            .map(|i| {
                let repo = repo_path.clone();
                std::thread::spawn(move || {
                    let req = NewTaskRequest {
                        repo_path: repo,
                        title: format!("Concurrent task {i}"),
                        description: Some(format!("Description for concurrent task {i}")),
                        task_id: Some(format!("gp-concurrent-{i:03}")),
                        status: Some("inbox".to_string()),
                        priority: Some((i % 4) as u32),
                        severity: Some("low".to_string()),
                        kind: Some("task".to_string()),
                        owner: Some(format!("worker-{i}")),
                        due: None,
                        labels: Some(vec!["concurrency".to_string()]),
                        repositories: Some(vec!["GitPulse".to_string()]),
                        planned_files: None,
                        acceptance_criteria: Some(vec!["Done".to_string()]),
                        logs: None,
                        tasks_dir: Some("tasks".to_string()),
                        overwrite: Some(false),
                    };
                    add_task_file(req)
                })
            })
            .collect();

        for h in handles {
            let res = h.join().unwrap().expect("task creation succeeded");
            assert!(res.ok);
        }

        let list = list_task_files(&repo_path, Some("tasks"), None, 100).expect("list tasks");
        assert_eq!(list.total, 20);
        assert_eq!(list.returned, 20);
        assert!(!list.truncated);
    }

    #[test]
    fn concurrent_duplicate_creation_race() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let init = std::process::Command::new("git")
            .args(["init", "-q", &repo_path])
            .output()
            .expect("git init");
        assert!(init.status.success());
        crate::test_support::trust_repo(dir.path());

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let repo = repo_path.clone();
                std::thread::spawn(move || {
                    let req = NewTaskRequest {
                        repo_path: repo,
                        title: "Raced Task".to_string(),
                        description: None,
                        task_id: Some("gp-raced-task".to_string()),
                        status: None,
                        priority: None,
                        severity: None,
                        kind: None,
                        owner: None,
                        due: None,
                        labels: None,
                        repositories: None,
                        planned_files: None,
                        acceptance_criteria: None,
                        logs: None,
                        tasks_dir: Some("tasks".to_string()),
                        overwrite: Some(false),
                    };
                    add_task_file(req)
                })
            })
            .collect();

        let mut successes = 0;
        let mut failures = 0;
        for h in handles {
            match h.join().unwrap() {
                Ok(_) => successes += 1,
                Err(err) => {
                    assert!(err.contains("already exists"), "unexpected err: {err}");
                    failures += 1;
                }
            }
        }
        assert_eq!(successes, 1, "exactly one creation must win the race");
        assert_eq!(failures, 9, "all racing duplicates must be refused");
    }

    #[test]
    fn malformed_and_adversarial_files_in_tasks_directory() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let tasks_dir = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();

        // 1. Empty file
        std::fs::write(tasks_dir.join("empty.md"), b"").unwrap();
        // 2. Corrupted frontmatter
        std::fs::write(tasks_dir.join("corrupt.md"), b"---\ninvalid yaml: [[\n---\nbody").unwrap();
        // 3. Dotfile (should be skipped)
        std::fs::write(tasks_dir.join(".hidden.md"), b"some secret").unwrap();
        // 4. Non-md file (should be skipped)
        std::fs::write(tasks_dir.join("notes.txt"), b"some notes").unwrap();
        // 5. Valid task
        std::fs::write(
            tasks_dir.join("valid.md"),
            b"---\nid: \"valid-task\"\ntitle: \"Valid Task\"\nstatus: inbox\npriority: 2\n---\n# Task brief v1\n",
        ).unwrap();

        let list = list_task_files(&repo_path, Some("tasks"), None, 50).expect("list tasks");
        // Only valid.md should be in the listing, corrupted/empty/dotfiles safely ignored
        assert_eq!(list.returned, 1);
        assert_eq!(list.tasks[0].id, "valid-task");

        // get_task_file on empty or corrupt should error cleanly without panicking
        assert!(get_task_file(&repo_path, Some("tasks"), "empty").is_err());
    }

    #[test]
    fn unicode_and_boundary_input_validation() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let init = std::process::Command::new("git")
            .args(["init", "-q", &repo_path])
            .output()
            .expect("git init");
        assert!(init.status.success());
        crate::test_support::trust_repo(dir.path());

        // Oversized title
        let huge_title = "A".repeat(MAX_TASK_TITLE + 1);
        let req = NewTaskRequest {
            repo_path: repo_path.clone(),
            title: huge_title,
            description: None,
            task_id: None,
            status: None,
            priority: None,
            severity: None,
            kind: None,
            owner: None,
            due: None,
            labels: None,
            repositories: None,
            planned_files: None,
            acceptance_criteria: None,
            logs: None,
            tasks_dir: None,
            overwrite: None,
        };
        assert!(add_task_file(req).is_err());

        // Control characters in title
        let ctrl_title = "Title\x07With\x1bEscapes";
        let req_ctrl = NewTaskRequest {
            repo_path: repo_path.clone(),
            title: ctrl_title.to_string(),
            description: None,
            task_id: None,
            status: None,
            priority: None,
            severity: None,
            kind: None,
            owner: None,
            due: None,
            labels: None,
            repositories: None,
            planned_files: None,
            acceptance_criteria: None,
            logs: None,
            tasks_dir: None,
            overwrite: None,
        };
        assert!(add_task_file(req_ctrl).is_err());

        // Unicode emoji and multi-lingual characters
        let unicode_req = NewTaskRequest {
            repo_path: repo_path.clone(),
            title: "Support multilingual UTF-8: 🚀 ログ & 日本語".to_string(),
            description: Some("Detailed desc: café, naïve, ⚡".to_string()),
            task_id: Some("gp-unicode-task".to_string()),
            status: Some("ready".to_string()),
            priority: Some(2),
            severity: Some("none".to_string()),
            kind: Some("feature".to_string()),
            owner: Some("@ユーザー".to_string()),
            due: Some("2026-12-31".to_string()),
            labels: Some(vec!["i18n".to_string(), "日本語".to_string()]),
            repositories: Some(vec!["GitPulse".to_string()]),
            planned_files: Some(vec!["src/i18n.rs".to_string()]),
            acceptance_criteria: Some(vec!["UTF-8 round trip works 💯".to_string()]),
            logs: None,
            tasks_dir: None,
            overwrite: None,
        };
        let add_res = add_task_file(unicode_req).expect("add unicode task");
        assert!(add_res.ok);

        let get_res = get_task_file(&repo_path, None, "gp-unicode-task").expect("get unicode task");
        assert_eq!(get_res.task.title, "Support multilingual UTF-8: 🚀 ログ & 日本語");
        assert_eq!(get_res.task.owner, "@ユーザー");
        assert!(get_res.content.contains("UTF-8 round trip works 💯"));
    }

    #[test]
    fn hardened_path_variants_and_dc_store_compatibility() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let init = std::process::Command::new("git")
            .args(["init", "-q", &repo_path])
            .output()
            .expect("git init");
        assert!(init.status.success());
        crate::test_support::trust_repo(dir.path());

        // 1. Create a task with complex raw logs containing backticks
        let logs_with_backticks = "Error occurred:\n```rust\nlet x = 42;\n```\nExtra `code` quote.";
        let req = NewTaskRequest {
            repo_path: repo_path.clone(),
            title: "Task with embedded backticks in logs".to_string(),
            description: Some("Context description".to_string()),
            task_id: Some("gp-backticks-test".to_string()),
            status: Some("in-progress".to_string()), // hyphenated status
            priority: Some(1),
            severity: Some("high".to_string()),
            kind: Some("bug".to_string()),
            owner: Some("@bob".to_string()),
            due: Some("2026-06-01".to_string()),
            labels: Some(vec!["bug".to_string()]),
            repositories: Some(vec!["GitPulse".to_string()]),
            planned_files: Some(vec!["src/main.rs".to_string()]),
            acceptance_criteria: Some(vec!["Fix backtick escape".to_string()]),
            logs: Some(logs_with_backticks.to_string()),
            tasks_dir: None,
            overwrite: None,
        };
        let add_res = add_task_file(req).expect("create task");
        assert!(add_res.ok);
        assert_eq!(add_res.status, "in_progress"); // normalized from in-progress

        // 2. Fetch using various path formats:
        // Bare ID
        let get1 = get_task_file(&repo_path, None, "gp-backticks-test").expect("get bare id");
        assert_eq!(get1.task.id, "gp-backticks-test");
        assert_eq!(get1.task.logs.as_deref(), Some(logs_with_backticks));

        // With .md
        let get2 = get_task_file(&repo_path, None, "gp-backticks-test.md").expect("get .md");
        assert_eq!(get2.task.id, "gp-backticks-test");

        // With tasks/ prefix
        let get3 = get_task_file(&repo_path, None, "tasks/gp-backticks-test.md").expect("get tasks/ prefix");
        assert_eq!(get3.task.id, "gp-backticks-test");

        // With ./tasks/ prefix
        let get4 = get_task_file(&repo_path, None, "./tasks/gp-backticks-test.md").expect("get ./tasks/ prefix");
        assert_eq!(get4.task.id, "gp-backticks-test");

        // 3. Test filter status with hyphenated query
        let list_hyphen = list_task_files(&repo_path, None, Some("in-progress"), 10).expect("list hyphen");
        assert_eq!(list_hyphen.total, 1);
        assert_eq!(list_hyphen.tasks[0].id, "gp-backticks-test");

        // 4. Test dc-store format parsing
        let dc_brief = r#"# Task brief v1

## Title
Fix crash on startup

Task: gp-crash-fix (revision 4)
Updated (Unix seconds): 1774900000
Type: bug
Status: In Progress
Severity: critical
Owner: @charlie
Priority: 0 (Urgent)
Due (Unix seconds): 1775000000
Labels: stability, core
Enhancement field locks: none

## Repositories
- GitPulse [repo-1] (revision 2) — primary

Home workspace: Default

## Description
Application crashes when reading corrupt cache file.

## Acceptance criteria
- [ ] Reproduce crash with fixture
- [ ] Add defensive error handling
- [ ] Unit tests passing

## Raw logs
Pasted evidence. Keep stack frames, timestamps, error codes and quoted text exactly as written.

```
thread 'main' panicked at 'called `Option::unwrap()` on a `None` value'
src/cache.rs:42:10
```
"#;
        let parsed = parse_gitpulse_task(dc_brief).expect("parse dc-store format");
        assert_eq!(parsed.id, "gp-crash-fix"); // stripped (revision 4)
        assert_eq!(parsed.title, "Fix crash on startup");
        assert_eq!(parsed.status, "in_progress"); // normalized from In Progress
        assert_eq!(parsed.priority, 0); // parsed from 0 (Urgent)
        assert_eq!(parsed.severity, "critical");
        assert_eq!(parsed.kind, "bug");
        assert_eq!(parsed.owner, "@charlie");
        assert_eq!(parsed.due, "1775000000"); // extracted from Due (Unix seconds)
        assert_eq!(parsed.repositories, vec!["GitPulse".to_string()]); // stripped [repo-1] (revision 2) — primary
        assert_eq!(parsed.acceptance_criteria.len(), 3);
        assert!(parsed.logs.as_ref().unwrap().contains("panicked at"));
        assert!(!parsed.logs.as_ref().unwrap().contains("Pasted evidence"));

        // 5. Test frontmatter with string labels for priority
        let frontmatter_labels = r#"---
id: gp-label-priority
title: Label priority test
status: In Progress
priority: urgent
labels:
  - test
---
# Task brief v1

## Title
Label priority test
"#;
        let parsed_fm = parse_gitpulse_task(frontmatter_labels).expect("parse frontmatter labels");
        assert_eq!(parsed_fm.id, "gp-label-priority");
        assert_eq!(parsed_fm.status, "in_progress");
        assert_eq!(parsed_fm.priority, 0); // "urgent" -> 0
    }
}
