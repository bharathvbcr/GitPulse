//! Prompt assembly and diff budgeting for the local-model features.
//!
//! Everything here is pure text work, so it is unit-testable without a model
//! server: what gets sent, how a diff too large for the window is cut down,
//! and how a reply is read back into a commit message.

/// Roughly how many characters a token is worth for budgeting purposes.
///
/// Deliberately conservative: under-filling the window costs a little context,
/// over-filling it costs the request. The harness's `chat.prepare` gives the
/// calibrated number afterwards; this is only what decides how much diff to
/// offer it in the first place.
const CHARS_PER_TOKEN: usize = 3;

/// A diff cut to fit, and an honest account of what was cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetedDiff {
    pub text: String,
    pub truncated: bool,
    pub used_bytes: usize,
    pub total_bytes: usize,
    pub omitted_files: usize,
}

/// Cuts a diff to `max_bytes`, at file and hunk boundaries where it can.
///
/// The cut is announced inside the text itself as well as reported in the
/// struct: the model is told its view is partial, so it does not describe a
/// change it never saw, and the UI is told too, so the user knows why.
pub fn budget_diff(diff: &str, max_bytes: usize) -> BudgetedDiff {
    let total_bytes = diff.len();
    if total_bytes <= max_bytes {
        return BudgetedDiff {
            text: diff.to_string(),
            truncated: false,
            used_bytes: total_bytes,
            total_bytes,
            omitted_files: 0,
        };
    }

    let mut kept = String::with_capacity(max_bytes);
    let mut files_seen = 0usize;
    let mut files_kept = 0usize;
    let mut stopped = false;

    for line in diff.split_inclusive('\n') {
        let starts_file = line.starts_with("diff --git ");
        if starts_file {
            files_seen += 1;
        }
        if stopped {
            continue;
        }
        if kept.len() + line.len() > max_bytes {
            stopped = true;
            continue;
        }
        if starts_file {
            files_kept += 1;
        }
        kept.push_str(line);
    }

    // A single file larger than the whole budget leaves nothing: keep a head
    // of it rather than sending an empty diff.
    if kept.is_empty() {
        let budget = max_bytes.saturating_sub(1);
        let mut cut = budget.min(diff.len());
        while cut > 0 && !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        kept.push_str(&diff[..cut]);
        files_kept = files_seen.min(1);
    }

    let omitted_files = files_seen.saturating_sub(files_kept);
    let used_bytes = kept.len();
    kept.push_str(&format!(
        "\n\n[diff truncated: {} of {} bytes shown{}]\n",
        used_bytes,
        total_bytes,
        if omitted_files > 0 {
            format!(", {} further file(s) not shown", omitted_files)
        } else {
            String::new()
        }
    ));

    BudgetedDiff {
        text: kept,
        truncated: true,
        used_bytes,
        total_bytes,
        omitted_files,
    }
}

/// How many diff bytes fit in a window, once the scaffolding is paid for.
///
/// `reserved_tokens` covers the system prompt, the instructions, the reply and
/// the chat template. The result is floored so a tiny window still sends
/// something rather than an empty diff the model would answer anyway.
pub fn diff_budget_bytes(context_window: i64, reserved_tokens: i64) -> usize {
    let usable = (context_window - reserved_tokens).max(1_024);
    let bytes = (usable as usize).saturating_mul(CHARS_PER_TOKEN);
    bytes.clamp(2_048, 512 * 1024)
}

/// Cuts arbitrary context text to `max_bytes` at a line boundary where it can.
///
/// The same honesty contract as `budget_diff`, for payloads that are not
/// diffs (dependency-health reports, logs): the model is told its view is
/// partial inside the text itself, and the caller is told via the struct.
pub fn budget_text(text: &str, max_bytes: usize) -> BudgetedDiff {
    let total_bytes = text.len();
    if total_bytes <= max_bytes {
        return BudgetedDiff {
            text: text.to_string(),
            truncated: false,
            used_bytes: total_bytes,
            total_bytes,
            omitted_files: 0,
        };
    }

    let mut kept = String::with_capacity(max_bytes);
    for line in text.split_inclusive('\n') {
        if kept.len() + line.len() > max_bytes {
            break;
        }
        kept.push_str(line);
    }
    // A single line longer than the whole budget still yields a head rather
    // than an empty context.
    if kept.is_empty() {
        let mut cut = max_bytes.min(text.len());
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        kept.push_str(&text[..cut]);
    }
    let used_bytes = kept.len();
    kept.push_str(&format!(
        "\n\n[context truncated: {} of {} bytes shown]\n",
        used_bytes, total_bytes
    ));
    BudgetedDiff {
        text: kept,
        truncated: true,
        used_bytes,
        total_bytes,
        omitted_files: 0,
    }
}

