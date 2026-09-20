//! `git blame`, bounded and parsed, with every refusal named.
//!
//! Blame is unbounded work on attacker-shaped input: a large file's
//! `--line-porcelain` output is roughly an order of magnitude bigger than the
//! file, and the file itself has no size limit. Every invocation here carries a
//! wall-clock deadline and an output cap, the same discipline
//! `devmap-query`'s churn reader applies to `git log`.
//!
//! A capped read is **refused**, not returned. A prefix of a blame is not a
//! blame: the lines it does not carry are exactly the ones a caller would
//! conclude nothing touched.

use std::path::Path;
use std::time::Duration;

use dc_proc::{git_with_program, run_bounded, Bounds, Failure};
use serde::{Deserialize, Serialize};

use crate::BlobIdentity;

/// Wall clock for one blame. Past it the child is killed.
pub const BLAME_DEADLINE: Duration = Duration::from_secs(20);

/// Bytes of `--line-porcelain` output kept. Past it the read is refused.
///
/// Porcelain repeats a full header for every line in a commit block, so this
/// is far larger than the source it describes — 32 MiB corresponds to roughly
/// a 2 MiB source file, well past anything this analysis is useful on.
pub const BLAME_OUTPUT_CAP: usize = 32 * 1024 * 1024;

/// Bytes of stderr kept for the refusal message.
pub const BLAME_STDERR_CAP: usize = 8 * 1024;

/// The all-zero object id `git blame` uses for a line that is not committed.
///
/// Matched by its characters rather than its length so both object formats are
/// covered at once: sha1 gives 40 zeroes and sha256 gives 64, and a check
/// written against one silently fails to recognise the other.
fn is_uncommitted_oid(oid: &str) -> bool {
    !oid.is_empty() && oid.chars().all(|character| character == '0')
}

/// One blamed line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameLine {
    /// One-based, matching `git blame` and [`crate::join`].
    pub line_no: u32,
    pub commit: String,
    pub author: String,
    pub author_mail: String,
    pub author_time: i64,
    /// True when the line is not committed — a worktree edit. Such a line has
    /// no commit to suspect and must never be rendered as a link to one.
    pub uncommitted: bool,
}

/// Why a blame produced nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlameRefusal {
    /// `git` could not be started.
    GitUnavailable { reason: String },
    /// The child was still running at [`BLAME_DEADLINE`].
    Deadline,
    /// The output passed [`BLAME_OUTPUT_CAP`]. A prefix is not an answer.
    OutputCapped { cap: usize },
    /// `git blame` exited non-zero — no such path at that rev, a binary file,
    /// a directory.
    GitRefused { status: String, stderr: String },
    /// The output did not parse as `--line-porcelain`.
    Unparsable { detail: String },
}

impl BlameRefusal {
    pub fn describe(&self) -> String {
        match self {
            BlameRefusal::GitUnavailable { reason } => format!("git could not be run: {reason}"),
            BlameRefusal::Deadline => {
                format!("git blame exceeded {BLAME_DEADLINE:?} and was killed")
            }
            BlameRefusal::OutputCapped { cap } => format!(
                "git blame produced more than {cap} bytes; a prefix of a blame is not a blame, \
                 because the lines it omits are exactly the ones that would read as untouched"
            ),
            BlameRefusal::GitRefused { status, stderr } => {
                if stderr.is_empty() {
                    format!("git blame exited {status}")
                } else {
                    format!("git blame exited {status}: {stderr}")
                }
            }
            BlameRefusal::Unparsable { detail } => {
                format!("git blame output could not be read: {detail}")
            }
        }
    }
}

/// A file's blame, and the basis it was taken against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBlame {
    pub path: String,
    pub lines: Vec<BlameLine>,
    /// The blob the blamed content is. Spans may only be read against this
    /// through [`BlobIdentity::comparable_with`].
    pub basis: BlobIdentity,
}

