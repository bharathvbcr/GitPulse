//! Transcript ingest across the worktrees of one repository: what a family
//! pass must leave exactly as the per-worktree passes did, and what it must no
//! longer do (read one corpus file once per worktree).

use super::*;

fn with_transcript_root<T>(root: &std::path::Path, f: impl FnOnce() -> T) -> T {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", root);
    f()
}

fn edit_line(cwd: &str, session: &str, second: u32, file: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": session,
        "timestamp": format!("2026-09-01T12:00:{second:02}.000Z"),
        "version": "2.1.241",
        "cwd": cwd,
        "message": { "content": [
            { "type": "tool_use", "name": "Edit", "input": { "file_path": file } }
        ]}
    })
    .to_string()
}

fn command_line(cwd: &str, session: &str, second: u32, command: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": session,
        "timestamp": format!("2026-09-01T12:00:{second:02}.000Z"),
        "version": "2.1.241",
        "cwd": cwd,
        "message": { "content": [
            { "type": "tool_use", "name": "Bash", "input": { "command": command } }
        ]}
    })
    .to_string()
}

fn write_lines(path: &std::path::Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn append_text(path: &std::path::Path, text: &str) {
    let mut file = std::fs::File::options().append(true).open(path).unwrap();
    std::io::Write::write_all(&mut file, text.as_bytes()).unwrap();
}

/// Three worktrees (plain directories; the ledger lives under the first) and a
/// corpus of four transcripts.
struct Fixture {
    _root: tempfile::TempDir,
    _corpus: tempfile::TempDir,
    root: String,
    worktrees: [String; 3],
    corpus: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let corpus = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let worktrees = ["w1", "w2", "w3"].map(|name| {
            let path = base.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path.to_string_lossy().into_owned()
        });
        let root_text = base.to_string_lossy().into_owned();
        Self {
            corpus: corpus.path().canonicalize().unwrap(),
            _root: root,
            _corpus: corpus,
            root: root_text,
            worktrees,
        }
    }

    fn ledger(&self) -> &str {
        &self.worktrees[0]
    }

    fn file(&self, name: &str) -> std::path::PathBuf {
        self.corpus.join("slug").join(name)
    }

    fn at(&self, index: usize, file: &str) -> String {
        format!("{}/{file}", self.worktrees[index])
    }

    /// The first state of the corpus.
    fn write_initial_corpus(&self) {
        write_lines(
            &self.file("S1.jsonl"),
            &[
                edit_line(&self.worktrees[0], "S1", 1, &self.at(0, "a.rs")),
                edit_line(&self.worktrees[1], "S1", 2, &self.at(1, "a.rs")),
                edit_line(&self.worktrees[2], "S1", 3, &self.at(2, "a.rs")),
            ],
        );
        write_lines(
            &self.file("S2.jsonl"),
            &[
                edit_line(&self.worktrees[1], "S2", 4, &self.at(1, "b.rs")),
                edit_line(&self.worktrees[1], "S2", 5, &self.at(1, "b2.rs")),
                edit_line(&self.worktrees[0], "S2", 6, &self.at(0, "b.rs")),
            ],
        );
        write_lines(
            &self.file("S3.jsonl"),
            &[edit_line("/elsewhere", "S3", 7, "/elsewhere/z.rs")],
        );
    }

    /// An append, a rotation (a shorter rewrite), a new file, and a fragment
    /// still being written.
    fn grow_corpus(&self) {
        append_text(
            &self.file("S1.jsonl"),
            &(edit_line(&self.worktrees[0], "S1", 8, &self.at(0, "c.rs"))
                + "\n"
                + &edit_line(&self.worktrees[2], "S1", 9, &self.at(2, "c.rs"))
                + "\n"),
        );
        write_lines(
            &self.file("S2.jsonl"),
            &[edit_line(&self.worktrees[1], "S2", 10, &self.at(1, "r.rs"))],
        );
        write_lines(
            &self.file("S4.jsonl"),
            &[
                edit_line(&self.worktrees[0], "S4", 11, &self.at(0, "d.rs")),
                edit_line(&self.worktrees[1], "S4", 12, &self.at(1, "d.rs")),
                edit_line(&self.worktrees[2], "S4", 13, &self.at(2, "d.rs")),
                command_line(&self.worktrees[2], "S4", 14, "cargo test"),
            ],
        );
        append_text(&self.file("S4.jsonl"), "{\"type\":\"assistant\",\"sess");
    }

    fn corpus_bytes(&self) -> u64 {
        ["S1.jsonl", "S2.jsonl", "S3.jsonl", "S4.jsonl"]
            .iter()
            .filter_map(|name| std::fs::metadata(self.file(name)).ok())
            .map(|meta| meta.len())
            .sum()
    }

    /// Everything the ledger now says, with machine-specific paths removed:
    /// the attributed events, and for each worktree how many whole lines of
    /// each transcript its cursor has consumed.
    fn digest(&self) -> String {
        let anchor = self.ledger();
        let scrub = |text: &str| text.replace(&self.root, "$ROOT");
        let mut events: Vec<String> = ledger::tail(anchor, 0, 1000)
            .unwrap()
            .into_iter()
            .filter(|event| transcript::ACTIONS.contains(&event.action.as_str()))
            .map(|event| {
                format!(
                    "{} {} {} {}",
                    scrub(event.worktree_path.as_deref().unwrap_or("<anchor>")),
                    event.action,
                    scrub(event.object.as_deref().unwrap_or("")),
                    event.session_id.unwrap_or_default()
                )
            })
            .collect();
        events.sort();
        let mut lines = events;
        for worktree in &self.worktrees {
            let progress = ledger::transcript_progress(anchor, worktree).unwrap();
            for name in ["S1.jsonl", "S2.jsonl", "S3.jsonl", "S4.jsonl"] {
                let path = self.file(name);
                let offset = progress.offsets.get(&ledger::transcript_key(&path));
                let consumed = offset.map(|offset| {
                    let bytes = std::fs::read(&path).unwrap();
                    let end = usize::try_from(*offset).unwrap().min(bytes.len());
                    bytes[..end].iter().filter(|b| **b == b'\n').count()
                });
                lines.push(format!("{} {name} {consumed:?}", scrub(worktree)));
            }
        }
        lines.join("\n")
    }
}