/// System prompt for commit-message generation.
pub fn commit_message_system(style_hint: &str) -> String {
    let mut s = String::from(
        "You write Git commit messages for a developer who is about to commit staged changes.\n\
         Rules:\n\
         - Reply with the commit message only. No preamble, no code fences, no quotes.\n\
         - First line: imperative mood, at most 72 characters, no trailing period.\n\
         - Then a blank line, then 1-3 short bullet lines starting with '- ' explaining what \
           changed and why. Omit the body entirely for a small, self-evident change.\n\
         - Describe only what the diff shows. Never invent issue numbers, ticket ids or \
           co-authors.\n",
    );
    if !style_hint.is_empty() {
        s.push_str(style_hint);
    }
    s
}

/// Builds the style hint from the subjects this repository already uses.
pub fn style_hint_from_history(recent_subjects: &[String]) -> String {
    let samples: Vec<&String> = recent_subjects.iter().take(8).collect();
    if samples.is_empty() {
        return String::new();
    }
    let conventional = samples.iter().filter(|s| is_conventional(s)).count();
    let mut hint = String::from("\nMatch the conventions already used in this repository.\n");
    if conventional * 2 >= samples.len() {
        hint.push_str(
            "This repository uses Conventional Commits, so start the subject with a type such as \
             feat:, fix:, refactor:, docs:, test: or chore:, with an optional (scope).\n",
        );
    }
    hint.push_str("Recent subjects from this repository:\n");
    for s in samples {
        hint.push_str("- ");
        hint.push_str(s.trim());
        hint.push('\n');
    }
    hint
}

fn is_conventional(subject: &str) -> bool {
    let head = subject.split(':').next().unwrap_or("");
    if head.len() == subject.len() {
        return false;
    }
    let base = head.split('(').next().unwrap_or("").trim_end_matches('!');
    matches!(
        base,
        "feat"
            | "fix"
            | "refactor"
            | "docs"
            | "test"
            | "chore"
            | "perf"
            | "build"
            | "ci"
            | "style"
            | "revert"
    )
}

/// User turn for commit-message generation.
pub fn commit_message_user(branch: &str, files: &[String], diff: &str) -> String {
    let mut s = String::new();
    if !branch.is_empty() {
        s.push_str(&format!("Current branch: {}\n", branch));
    }
    if !files.is_empty() {
        s.push_str("Staged files:\n");
        for f in files.iter().take(50) {
            s.push_str("- ");
            s.push_str(f);
            s.push('\n');
        }
        if files.len() > 50 {
            s.push_str(&format!("- …and {} more\n", files.len() - 50));
        }
    }
    s.push_str("\nStaged diff:\n```diff\n");
    s.push_str(diff);
    s.push_str("\n```\n\nWrite the commit message.");
    s
}

/// System prompt for explaining an existing commit.
pub fn explain_system() -> String {
    String::from(
        "You explain Git commits to a developer reading history.\n\
         Reply with 2-5 sentences of plain prose, then at most three '- ' bullets naming the \
         concrete changes. Say what the change does and what it affects. Describe only what the \
         diff and metadata show, and say plainly when the diff shown is partial.",
    )
}

/// User turn for explaining a commit.
pub fn explain_user(subject: &str, author: &str, date: &str, body: &str, diff: &str) -> String {
    let mut s = format!("Commit: {}\nAuthor: {}\nDate: {}\n", subject, author, date);
    if !body.trim().is_empty() {
        s.push_str("\nMessage body:\n");
        s.push_str(body.trim());
        s.push('\n');
    }
    s.push_str("\nDiff:\n```diff\n");
    s.push_str(diff);
    s.push_str("\n```\n\nExplain this commit.");
    s
}