/// Blame `path` at `rev`, bounded.
///
/// `rev` names the commit whose content is blamed; passing `HEAD` blames the
/// committed content rather than the working tree, which is what makes the
/// result's [`BlobIdentity`] a stable `Blob` rather than a worktree read.
pub fn blame_file(repo: &Path, rev: &str, path: &str) -> Result<FileBlame, BlameRefusal> {
    blame_file_with_program(std::ffi::OsStr::new("git"), repo, rev, path)
}

/// [`blame_file`] with the program named, so a test can stand a script in for
/// it and drive the refusal paths without a repository.
#[doc(hidden)]
pub fn blame_file_with_program(
    program: &std::ffi::OsStr,
    repo: &Path,
    rev: &str,
    path: &str,
) -> Result<FileBlame, BlameRefusal> {
    let mut command = git_with_program(program, repo);
    command.args([
        "blame",
        "--line-porcelain",
        // Whitespace-only reformatting must not re-attribute a line. Without
        // this, a `cargo fmt` sweep becomes the prime suspect for every
        // regression in every file it touched.
        "-w",
        // Nor must moving code within a file. `-M` follows a line back to
        // where it was written instead of crediting whoever relocated it,
        // which is the other half of the same problem: an "extract this into a
        // helper" commit otherwise owns every line it relocated and outranks
        // whatever actually changed behaviour.
        //
        // `-C` — the same detection across files — is deliberately not passed.
        // It re-reads other blobs looking for origins and is markedly more
        // expensive, and the bound here is a wall clock that a large repository
        // would then start hitting. The consequence is stated rather than
        // hidden: a line moved *between* files is credited to the commit that
        // moved it, so a file-splitting commit can appear as a suspect.
        "-M",
        rev,
        // `--` terminates options, so a path beginning with a dash is a path.
        // No pathspec magic: `path` is a literal name, and `:(literal)` is not
        // used because the argument is already after `--`.
        "--",
        path,
    ]);
    let captured = match run_bounded(
        &mut command,
        Bounds {
            deadline: BLAME_DEADLINE,
            stdout_cap: BLAME_OUTPUT_CAP,
            stderr_cap: BLAME_STDERR_CAP,
        },
    ) {
        Ok(captured) => captured,
        Err(Failure::Deadline { .. }) => return Err(BlameRefusal::Deadline),
        Err(failure) => {
            return Err(BlameRefusal::GitUnavailable {
                reason: failure.to_string(),
            })
        }
    };

    // Order matters. A capped read can also carry a success status, because
    // the child wrote its prefix and exited fine; checking the status first
    // would accept the prefix as the whole answer.
    if captured.stdout_truncated {
        return Err(BlameRefusal::OutputCapped {
            cap: BLAME_OUTPUT_CAP,
        });
    }
    if !captured.status.success() {
        return Err(BlameRefusal::GitRefused {
            status: captured.status.to_string(),
            stderr: captured.stderr_trimmed(),
        });
    }

    let lines = parse_porcelain(&captured.stdout_lossy())?;
    Ok(FileBlame {
        path: path.to_string(),
        lines,
        // The caller supplies the basis: this function knows the rev it asked
        // for, not the blob id that rev resolved the path to. `Unknown` is the
        // honest answer here and forces the caller to establish one, rather
        // than inventing a basis that would compare equal to something.
        basis: BlobIdentity::Unknown,
    })
}