/// One catch-up per worktree, in turn: how every pass before the family pass
/// worked, and so the oracle the family pass is held to.
fn one_pass_per_worktree(fixture: &Fixture, worktrees: &[usize]) {
    for index in worktrees {
        ingest_transcripts_into(fixture.ledger(), &fixture.worktrees[*index]);
    }
}

/// What the per-worktree passes left, recorded from the code as it was before
/// the family pass existed: an append, a rotation, a new transcript, a fragment
/// still being written, and a worktree (`w3`) reading the corpus for the first
/// time, with `w1` and `w2` already holding cursors.
const PER_WORKTREE_DIGEST: &str = "\
$ROOT/w2 session.edit $ROOT/w2/a.rs S1
$ROOT/w2 session.edit $ROOT/w2/b.rs S2
$ROOT/w2 session.edit $ROOT/w2/b2.rs S2
$ROOT/w2 session.edit $ROOT/w2/d.rs S4
$ROOT/w2 session.edit $ROOT/w2/r.rs S2
$ROOT/w3 session.command cargo test S4
$ROOT/w3 session.edit $ROOT/w3/a.rs S1
$ROOT/w3 session.edit $ROOT/w3/c.rs S1
$ROOT/w3 session.edit $ROOT/w3/d.rs S4
<anchor> session.edit $ROOT/w1/a.rs S1
<anchor> session.edit $ROOT/w1/b.rs S2
<anchor> session.edit $ROOT/w1/c.rs S1
<anchor> session.edit $ROOT/w1/d.rs S4
$ROOT/w1 S1.jsonl Some(5)
$ROOT/w1 S2.jsonl Some(1)
$ROOT/w1 S3.jsonl Some(1)
$ROOT/w1 S4.jsonl Some(4)
$ROOT/w2 S1.jsonl Some(5)
$ROOT/w2 S2.jsonl Some(1)
$ROOT/w2 S3.jsonl Some(1)
$ROOT/w2 S4.jsonl Some(4)
$ROOT/w3 S1.jsonl Some(5)
$ROOT/w3 S2.jsonl Some(1)
$ROOT/w3 S3.jsonl Some(1)
$ROOT/w3 S4.jsonl Some(4)";

/// A transcript that comes back shorter than what a worktree already read was
/// rewritten, not appended to: its cursor is reset and the file is read again
/// ("a duplicate row is recoverable, a lost one is not"). A characterization
/// of the behaviour the family pass must keep.
#[test]
fn a_rotated_transcript_resets_its_cursor() {
    let fixture = Fixture::new();
    let anchor = fixture.ledger().to_string();
    let worktree = fixture.worktrees[0].clone();
    let file = fixture.file("R1.jsonl");
    with_transcript_root(&fixture.corpus, || {
        write_lines(
            &file,
            &[
                edit_line(&worktree, "R1", 1, &fixture.at(0, "one.rs")),
                edit_line(&worktree, "R1", 2, &fixture.at(0, "two.rs")),
            ],
        );
        ingest_transcripts_into(&anchor, &worktree);
        let key = ledger::transcript_key(&file);
        let before = ledger::transcript_progress(&anchor, &worktree).unwrap();
        assert_eq!(
            before.offsets.get(&key).copied(),
            Some(std::fs::metadata(&file).unwrap().len())
        );

        write_lines(
            &file,
            &[edit_line(&worktree, "R1", 3, &fixture.at(0, "new.rs"))],
        );
        let rewritten = std::fs::metadata(&file).unwrap().len();
        assert!(rewritten < before.offsets[&key]);
        let again = ingest_transcripts_into(&anchor, &worktree);
        assert_eq!(again.recorded, 1, "the rewritten transcript was not re-read");
        let after = ledger::transcript_progress(&anchor, &worktree).unwrap();
        assert_eq!(
            after.offsets.get(&key).copied(),
            Some(rewritten),
            "the cursor was not reset to the rewritten file"
        );
    });
}

#[test]
fn per_worktree_passes_leave_this_digest() {
    let fixture = Fixture::new();
    with_transcript_root(&fixture.corpus, || {
        fixture.write_initial_corpus();
        one_pass_per_worktree(&fixture, &[0, 1]);
        fixture.grow_corpus();
        one_pass_per_worktree(&fixture, &[0, 1, 2]);
    });
    let digest = fixture.digest();
    println!("DIGEST-BEGIN\n{digest}\nDIGEST-END");
    assert_eq!(digest, PER_WORKTREE_DIGEST);
}