/// System prompt for turning dependency-health findings into a fix plan.
pub fn health_fix_system() -> String {
    String::from(
        "You turn dependency-health findings into a remediation plan a developer can follow.\n\
         Rules:\n\
         - Reply with a numbered plan of concrete steps. Where a step is a command, show the\n\
           exact command in an inline code span.\n\
         - Order steps by severity: critical and high vulnerabilities first, then warnings,\n\
           then routine updates.\n\
         - Use only what the report shows. Never invent package versions, advisories or\n\
           findings; when no fixed version is reported, say so instead of guessing one.\n\
         - Flag major version bumps as potentially breaking and say what to check after them\n\
           (tests, lockfile regeneration, peer-dependency ranges).\n\
         - End with one short verification step the developer can run to confirm the fix.",
    )
}

/// User turn carrying the formatted health report.
pub fn health_fix_user(report: &str) -> String {
    format!(
        "Dependency-health report for this repository:\n\n```\n{}\n```\n\n\
         Write the remediation plan.",
        report
    )
}

/// System prompt for turning rendered test-coverage findings into a
/// prioritized analysis and action plan.
pub fn coverage_report_system() -> String {
    String::from(
        "You turn test-coverage findings into an analysis a developer can act on.\n\
         Rules:\n\
         - Reply with a short prose summary of overall coverage health first.\n\
         - Then give a numbered plan of concrete steps. Where a step is a command, show the\n\
           exact command in an inline code span.\n\
         - Use only what the report shows. Never invent percentages, file names or counts; when\n\
           the report says artifacts are missing or skipped, address generating them instead of\n\
           pretending the data exists.\n\
         - Order steps by impact: the lowest-percentage and largest uncovered areas first, and\n\
           say which language each step concerns.\n\
         - End with one short verification step the developer can run to confirm progress.",
    )
}

/// User turn carrying the rendered test-coverage report.
pub fn coverage_report_user(report: &str) -> String {
    format!(
        "Test-coverage report for this repository:\n\n```\n{}\n```\n\n\
         Write the coverage analysis.",
        report
    )
}

/// System prompt for branch-name suggestion.
pub fn branch_name_system() -> String {
    String::from(
        "You name Git branches. Reply with one branch name and nothing else.\n\
         Use lowercase kebab-case with a conventional prefix (feat/, fix/, refactor/, docs/, \
         chore/), at most 40 characters, no spaces, no trailing slash, ASCII only.",
    )
}

/// Reads a model reply back into a commit message.
///
/// Local models wrap answers in fences, restate the question, and — when the
/// harness is not available to settle the reply — leave thinking tags in the
/// text. All three are stripped here so the composer is not filled with markup
/// the user has to delete by hand.
pub fn clean_commit_message(raw: &str) -> String {
    let text = strip_think_tags(raw);
    // Preamble first, fence second: a model that says "Here is the message:"
    // before opening the fence leaves the fence on line two, where a
    // fence-first pass cannot see it.
    let mut lines: Vec<&str> = text.lines().collect();

    while let Some(first) = lines.first() {
        let lowered = first.trim().to_ascii_lowercase();
        let is_preamble = lowered.starts_with("here is")
            || lowered.starts_with("here's")
            || lowered.starts_with("commit message:")
            || lowered.starts_with("sure,")
            || first.trim().is_empty();
        if is_preamble {
            lines.remove(0);
        } else {
            break;
        }
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }

    let joined = strip_code_fence(&lines.join("\n"));
    let joined = joined.trim();
    // A whole message wrapped in quotes is the other common shape.
    let unquoted = if joined.len() > 1 && joined.starts_with('"') && joined.ends_with('"') {
        &joined[1..joined.len() - 1]
    } else {
        joined
    };
    unquoted.trim().to_string()
}