/// Parse `git blame --line-porcelain`.
///
/// The format is a header block per line: an object id, the line numbers, then
/// `key value` lines, then the content prefixed with a tab. Header keys repeat
/// only on the first line of each commit block, so author fields are carried
/// forward from the block header — which is the detail a naive parser gets
/// wrong, producing empty authors for every line after the first in a block.
fn parse_porcelain(stdout: &str) -> Result<Vec<BlameLine>, BlameRefusal> {
    let mut lines = Vec::new();
    // Commit metadata, remembered per object id, because porcelain states it
    // once per commit and then refers back to the id.
    let mut known: std::collections::HashMap<String, (String, String, i64)> =
        std::collections::HashMap::new();

    let mut current_oid: Option<String> = None;
    let mut current_line_no: Option<u32> = None;
    let mut author = String::new();
    let mut author_mail = String::new();
    let mut author_time: i64 = 0;

    for raw in stdout.lines() {
        if let Some(rest) = raw.strip_prefix('\t') {
            // Content line: closes the current header block.
            let _ = rest;
            let Some(oid) = current_oid.take() else {
                return Err(BlameRefusal::Unparsable {
                    detail: "a content line arrived before any commit header".to_string(),
                });
            };
            let Some(line_no) = current_line_no.take() else {
                return Err(BlameRefusal::Unparsable {
                    detail: format!("commit {oid} carried no result line number"),
                });
            };
            // A block that restated the author updates the memo; one that did
            // not reads it back.
            if !author.is_empty() || author_time != 0 {
                known.insert(
                    oid.clone(),
                    (author.clone(), author_mail.clone(), author_time),
                );
            }
            let (block_author, block_mail, block_time) = known
                .get(&oid)
                .cloned()
                .unwrap_or_else(|| (String::new(), String::new(), 0));
            lines.push(BlameLine {
                line_no,
                uncommitted: is_uncommitted_oid(&oid),
                commit: oid,
                author: block_author,
                author_mail: block_mail,
                author_time: block_time,
            });
            author.clear();
            author_mail.clear();
            author_time = 0;
            continue;
        }

        if let Some(value) = raw.strip_prefix("author ") {
            author = value.to_string();
        } else if let Some(value) = raw.strip_prefix("author-mail ") {
            author_mail = value.trim_matches(|c| c == '<' || c == '>').to_string();
        } else if let Some(value) = raw.strip_prefix("author-time ") {
            author_time = value.trim().parse().unwrap_or(0);
        } else if let Some((oid, rest)) = header_oid(raw) {
            // `<oid> <orig-line> <final-line> [<num-lines>]`
            let final_line = rest
                .split_whitespace()
                .nth(1)
                .and_then(|field| field.parse::<u32>().ok());
            let Some(final_line) = final_line else {
                return Err(BlameRefusal::Unparsable {
                    detail: format!("commit header for {oid} carried no final line number"),
                });
            };
            current_oid = Some(oid);
            current_line_no = Some(final_line);
        }
        // Any other key line is metadata this analysis does not read.
    }

    Ok(lines)
}

/// The object id at the head of a porcelain block, if this line is one.
///
/// A header is `<hex oid> <numbers…>`. Requiring hex and a following space is
/// what stops a *content* line — which porcelain prefixes with a tab, but
/// which a malformed stream might not — from being read as a new block and
/// hijacking every line after it.
fn header_oid(line: &str) -> Option<(String, &str)> {
    let (candidate, rest) = line.split_once(' ')?;
    let len = candidate.len();
    // sha1 is 40, sha256 is 64. Nothing else is an object id.
    if len != 40 && len != 64 {
        return None;
    }
    if !candidate.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    // The remainder must start with a line number, or this is not a header.
    rest.split_whitespace()
        .next()
        .and_then(|field| field.parse::<u32>().ok())?;
    Some((candidate.to_string(), rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(oid: &str, final_line: u32, author: &str, time: i64, content: &str) -> String {
        format!(
            "{oid} {final_line} {final_line} 1\nauthor {author}\nauthor-mail <{author}@example.com>\nauthor-time {time}\nauthor-tz +0000\nsummary s\nfilename f\n\t{content}\n"
        )
    }

    const SHA1: &str = "1234567890abcdef1234567890abcdef12345678";
    const SHA1_B: &str = "abcdef1234567890abcdef1234567890abcdef12";

    #[test]
    fn a_single_block_parses() {
        let parsed = parse_porcelain(&block(SHA1, 1, "ada", 100, "let x = 1;")).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].line_no, 1);
        assert_eq!(parsed[0].commit, SHA1);
        assert_eq!(parsed[0].author, "ada");
        assert_eq!(parsed[0].author_time, 100);
        assert!(!parsed[0].uncommitted);
    }

    /// The detail a naive parser loses. Porcelain states the author once per
    /// commit block; a second line from the same commit carries only the id.
    #[test]
    fn a_repeated_commit_keeps_its_author_from_the_block_header() {
        let mut stream = block(SHA1, 1, "ada", 100, "first");
        stream.push_str(&format!("{SHA1} 2 2 1\nfilename f\n\tsecond\n"));
        let parsed = parse_porcelain(&stream).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[1].author, "ada",
            "the second line of a commit block carries no author line of its \
             own; reading it as empty loses attribution for every line after \
             the first"
        );
        assert_eq!(parsed[1].author_time, 100);
    }

    #[test]
    fn two_commits_keep_their_own_authors() {
        let mut stream = block(SHA1, 1, "ada", 100, "first");
        stream.push_str(&block(SHA1_B, 2, "grace", 200, "second"));
        let parsed = parse_porcelain(&stream).unwrap();
        assert_eq!(parsed[0].author, "ada");
        assert_eq!(parsed[1].author, "grace");
        assert_eq!(parsed[1].author_time, 200);
    }

    #[test]
    fn an_all_zero_oid_is_uncommitted_at_both_object_lengths() {
        let sha1_zero = "0".repeat(40);
        let sha256_zero = "0".repeat(64);
        for zero in [sha1_zero, sha256_zero] {
            let parsed = parse_porcelain(&block(&zero, 1, "you", 0, "wip")).unwrap();
            assert!(
                parsed[0].uncommitted,
                "an all-zero id is a worktree line, at every object length"
            );
        }
    }

    #[test]
    fn a_sha256_repository_parses() {
        let oid = "a".repeat(64);
        let parsed = parse_porcelain(&block(&oid, 7, "ada", 1, "x")).unwrap();
        assert_eq!(parsed[0].commit, oid);
        assert_eq!(parsed[0].line_no, 7);
    }

    /// Content that looks like a header must not be read as one. Blame output
    /// contains arbitrary source, and source contains hex strings.
    #[test]
    fn source_text_resembling_a_header_does_not_open_a_block() {
        let content = format!("{SHA1} 1 1 1");
        let parsed = parse_porcelain(&block(SHA1_B, 1, "ada", 5, &content)).unwrap();
        assert_eq!(
            parsed.len(),
            1,
            "the tab-prefixed content line is content, whatever it spells"
        );
        assert_eq!(parsed[0].commit, SHA1_B);
    }

    #[test]
    fn a_content_line_before_any_header_is_refused() {
        let error = parse_porcelain("\torphan content\n").unwrap_err();
        assert!(matches!(error, BlameRefusal::Unparsable { .. }));
    }

    #[test]
    fn a_header_without_a_line_number_is_refused() {
        let error = parse_porcelain(&format!("{SHA1} notanumber\n\tx\n")).unwrap_err();
        assert!(matches!(error, BlameRefusal::Unparsable { .. }));
    }

    #[test]
    fn an_empty_stream_is_an_empty_blame_not_an_error() {
        assert!(parse_porcelain("").unwrap().is_empty());
    }

    #[test]
    fn a_non_hex_oid_length_match_is_not_a_header() {
        // 40 characters, but not hex.
        let fake = "z".repeat(40);
        assert!(header_oid(&format!("{fake} 1 1 1")).is_none());
    }

    #[test]
    fn a_short_hex_run_is_not_a_header() {
        assert!(header_oid("abc123 1 1 1").is_none());
    }
}