/// Reads a model reply back into a branch name.
pub fn clean_branch_name(raw: &str) -> String {
    let text = strip_think_tags(raw);
    let text = strip_code_fence(text.trim());
    let candidate = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .trim_matches(|c: char| c == '"' || c == '`' || c == '\'');
    let mut cleaned = String::with_capacity(candidate.len());
    for ch in candidate.chars() {
        let mapped = match ch {
            'a'..='z' | '0'..='9' | '/' | '-' | '_' | '.' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            ' ' | '\t' => '-',
            _ => continue,
        };
        // Collapse runs of separators rather than emitting `feat//a--b`.
        if (mapped == '-' || mapped == '/') && cleaned.ends_with(mapped) {
            continue;
        }
        cleaned.push(mapped);
    }
    cleaned
        .trim_matches(|c| c == '-' || c == '/' || c == '.')
        .chars()
        .take(60)
        .collect()
}

/// Removes `<think>…</think>` blocks, including one left unclosed by a
/// truncated reply.
pub fn strip_think_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    loop {
        match rest.find("<think>") {
            Some(start) => {
                out.push_str(&rest[..start]);
                let after = &rest[start + "<think>".len()..];
                match after.find("</think>") {
                    Some(end) => rest = &after[end + "</think>".len()..],
                    // Unclosed: the whole remainder is thinking.
                    None => return out.trim().to_string(),
                }
            }
            None => {
                out.push_str(rest);
                return out.trim().to_string();
            }
        }
    }
}

fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let mut lines: Vec<&str> = trimmed.lines().collect();
    lines.remove(0);
    if lines.last().is_some_and(|l| l.trim().starts_with("```")) {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n\
                        diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\n";

    #[test]
    fn a_diff_within_budget_is_untouched() {
        let out = budget_diff(DIFF, 10_000);
        assert!(!out.truncated);
        assert_eq!(out.text, DIFF);
        assert_eq!(out.used_bytes, DIFF.len());
        assert_eq!(out.omitted_files, 0);
    }

    #[test]
    fn truncation_cuts_at_a_file_boundary_and_says_so() {
        let out = budget_diff(DIFF, 80);
        assert!(out.truncated);
        assert_eq!(out.omitted_files, 1);
        assert!(out.text.contains("diff --git a/a.rs"));
        assert!(!out.text.contains("diff --git a/b.rs"));
        assert!(out.text.contains("[diff truncated"));
        assert!(out.used_bytes <= 80);
        assert_eq!(out.total_bytes, DIFF.len());
    }

    #[test]
    fn a_single_oversized_file_still_yields_a_head() {
        let huge = format!("diff --git a/x.rs b/x.rs\n{}", "+line\n".repeat(5_000));
        let out = budget_diff(&huge, 200);
        assert!(out.truncated);
        assert!(!out.text.is_empty());
        assert!(out.used_bytes <= 200);
    }

    #[test]
    fn budget_scales_with_the_window_and_stays_bounded() {
        assert!(diff_budget_bytes(262_144, 2_000) <= 512 * 1024);
        assert!(diff_budget_bytes(4_096, 2_000) >= 2_048);
        assert!(diff_budget_bytes(32_768, 2_000) > diff_budget_bytes(8_192, 2_000));
    }

    /// Regression (audit L1): the single-file fallback truncated by CHARS,
    /// so a multibyte diff could exceed `max_bytes` up to 4x while reporting
    /// those inflated numbers as bytes to the model and UI.
    #[test]
    fn oversized_multibyte_diff_respects_the_byte_budget() {
        let line = "héllo wörld\n";
        let diff = format!(
            "diff --git a/ünïcode b/ünïcode\nindex 1..2\n--- a/ünïcode\n+++ b/ünïcode\n@@ -1 +1 @@\n{}",
            line.repeat(50)
        );
        let budgeted = budget_diff(&diff, 64);
        assert!(budgeted.truncated);
        assert!(
            budgeted.used_bytes <= 64,
            "used {} bytes against a 64-byte budget",
            budgeted.used_bytes
        );
        // used_bytes reports the diff payload; the truncation notice rides
        // on top, so the invariant is on the payload, not the whole text.
    }

    #[test]
    fn think_tags_are_removed_open_or_closed() {
        assert_eq!(strip_think_tags("<think>plan</think>feat: x"), "feat: x");
        assert_eq!(strip_think_tags("answer <think>tail"), "answer");
        assert_eq!(strip_think_tags("plain"), "plain");
    }

    #[test]
    fn commit_messages_are_read_out_of_fenced_chatty_replies() {
        let raw = "Here is the commit message:\n```\nfeat(auth): add token refresh\n\n- rotate on 401\n```";
        assert_eq!(
            clean_commit_message(raw),
            "feat(auth): add token refresh\n\n- rotate on 401"
        );
        assert_eq!(clean_commit_message("\"fix: typo\""), "fix: typo");
        assert_eq!(
            clean_commit_message("<think>hmm</think>\nfix: guard nil"),
            "fix: guard nil"
        );
    }

    #[test]
    fn branch_names_are_normalised_to_something_git_accepts() {
        assert_eq!(
            clean_branch_name("Feat/Add Token Refresh"),
            "feat/add-token-refresh"
        );
        assert_eq!(clean_branch_name("`fix/nil-guard`"), "fix/nil-guard");
        assert_eq!(clean_branch_name("feat//a  b"), "feat/a-b");
        assert_eq!(clean_branch_name("-- trailing --"), "trailing");
    }

    #[test]
    fn style_hint_reflects_the_repository_it_was_built_from() {
        let conventional = vec!["feat: a".to_string(), "fix(core): b".to_string()];
        assert!(style_hint_from_history(&conventional).contains("Conventional Commits"));
        let prose = vec!["Add a thing".to_string(), "Tidy up".to_string()];
        assert!(!style_hint_from_history(&prose).contains("Conventional Commits"));
        assert!(style_hint_from_history(&[]).is_empty());
    }

    #[test]
    fn text_within_budget_passes_through_untouched() {
        let report = "# Dependency health report\n2 critical · 1 high\n";
        let out = budget_text(report, 10_000);
        assert!(!out.truncated);
        assert_eq!(out.text, report);
        assert_eq!(out.used_bytes, report.len());
    }

    #[test]
    fn oversized_text_cuts_at_a_line_boundary_and_announces_the_cut() {
        let report = format!("{}[long tail]\n", "- finding line with detail\n".repeat(50));
        let out = budget_text(&report, 300);
        assert!(out.truncated);
        assert_eq!(out.total_bytes, report.len());
        assert!(out.used_bytes <= 300 + "[context truncated".len());
        assert!(out.text.starts_with("- finding line"));
        assert!(out.text.ends_with("bytes shown]\n"));
        // The model is told its view is partial inside the text itself.
        assert!(out.text.contains("[context truncated:"));
    }

    #[test]
    fn a_single_oversized_line_still_yields_a_head() {
        let huge = "x".repeat(5_000);
        let out = budget_text(&huge, 100);
        assert!(out.truncated);
        assert!(!out.text.is_empty());
        assert!(out.used_bytes <= 100 + "[context truncated…".len());
    }

    #[test]
    fn oversized_multibyte_text_respects_the_byte_budget() {
        let line = "héllo wörld 你好\n";
        let report = line.repeat(40);
        let out = budget_text(&report, 64);
        assert!(out.truncated);
        assert!(
            out.used_bytes <= 64,
            "used {} bytes against a 64-byte budget",
            out.used_bytes
        );
        assert!(out.text.contains("[context truncated:"));
    }

    #[test]
    fn health_fix_prompts_bind_the_plan_to_the_report() {
        let system = health_fix_system();
        assert!(system.contains("Never invent package versions"));
        assert!(system.contains("verification step"));

        let user = health_fix_user("REPORT BODY");
        assert!(user.contains("```\nREPORT BODY\n```"));
        assert!(user.ends_with("Write the remediation plan."));
    }

    /// The frontend parses numbered steps with backtick commands into runnable
    /// actions, so the system prompt must demand inline code spans, and it
    /// must forbid invented numbers — a coverage analysis full of percentages
    /// the report never stated is worse than none.
    #[test]
    fn coverage_report_prompts_bind_the_plan_to_the_report() {
        let system = coverage_report_system();
        assert!(system.contains("inline code span"));
        assert!(system.contains("Never invent percentages"));
        assert!(system.contains("verification step"));

        let user = coverage_report_user("REPORT BODY");
        assert!(user.contains("```\nREPORT BODY\n```"));
        assert!(user.ends_with("Write the coverage analysis."));
    }
}
