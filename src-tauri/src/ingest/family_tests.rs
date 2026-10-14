//! Transcript ingest across the worktrees of one repository: what a family
//! pass must leave exactly as the per-worktree passes did, and what it must no
//! longer do (read one corpus file once per worktree).

use super::*;

fn with_transcript_root<T>(root: &std::path::Path, f: impl FnOnce() -> T) -> T {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", root);
    f()
}

fn past(second: u32) -> String {
    format!("2026-09-01T12:00:{second:02}.000Z")
}

/// The time a call made right now carries. A test that binds a task and then
/// expects its calls to carry it stamps them with this, after the bind: a call
/// stamped before the bind happened before it, and carries no task.
fn now_stamp() -> String {
    ledger::ids::iso8601_utc(ledger::ids::now_millis())
}

fn edit_line_at(cwd: &str, session: &str, timestamp: &str, file: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": session,
        "timestamp": timestamp,
        "version": "2.1.241",
        "cwd": cwd,
        "message": { "content": [
            { "type": "tool_use", "name": "Edit", "input": { "file_path": file } }
        ]}
    })
    .to_string()
}

fn command_line_at(cwd: &str, session: &str, timestamp: &str, command: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": session,
        "timestamp": timestamp,
        "version": "2.1.241",
        "cwd": cwd,
        "message": { "content": [
            { "type": "tool_use", "name": "Bash", "input": { "command": command } }
        ]}
    })
    .to_string()
}

fn edit_line(cwd: &str, session: &str, second: u32, file: &str) -> String {
    edit_line_at(cwd, session, &past(second), file)
}

fn command_line(cwd: &str, session: &str, second: u32, command: &str) -> String {
    command_line_at(cwd, session, &past(second), command)
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

/// One pass serving the listed worktrees together.
fn one_family_pass(fixture: &Fixture, worktrees: &[usize]) -> CatchUp {
    let trigger = &fixture.worktrees[worktrees[0]];
    let siblings: Vec<String> = worktrees[1..]
        .iter()
        .map(|index| fixture.worktrees[*index].clone())
        .collect();
    pass::ingest_family_until(fixture.ledger(), trigger, &siblings, &mut |_| false)
}

fn bytes_read() -> u64 {
    pass::BYTES_READ.with(std::cell::Cell::get)
}

/// The family pass leaves exactly what the per-worktree passes left: same
/// events, same cursors, with an append, a rotation, a new transcript, an
/// unfinished line, and a worktree with no cursor reading the corpus from zero.
#[test]
fn a_family_pass_leaves_the_digest_the_per_worktree_passes_left() {
    // Cursors made by earlier per-worktree passes, then one family pass.
    let fixture = Fixture::new();
    with_transcript_root(&fixture.corpus, || {
        fixture.write_initial_corpus();
        one_pass_per_worktree(&fixture, &[0, 1]);
        fixture.grow_corpus();
        one_family_pass(&fixture, &[0, 1, 2]);
    });
    assert_eq!(fixture.digest(), PER_WORKTREE_DIGEST);

    // The same from the very first read: family passes all the way.
    let fixture = Fixture::new();
    with_transcript_root(&fixture.corpus, || {
        fixture.write_initial_corpus();
        one_family_pass(&fixture, &[0, 1]);
        fixture.grow_corpus();
        one_family_pass(&fixture, &[0, 1, 2]);
        // And idempotent.
        let again = one_family_pass(&fixture, &[2, 0, 1]);
        assert_eq!(again.recorded, 0);
        // Only the transcript with an unfinished last line is owed anything.
        assert!(again.transcripts <= 1, "{again:?}");
    });
    assert_eq!(fixture.digest(), PER_WORKTREE_DIGEST);
}

/// Eighteen worktrees used to read one corpus eighteen times. One pass reads
/// each transcript once, however many worktrees it serves.
#[test]
fn a_family_pass_reads_each_transcript_once_for_any_number_of_worktrees() {
    let fixture = Fixture::new();
    with_transcript_root(&fixture.corpus, || {
        fixture.write_initial_corpus();
        fixture.grow_corpus();
        // The unfinished line of S4 is never read as a line, but it is in the
        // file; the reader stops at the last newline, so count what is whole.
        let whole: u64 = ["S1.jsonl", "S2.jsonl", "S3.jsonl", "S4.jsonl"]
            .iter()
            .map(|name| {
                let bytes = std::fs::read(fixture.file(name)).unwrap();
                bytes
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map_or(0, |i| i as u64 + 1)
            })
            .sum();
        assert!(whole <= fixture.corpus_bytes());

        let before = bytes_read();
        one_family_pass(&fixture, &[0, 1, 2]);
        let family = bytes_read() - before;
        // The reader also pulls the unfinished tail, so allow for it: what must
        // not happen is a second read of any file.
        assert!(
            family <= fixture.corpus_bytes(),
            "a family of three read {family} bytes of a {} byte corpus",
            fixture.corpus_bytes()
        );
        assert!(family >= whole, "{family} < {whole}");

        // The per-worktree passes this replaces: the same corpus once each,
        // into a ledger that has read none of it.
        let other = tempfile::tempdir().unwrap();
        let other = other.path().to_str().unwrap();
        let before = bytes_read();
        for worktree in &fixture.worktrees {
            ingest_transcripts_into(other, worktree);
        }
        let separate = bytes_read() - before;
        assert!(
            separate >= 3 * whole,
            "three separate passes read {separate} bytes; one family pass read {family}"
        );
    });
}

/// A line is deserialized once however many worktrees are offered its calls.
#[test]
fn a_family_pass_parses_each_line_once() {
    let fixture = Fixture::new();
    with_transcript_root(&fixture.corpus, || {
        // Twelve prose-only assistant lines: the shape that used to be parsed
        // twice, once to look for calls and once to learn it was readable.
        let prose = serde_json::json!({
            "type": "assistant", "sessionId": "P", "timestamp": "2026-09-01T12:00:00.000Z",
            "cwd": fixture.worktrees[0],
            "message": { "content": [{ "type": "text", "text": "done" }] }
        })
        .to_string();
        write_lines(&fixture.file("P.jsonl"), &vec![prose; 12]);
        let before = transcript::PARSES.with(std::cell::Cell::get);
        one_family_pass(&fixture, &[0, 1, 2]);
        let parses = transcript::PARSES.with(std::cell::Cell::get) - before;
        assert_eq!(parses, 12, "twelve lines, three worktrees, {parses} parses");
    });
}

/// A pass the deadline cuts reports it, with how much it had read of how much
/// it listed, and the next pass delivers what it never reached.
#[test]
fn a_cut_family_pass_carries_both_numbers_and_loses_nothing() {
    let fixture = Fixture::new();
    with_transcript_root(&fixture.corpus, || {
        fixture.write_initial_corpus();
        // Cut once the first transcript has been committed.
        let trigger = fixture.worktrees[0].clone();
        let siblings = vec![fixture.worktrees[1].clone(), fixture.worktrees[2].clone()];
        let cut = pass::ingest_family_until(fixture.ledger(), &trigger, &siblings, &mut |out| {
            out.recorded >= 1
        });
        assert!(cut.truncated, "{cut:?}");
        assert_eq!(
            cut.transcripts_listed, 3,
            "the walk listed the whole corpus"
        );
        assert!(cut.transcripts < cut.transcripts_listed, "{cut:?}");

        let merged = CatchUp::default().with_transcripts(&CatchUp {
            error: String::new(),
            ..cut.clone()
        });
        assert!(merged.truncated);
        let both = format!(
            "{} read, {} listed",
            cut.transcripts, cut.transcripts_listed
        );
        assert!(
            merged.error.contains(&both),
            "a cut pass must say how far it got ({both}): {}",
            merged.error
        );

        one_family_pass(&fixture, &[0, 1, 2]);
    });
    // Nothing owed was lost: the whole initial corpus is accounted for.
    let digest = fixture.digest();
    for expected in [
        "$ROOT/w2 session.edit $ROOT/w2/a.rs S1",
        "$ROOT/w3 session.edit $ROOT/w3/a.rs S1",
        "<anchor> session.edit $ROOT/w1/b.rs S2",
    ] {
        assert!(
            digest.contains(expected),
            "{expected} missing from\n{digest}"
        );
    }
}

/// A line longer than the reader keeps is counted, not buffered, and does not
/// stop the lines around it from being read.
#[test]
fn an_oversize_line_is_counted_and_the_pass_goes_on() {
    let fixture = Fixture::new();
    with_transcript_root(&fixture.corpus, || {
        let first = edit_line(&fixture.worktrees[0], "O1", 1, &fixture.at(0, "a.rs"));
        let last = edit_line(&fixture.worktrees[0], "O1", 3, &fixture.at(0, "z.rs"));
        let huge = "x".repeat(lines::MAX_LINE + 10);
        write_lines(&fixture.file("O1.jsonl"), &[first, huge, last]);
        let out = one_family_pass(&fixture, &[0]);
        assert_eq!(out.recorded, 2, "{out:?}");
        assert_eq!(out.skipped_lines, 1, "the oversize line must be reported");
        let size = std::fs::metadata(fixture.file("O1.jsonl")).unwrap().len();
        let progress =
            ledger::transcript_progress(fixture.ledger(), &fixture.worktrees[0]).unwrap();
        assert_eq!(
            progress
                .offsets
                .get(&ledger::transcript_key(&fixture.file("O1.jsonl"))),
            Some(&size),
            "the cursor must pass the oversize line"
        );
    });
}

/// With another process holding the ledger's write lock, the old pass sat in
/// SQLite's five-second busy wait with no deadline in sight. A budgeted
/// catch-up returns inside its budget and says it was cut short.
#[test]
fn a_catch_up_stalled_on_a_locked_ledger_returns_truncated_within_budget() {
    let fixture = Fixture::new();
    let repo = fixture.ledger().to_string();
    with_transcript_root(&fixture.corpus, || {
        fixture.write_initial_corpus();
        // Open (and so create) the ledger before the lock is taken.
        ledger::tail(&repo, 0, 1).expect("ledger opens");
        let locker = rusqlite::Connection::open(ledger::ledger_path(&repo)).unwrap();
        locker.execute_batch("BEGIN IMMEDIATE").unwrap();

        let budget = std::time::Duration::from_millis(300);
        let mut fastest = std::time::Duration::MAX;
        let mut last = CatchUp::default();
        for _ in 0..3 {
            let started = std::time::Instant::now();
            last = catch_up_into_bounded(&repo, &repo, Some(budget));
            fastest = fastest.min(started.elapsed());
        }
        locker.execute_batch("COMMIT").unwrap();

        assert!(last.truncated, "a stalled pass reported complete: {last:?}");
        assert!(last.error.contains("truncated"), "{last:?}");
        assert!(
            fastest < budget + std::time::Duration::from_millis(250),
            "fastest of three: {fastest:?} for a {budget:?} budget"
        );
    });
}

/// The ledger paging that draws a baseline checks the clock between pages.
#[test]
fn paging_the_ledger_for_a_baseline_stops_when_the_clock_says_so() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    for i in 0..2500 {
        ledger::append(Draft {
            repo_path: repo.to_string(),
            action: "session.edit".into(),
            object: Some(format!("{repo}/f{i}")),
            actor_kind: Some(ActorKind::Agent),
            outcome: Some(Outcome::Ok),
            ..Default::default()
        })
        .unwrap();
    }
    // A clock that advances a second per reading, against a three second budget.
    let base = std::time::Instant::now();
    let ticks = std::sync::atomic::AtomicU64::new(0);
    let clock: flight::Clock = std::sync::Arc::new(move || {
        base + std::time::Duration::from_secs(
            ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        )
    });
    let budget = Budget::within_on(std::time::Duration::from_secs(3), clock);
    let cut = legacy_watermarks(repo, &[repo], &mut || budget.expired()).unwrap();
    assert!(
        cut.is_none(),
        "a budget that ran out mid-scan drew a baseline"
    );

    let whole = legacy_watermarks(repo, &[repo], &mut || false)
        .unwrap()
        .expect("an unbounded scan finishes");
    assert_eq!(whole.len(), 1);
    assert!(!whole[0].is_empty());
}

/// A pass that finished a moment ago answers for the next one.
#[test]
fn a_second_coalesced_catch_up_is_answered_not_run() {
    let fixture = Fixture::new();
    let repo = fixture.ledger().to_string();
    with_transcript_root(&fixture.corpus, || {
        // The fixture directories are not Git checkouts, so the repository
        // cannot be resolved: the answer is an error, and it is the same one.
        let first = catch_up_coalesced(&repo);
        assert!(!first.error.is_empty());
        assert!(!first.truncated);
        let second = catch_up_coalesced(&repo);
        assert_eq!(first.error, second.error);
    });
}

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
        assert_eq!(
            again.recorded, 1,
            "the rewritten transcript was not re-read"
        );
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

/// A repository whose worktrees live under it, as isolation worktrees do:
/// `base/.claude/worktrees/{w1,w2,w3}`, with the ledger anchored at `base`.
struct Nested {
    _root: tempfile::TempDir,
    _corpus: tempfile::TempDir,
    base: String,
    worktrees: [String; 3],
    corpus: std::path::PathBuf,
}

impl Nested {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let corpus = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let worktrees = ["w1", "w2", "w3"].map(|name| {
            let path = base.join(".claude/worktrees").join(name);
            std::fs::create_dir_all(path.join("src")).unwrap();
            path.to_string_lossy().into_owned()
        });
        Self {
            corpus: corpus.path().canonicalize().unwrap(),
            _root: root,
            _corpus: corpus,
            base: base.to_string_lossy().into_owned(),
            worktrees,
        }
    }

    fn file(&self, name: &str) -> std::path::PathBuf {
        self.corpus.join("slug").join(name)
    }

    /// One pass for `trigger` serving `siblings`.
    fn pass(&self, trigger: &str, siblings: &[&str]) -> CatchUp {
        let siblings: Vec<String> = siblings.iter().map(|s| (*s).to_string()).collect();
        pass::ingest_family_until(&self.base, trigger, &siblings, &mut |_| false)
    }

    /// Every transcript row now in the ledger: where it was recorded, what
    /// it was about, and the task it carries.
    fn rows(&self) -> Vec<(Option<String>, String, Option<String>)> {
        let mut rows: Vec<_> = ledger::tail(&self.base, 0, 5000)
            .unwrap()
            .into_iter()
            .filter(|event| transcript::ACTIONS.contains(&event.action.as_str()))
            .map(|event| {
                (
                    event.worktree_path,
                    event.object.unwrap_or_default(),
                    event.task_id,
                )
            })
            .collect();
        rows.sort();
        rows
    }
}

#[test]
fn an_edit_in_a_nested_worktree_is_one_row_under_that_worktree() {
    let nested = Nested::new();
    let w = &nested.worktrees[0];
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("N1.jsonl"),
            &[
                edit_line(w, "N1", 1, &format!("{w}/a.rs")),
                command_line(w, "N1", 2, "cargo test"),
            ],
        );
        nested.pass(&nested.base, &[w]);
    });
    let rows = nested.rows();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (path, _, _) in &rows {
        assert_eq!(path.as_deref(), Some(w.as_str()), "{rows:?}");
    }
}

#[test]
fn a_departed_worktree_keeps_its_own_path() {
    let nested = Nested::new();
    let w = nested.worktrees[0].clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("D1.jsonl"),
            &[
                edit_line(&w, "D1", 1, &format!("{w}/a.rs")),
                edit_line(&w, "D1", 6, &format!("{w}/a2.rs")),
            ],
        );
        nested.pass(&nested.base, &[&w]);

        std::fs::remove_dir_all(&w).unwrap();
        append_text(
            &nested.file("D1.jsonl"),
            &(edit_line(&w, "D1", 2, &format!("{w}/b.rs"))
                + "\n"
                + &command_line(&w, "D1", 3, "cargo build")
                + "\n"
                + &edit_line(&nested.base, "D1", 4, &format!("{}/main.rs", nested.base))
                + "\n"),
        );
        nested.pass(&nested.base, &[]);
        let rows = nested.rows();
        let departed: Vec<_> = rows
            .iter()
            .filter(|(_, object, _)| object.ends_with("b.rs") || object == "cargo build")
            .collect();
        assert_eq!(departed.len(), 2, "{rows:?}");
        for (path, _, task) in departed {
            assert_eq!(path.as_deref(), Some(w.as_str()), "{rows:?}");
            assert_eq!(task, &None, "a departed worktree never carries main's task");
        }
        let main: Vec<_> = rows
            .iter()
            .filter(|(_, object, _)| object.ends_with("main.rs"))
            .collect();
        assert_eq!(main.len(), 1);
        assert_eq!(main[0].0, None);

        // A rotation: the file is rewritten shorter and replayed from zero.
        write_lines(
            &nested.file("D1.jsonl"),
            &[edit_line(&w, "D1", 5, &format!("{w}/c.rs"))],
        );
        nested.pass(&nested.base, &[]);
    });
    let rows = nested.rows();
    assert!(
        !rows
            .iter()
            .any(|(path, object, _)| path.is_none() && object.starts_with(&w)),
        "no main row for a path under the departed worktree: {rows:?}"
    );
    assert!(
        rows.iter().any(
            |(path, object, _)| path.as_deref() == Some(w.as_str()) && object.ends_with("c.rs")
        ),
        "{rows:?}"
    );
}

#[test]
fn a_departed_worktree_ingested_alone_is_not_recorded_again() {
    let nested = Nested::new();
    let w = nested.worktrees[0].clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("A1.jsonl"),
            &[edit_line(&w, "A1", 1, &format!("{w}/a.rs"))],
        );
        // The worktree ran a pass of its own while it lived.
        ingest_transcripts_into(&nested.base, &w);
        assert_eq!(nested.rows().len(), 1);

        std::fs::remove_dir_all(&w).unwrap();
        let out = nested.pass(&nested.base, &[]);
        assert_eq!(out.recorded, 0, "{out:?}");
    });
    assert_eq!(nested.rows().len(), 1);
}

#[test]
fn no_double_recording_across_a_three_worktree_family() {
    let nested = Nested::new();
    let [w1, w2, w3] = nested.worktrees.clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("T1.jsonl"),
            &[
                edit_line(&nested.base, "T1", 1, &format!("{}/m.rs", nested.base)),
                edit_line(&w1, "T1", 2, &format!("{w1}/a.rs")),
                edit_line(&w2, "T1", 3, &format!("{w2}/a.rs")),
                edit_line(&w3, "T1", 4, &format!("{w3}/a.rs")),
                command_line(&format!("{w2}/src"), "T1", 5, "ls"),
            ],
        );
        let first = nested.pass(&w2, &[&nested.base, &w1, &w3]);
        assert_eq!(first.recorded, 5, "{first:?}");
        let rows = nested.rows();
        let held = |w: &str| rows.iter().filter(|r| r.0.as_deref() == Some(w)).count();
        assert_eq!(held(&w1), 1, "{rows:?}");
        assert_eq!(held(&w2), 2, "{rows:?}");
        assert_eq!(held(&w3), 1, "{rows:?}");
        assert_eq!(rows.iter().filter(|r| r.0.is_none()).count(), 1, "{rows:?}");

        let again = nested.pass(&nested.base, &[&w1, &w2, &w3]);
        assert_eq!(again.recorded, 0, "{again:?}");
    });
}

/// Transcript content is untrusted: a hostile cwd never panics and never
/// invents a departed worktree; the call keeps the live root's attribution.
#[test]
fn a_hostile_cwd_is_attributed_without_panic_or_a_departed_root() {
    let nested = Nested::new();
    let base = nested.base.clone();
    let hostile = [
        format!("{base}/../../etc"),
        format!("{base}/a\0b"),
        format!("{base}/./x"),
        format!("{base}//x"),
        format!("{base}/{}", "d".repeat(64 * 1024)),
    ];
    with_transcript_root(&nested.corpus, || {
        let lines: Vec<String> = hostile
            .iter()
            .enumerate()
            .map(|(i, cwd)| command_line(cwd, "H1", u32::try_from(i).unwrap() + 1, "ls"))
            .collect();
        write_lines(&nested.file("H1.jsonl"), &lines);
        let out = nested.pass(&nested.base, &[]);
        assert_eq!(out.error, "", "{out:?}");
        assert_eq!(out.recorded, 5, "{out:?}");
    });
    let rows = nested.rows();
    assert!(rows.iter().all(|row| row.0.is_none()), "{rows:?}");
}

/// The same through Git: a real linked worktree under the repository, then
/// removed. Its rows keep its path, and only its own task rides on them.
#[test]
fn a_removed_git_worktree_keeps_its_path_and_never_takes_the_main_task() {
    use crate::test_support::{git_in, trust_repo};
    let main = tempfile::tempdir().unwrap();
    let base = main.path().canonicalize().unwrap();
    git_in(&base, &["init", "-b", "main"]);
    std::fs::write(base.join("seed.txt"), "seed").unwrap();
    git_in(&base, &["add", "seed.txt"]);
    git_in(&base, &["commit", "-m", "seed"]);
    let linked = base.join(".claude/worktrees/w");
    git_in(
        &base,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    trust_repo(&linked);
    let linked = linked.canonicalize().unwrap();
    let w = linked.to_string_lossy().into_owned();
    let anchor = base.to_string_lossy().into_owned();
    ledger::bindings::bind(&anchor, &anchor, "T-MAIN").unwrap();
    ledger::bindings::bind(&anchor, &w, "T-W").unwrap();

    let corpus = tempfile::tempdir().unwrap();
    let corpus_dir = corpus.path().canonicalize().unwrap();
    let file = corpus_dir.join("slug/G1.jsonl");
    let rows = || -> Vec<(Option<String>, String, Option<String>)> {
        let mut rows: Vec<_> = ledger::tail(&anchor, 0, 5000)
            .unwrap()
            .into_iter()
            .filter(|event| transcript::ACTIONS.contains(&event.action.as_str()))
            .map(|e| (e.worktree_path, e.object.unwrap_or_default(), e.task_id))
            .collect();
        rows.sort();
        rows
    };
    with_transcript_root(&corpus_dir, || {
        write_lines(
            &file,
            &[edit_line_at(&w, "G1", &now_stamp(), &format!("{w}/a.rs"))],
        );
        catch_up_into_bounded(&anchor, &anchor, None);
        assert_eq!(
            rows(),
            vec![(
                Some(w.clone()),
                format!("{w}/a.rs"),
                Some("T-W".to_string())
            )]
        );

        git_in(&base, &["worktree", "remove", "--force", &w]);
        append_text(
            &file,
            &(edit_line(&w, "G1", 2, &format!("{w}/b.rs"))
                + "\n"
                + &edit_line_at(&anchor, "G1", &now_stamp(), &format!("{anchor}/m.rs"))
                + "\n"),
        );
        catch_up_into_bounded(&anchor, &anchor, None);
        let after = rows();
        assert_eq!(after.len(), 3, "{after:?}");
        assert!(
            after.contains(&(Some(w.clone()), format!("{w}/b.rs"), None)),
            "{after:?}"
        );
        assert!(
            after.contains(&(None, format!("{anchor}/m.rs"), Some("T-MAIN".into()))),
            "{after:?}"
        );
        // A third catch-up finds nothing new.
        let third = catch_up_into_bounded(&anchor, &anchor, None);
        assert_eq!(third.recorded, 0, "{third:?}");
        assert_eq!(rows(), after);
    });
}

/// Hosting a removed worktree's rows moves that worktree's own cursor, and only
/// as far as the pass read: nothing it was not shown is skipped for it later.
#[test]
fn hosting_a_removed_worktrees_rows_writes_its_cursor() {
    let nested = Nested::new();
    let w = nested.worktrees[0].clone();
    with_transcript_root(&nested.corpus, || {
        let line = edit_line(&w, "E1", 1, &format!("{w}/a.rs"));
        write_lines(&nested.file("E1.jsonl"), std::slice::from_ref(&line));
        std::fs::remove_dir_all(&w).unwrap();
        let out = nested.pass(&nested.base, &[]);
        assert_eq!(out.recorded, 1, "{out:?}");
        let key = ledger::transcript_key(&nested.file("E1.jsonl"));
        let progress = ledger::transcript_progress(&nested.base, &w).unwrap();
        assert_eq!(
            progress.offsets.get(&key),
            Some(&(line.len() as u64 + 1)),
            "{progress:?}"
        );
        // The pass that hosted it is replayed: nothing is recorded twice.
        assert_eq!(nested.pass(&nested.base, &[]).recorded, 0);
    });
    assert_eq!(nested.rows().len(), 1);
}

// ---------------------------------------------------------------------------
// Adversarial cases for nested-worktree attribution.
// ---------------------------------------------------------------------------

type Row = (Option<String>, String, Option<String>);

impl Nested {
    /// Makes the anchor a Git repository, which task bindings require.
    fn init_git(&self) {
        use crate::test_support::{git_in, trust_repo};
        let root = std::path::Path::new(&self.base);
        git_in(root, &["init", "-b", "main"]);
        std::fs::write(root.join("seed.txt"), "seed").unwrap();
        git_in(root, &["add", "seed.txt"]);
        git_in(root, &["commit", "-m", "seed"]);
        trust_repo(root);
    }

    fn rows_for(&self, needle: &str) -> Vec<Row> {
        self.rows()
            .into_iter()
            .filter(|r| r.1.contains(needle))
            .collect()
    }
}

/// A cwd that goes through a dangling symlink is not a removed worktree: the
/// path has no `.claude/worktrees/<name>` shape, and nothing is statted.
#[cfg(unix)]
#[test]
fn adv_a_dangling_symlink_ancestor_is_not_a_departed_worktree() {
    let nested = Nested::new();
    let link = format!("{}/dangling", nested.base);
    std::os::unix::fs::symlink("/definitely/not/here", &link).unwrap();
    with_transcript_root(&nested.corpus, || {
        let cwd = format!("{link}/sub");
        write_lines(
            &nested.file("S1.jsonl"),
            &[command_line(&cwd, "S1", 1, "ls")],
        );
        nested.pass(&nested.base, &[]);
    });
    let rows = nested.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].0, None,
        "a path below a dangling link is not a worktree: {rows:?}"
    );
}

/// A cwd that is a regular file (or lies below one) is not a removed worktree.
#[test]
fn adv_a_cwd_that_is_a_file_is_not_departed() {
    let nested = Nested::new();
    let file = format!("{}/plain.txt", nested.base);
    std::fs::write(&file, "x").unwrap();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("F1.jsonl"),
            &[
                command_line(&file, "F1", 1, "ls"),
                command_line(&format!("{file}/below"), "F1", 2, "ls"),
            ],
        );
        let out = nested.pass(&nested.base, &[]);
        assert_eq!(out.recorded, 2, "{out:?}");
    });
    let rows = nested.rows();
    assert!(rows.iter().all(|r| r.0.is_none()), "{rows:?}");
}

/// A permission-denied directory is not departed: only the path decides, and
/// nothing about the directory's permissions is ever read.
#[cfg(unix)]
#[test]
fn adv_a_permission_denied_directory_is_not_departed() {
    use std::os::unix::fs::PermissionsExt;
    let nested = Nested::new();
    let locked = format!("{}/locked", nested.base);
    std::fs::create_dir_all(format!("{locked}/inner")).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("P1.jsonl"),
            &[command_line(&format!("{locked}/inner"), "P1", 1, "ls")],
        );
        nested.pass(&nested.base, &[]);
    });
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let rows = nested.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, None, "{rows:?}");
}

/// `/repo-old/.claude/worktrees/x` shares a byte prefix with `/repo` only.
#[test]
fn adv_a_sibling_repo_with_a_shared_prefix_is_nobodys() {
    let nested = Nested::new();
    let sibling = format!("{}-old", nested.base);
    with_transcript_root(&nested.corpus, || {
        let gone = format!("{sibling}/.claude/worktrees/x");
        write_lines(
            &nested.file("X1.jsonl"),
            &[
                command_line(&gone, "X1", 1, "ls"),
                edit_line(&gone, "X1", 2, &format!("{gone}/a.rs")),
            ],
        );
        nested.pass(&nested.base, &[]);
    });
    assert_eq!(
        nested.rows(),
        vec![],
        "a foreign repo is never recorded here"
    );
}

/// The last worktree is removed and `.claude` goes with it: the departed root
/// must still be the worktree, not a plain parent directory.
#[test]
fn adv_a_removed_worktrees_directory_does_not_become_the_root() {
    let nested = Nested::new();
    let w = nested.worktrees[0].clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("R1.jsonl"),
            &[edit_line(&w, "R1", 1, &format!("{w}/a.rs"))],
        );
        std::fs::remove_dir_all(format!("{}/.claude", nested.base)).unwrap();
        nested.pass(&nested.base, &[]);
    });
    let rows = nested.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].0.as_deref(),
        Some(w.as_str()),
        "worktree_path must be the worktree, got {:?}",
        rows[0].0
    );
}

/// A plain subdirectory deleted by a branch switch, used as cwd by an old
/// session in main. Only `.claude/worktrees/<name>` is read as a removed
/// worktree, so this is main's work: it stays on main, with main's task.
#[test]
fn adv_a_deleted_plain_subdirectory_stays_on_main() {
    let nested = Nested::new();
    let base = nested.base.clone();
    nested.init_git();
    ledger::bindings::bind(&base, &base, "T-MAIN").unwrap();
    with_transcript_root(&nested.corpus, || {
        let gone = format!("{base}/old_dir_removed_by_checkout");
        write_lines(
            &nested.file("Q1.jsonl"),
            &[
                command_line_at(&gone, "Q1", &now_stamp(), "cargo test"),
                edit_line_at(&gone, "Q1", &now_stamp(), &format!("{gone}/a.rs")),
            ],
        );
        nested.pass(&nested.base, &[]);
    });
    let rows = nested.rows();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for r in &rows {
        assert_eq!(
            r.0, None,
            "main work attributed to a fake worktree: {rows:?}"
        );
        assert_eq!(r.2.as_deref(), Some("T-MAIN"), "{rows:?}");
    }
}

/// w removed, a call recorded as departed, then a different worktree created at
/// the same path: later calls are live again, earlier ones stay departed rows,
/// and replaying changes nothing.
#[test]
fn adv_a_worktree_recreated_at_the_same_path_between_passes() {
    let nested = Nested::new();
    let w = nested.worktrees[0].clone();
    nested.init_git();
    std::fs::remove_dir_all(&w).unwrap();
    crate::test_support::git_in(
        std::path::Path::new(&nested.base),
        &["worktree", "add", "--detach", &w],
    );
    crate::test_support::trust_repo(std::path::Path::new(&w));
    ledger::bindings::bind(&nested.base, &w, "T-W").unwrap();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("C1.jsonl"),
            &[edit_line_at(&w, "C1", &now_stamp(), &format!("{w}/a.rs"))],
        );
        nested.pass(&nested.base, &[&w]);
        std::fs::remove_dir_all(&w).unwrap();
        append_text(
            &nested.file("C1.jsonl"),
            &(edit_line(&w, "C1", 2, &format!("{w}/gone.rs")) + "\n"),
        );
        nested.pass(&nested.base, &[]);
        std::fs::create_dir_all(format!("{w}/src")).unwrap();
        append_text(
            &nested.file("C1.jsonl"),
            &(edit_line_at(&w, "C1", &now_stamp(), &format!("{w}/back.rs")) + "\n"),
        );
        nested.pass(&nested.base, &[&w]);
        let first = nested.rows();
        for _ in 0..3 {
            let out = nested.pass(&nested.base, &[&w]);
            assert_eq!(out.recorded, 0, "{out:?}");
            assert_eq!(nested.rows(), first);
        }
    });
    let rows = nested.rows();
    let task = |needle: &str| nested.rows_for(needle)[0].2.clone();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(
        rows.iter().all(|r| r.0.as_deref() == Some(w.as_str())),
        "{rows:?}"
    );
    assert_eq!(task("gone.rs"), None);
    assert_eq!(task("a.rs").as_deref(), Some("T-W"));
    assert_eq!(task("back.rs").as_deref(), Some("T-W"));
}

/// Worktree b nested in worktree a (both under the repo).
#[test]
fn adv_a_worktree_nested_two_levels_deep() {
    let nested = Nested::new();
    let a = nested.worktrees[0].clone();
    let b = format!("{a}/.claude/worktrees/b");
    std::fs::create_dir_all(format!("{b}/src")).unwrap();
    // live a, departed b: b owns its calls.
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("L1.jsonl"),
            &[edit_line(&b, "L1", 1, &format!("{b}/x.rs"))],
        );
        std::fs::remove_dir_all(&b).unwrap();
        nested.pass(&nested.base, &[&a]);
    });
    let rows = nested.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].0.as_deref(),
        Some(b.as_str()),
        "live a, departed b: {rows:?}"
    );

    // both departed: the deepest removed root is b, not a.
    let nested = Nested::new();
    let a = nested.worktrees[0].clone();
    let b = format!("{a}/.claude/worktrees/b");
    std::fs::create_dir_all(format!("{b}/src")).unwrap();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("L2.jsonl"),
            &[edit_line(&b, "L2", 1, &format!("{b}/x.rs"))],
        );
        std::fs::remove_dir_all(&a).unwrap();
        nested.pass(&nested.base, &[]);
    });
    let rows = nested.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].0.as_deref(),
        Some(b.as_str()),
        "both departed: {rows:?}"
    );
}

/// 300 distinct departed cwds in one pass. Detection is pure string work with
/// no memo, so every one of them keeps its own worktree; only the cursors of
/// owners past the per-pass bound are skipped, and counted.
#[test]
fn adv_more_departed_cwds_than_the_memo_holds() {
    let nested = Nested::new();
    let base = nested.base.clone();
    with_transcript_root(&nested.corpus, || {
        let lines: Vec<String> = (0..300)
            .map(|i| {
                let g = format!("{base}/.claude/worktrees/gone{i}");
                edit_line(&g, "M1", (i % 59) as u32, &format!("{g}/a.rs"))
            })
            .collect();
        write_lines(&nested.file("M1.jsonl"), &lines);
        let out = nested.pass(&nested.base, &[]);
        assert_eq!(out.recorded, 300);
        assert_eq!(out.rehosted_calls, 0, "{out:?}");
        // 256 new removed worktrees are entered per pass; the other 44 are
        // counted, and still recorded under their own paths.
        assert_eq!(out.departed_overflow, 44, "{out:?}");
    });
    let rows = nested.rows();
    let main = rows.iter().filter(|r| r.0.is_none()).count();
    println!("MEMO main_rows={main} departed_rows={}", rows.len() - main);
    assert_eq!(main, 0, "{main} of 300 departed calls fell back to main");
}

/// A family bigger than the participant cap: a call in worktree #70 must still
/// be recorded somewhere (it went to main before attribution picked one owner).
#[test]
fn adv_a_worktree_beyond_the_participant_cap_is_not_lost() {
    let nested = Nested::new();
    let base = nested.base.clone();
    let wts: Vec<String> = (0..70)
        .map(|i| {
            let p = format!("{base}/.claude/worktrees/live{i:02}");
            std::fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();
    with_transcript_root(&nested.corpus, || {
        let lines: Vec<String> = wts
            .iter()
            .enumerate()
            .map(|(i, w)| edit_line(w, "B1", (i % 59) as u32, &format!("{w}/a.rs")))
            .collect();
        write_lines(&nested.file("B1.jsonl"), &lines);
        let out = pass::ingest_family_until(&base, &base, &wts, &mut |_| false);
        // The trigger and 63 siblings have a slot; the other 7 are hosted on
        // the deepest root that has one, and counted.
        assert_eq!(out.recorded, 70, "{out:?}");
        assert_eq!(out.rehosted_calls, 7, "{out:?}");
    });
    let rows = nested.rows();
    assert_eq!(
        rows.len(),
        70,
        "calls lost beyond the cap: {}",
        70 - rows.len()
    );
}

/// Hostile and malformed records, none of which may panic or duplicate.
#[test]
fn adv_hostile_records_never_panic_or_double() {
    let nested = Nested::new();
    let base = nested.base.clone();
    let w = nested.worktrees[0].clone();
    let raw = |cwd: Option<&str>, tool: &str, input: serde_json::Value, sec: u32| {
        let mut v = serde_json::json!({
            "type":"assistant","sessionId":"Z1",
            "timestamp": format!("2026-09-01T12:00:{sec:02}.000Z"),
            "message":{"content":[{"type":"tool_use","name":tool,"input":input}]}
        });
        if let Some(c) = cwd {
            v["cwd"] = serde_json::json!(c);
        }
        v.to_string()
    };
    with_transcript_root(&nested.corpus, || {
        let lines = vec![
            raw(None, "Bash", serde_json::json!({"command":"no cwd"}), 1),
            raw(
                Some(""),
                "Bash",
                serde_json::json!({"command":"empty cwd"}),
                2,
            ),
            raw(
                Some("foo/bar"),
                "Bash",
                serde_json::json!({"command":"relative"}),
                3,
            ),
            raw(Some("/"), "Bash", serde_json::json!({"command":"root"}), 4),
            raw(
                Some(&base),
                "Edit",
                serde_json::json!({"file_path":"/elsewhere/a.rs"}),
                5,
            ),
            raw(
                Some(&base),
                "Edit",
                serde_json::json!({"file_path":format!("{w}/under_w.rs")}),
                6,
            ),
            raw(
                Some(&format!("{base}/.claude/worktrees/ghost")),
                "Edit",
                serde_json::json!({"file_path":format!("{base}/main_file.rs")}),
                7,
            ),
            raw(
                Some(&format!("{base}/{}", "x".repeat(1 << 20))),
                "Bash",
                serde_json::json!({"command":"huge"}),
                8,
            ),
            "{not json".to_string(),
        ];
        write_lines(&nested.file("Z1.jsonl"), &lines);
        append_text(&nested.file("Z1.jsonl"), "{\"cwd\":\"");
        let mut f = std::fs::File::options()
            .append(true)
            .open(nested.file("Z1.jsonl"))
            .unwrap();
        std::io::Write::write_all(&mut f, b"\xff\xfe\"}\n").unwrap();
        let out = nested.pass(&base, &[&w]);
        assert_eq!(out.error, "");
        let again = nested.pass(&base, &[&w]);
        assert_eq!(again.recorded, 0);
    });
    let rows = nested.rows();
    let by = |n: &str| nested.rows_for(n);
    assert_eq!(
        by("under_w.rs")[0].0.as_deref(),
        Some(w.as_str()),
        "{rows:?}"
    );
    // cwd in a ghost worktree but the edited file is in main: not the ghost's.
    assert_eq!(by("main_file.rs")[0].0, None, "{rows:?}");
    assert_eq!(by("/elsewhere/a.rs").len(), 0, "{rows:?}");
    assert_eq!(
        by("relative").len() + by("root").len() + by("empty cwd").len(),
        0,
        "{rows:?}"
    );
}

/// A transcript shared by two departed worktrees, one of which ran alone first.
#[test]
fn adv_one_transcript_two_departed_worktrees() {
    let nested = Nested::new();
    let [w1, w2, _] = nested.worktrees.clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("K1.jsonl"),
            &[
                edit_line(&w1, "K1", 1, &format!("{w1}/a.rs")),
                edit_line(&w2, "K1", 2, &format!("{w2}/a.rs")),
            ],
        );
        ingest_transcripts_into(&nested.base, &w1);
        std::fs::remove_dir_all(&w1).unwrap();
        std::fs::remove_dir_all(&w2).unwrap();
        append_text(
            &nested.file("K1.jsonl"),
            &(edit_line(&w1, "K1", 3, &format!("{w1}/b.rs"))
                + "\n"
                + &edit_line(&w2, "K1", 4, &format!("{w2}/b.rs"))
                + "\n"),
        );
        nested.pass(&nested.base, &[]);
        nested.pass(&nested.base, &[]);
    });
    let rows = nested.rows();
    let n = |w: &str, f: &str| {
        rows.iter()
            .filter(|r| r.0.as_deref() == Some(w) && r.1.ends_with(f))
            .count()
    };
    assert_eq!(n(&w1, "a.rs"), 1, "{rows:?}");
    assert_eq!(n(&w1, "b.rs"), 1, "{rows:?}");
    assert_eq!(n(&w2, "a.rs"), 1, "{rows:?}");
    assert_eq!(n(&w2, "b.rs"), 1, "{rows:?}");
    assert_eq!(rows.len(), 4, "{rows:?}");
}

/// Rewrite longer than the departed cursor: the cursor cannot tell a rewritten
/// file from an appended one while it is not past the file's end, so the lines
/// it seems to have consumed are skipped. KNOWN LIMIT, shared with every live
/// worktree's cursor (`owed_on` detects a rotation by length alone) and out of
/// scope here; this test pins today's behaviour so a rotation-aware cursor
/// shows up as a deliberate change. Only the first rewritten line is affected:
/// the others lie past the stale cursor and are recorded once.
#[test]
fn adv_rotation_longer_than_the_cursor_skips_the_first_line_a_known_limit() {
    let nested = Nested::new();
    let w = nested.worktrees[0].clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("O1.jsonl"),
            &[edit_line(&w, "O1", 1, &format!("{w}/a.rs"))],
        );
        ingest_transcripts_into(&nested.base, &w);
        std::fs::remove_dir_all(&w).unwrap();
        write_lines(
            &nested.file("O1.jsonl"),
            &[
                edit_line(&w, "O1", 2, &format!("{w}/new1.rs")),
                edit_line(&w, "O1", 3, &format!("{w}/new2.rs")),
                edit_line(&w, "O1", 4, &format!("{w}/new3.rs")),
            ],
        );
        nested.pass(&nested.base, &[]);
    });
    let rows = nested.rows();
    assert_eq!(
        nested.rows_for("new1.rs").len(),
        0,
        "the known limit: the stale cursor covers the first rewritten line: {rows:?}"
    );
    for f in ["new2.rs", "new3.rs"] {
        assert_eq!(nested.rows_for(f).len(), 1, "{f}: {rows:?}");
    }
}

/// Race: two passes on the same family at once keep every row exactly-once,
/// departed rows included.
#[test]
fn adv_racing_passes_keep_departed_rows_exactly_once() {
    let nested = Nested::new();
    let base = nested.base.clone();
    let w = nested.worktrees[0].clone();
    with_transcript_root(&nested.corpus, || {
        std::fs::remove_dir_all(&w).unwrap();
        let lines: Vec<String> = (0..200)
            .map(|i| {
                if i % 2 == 0 {
                    edit_line(&w, "G1", (i % 59) as u32, &format!("{w}/f{i}.rs"))
                } else {
                    edit_line(&base, "G1", (i % 59) as u32, &format!("{base}/f{i}.rs"))
                }
            })
            .collect();
        write_lines(&nested.file("G1.jsonl"), &lines);
        let a = std::thread::scope(|s| {
            let h1 = s.spawn(|| nested.pass(&base, &[]));
            let h2 = s.spawn(|| nested.pass(&base, &[]));
            (h1.join().unwrap(), h2.join().unwrap())
        });
        println!("RACE recorded {} + {}", a.0.recorded, a.1.recorded);
        nested.pass(&base, &[]);
    });
    let rows = nested.rows();
    assert_eq!(rows.len(), 200, "{}", rows.len());
    let mut objs: Vec<_> = rows.iter().map(|r| r.1.clone()).collect();
    objs.sort();
    objs.dedup();
    assert_eq!(objs.len(), 200);
}

/// Replay idempotency over a mixed family, three passes.
#[test]
fn adv_replaying_a_mixed_family_is_idempotent() {
    let nested = Nested::new();
    let base = nested.base.clone();
    let [w1, w2, w3] = nested.worktrees.clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("I1.jsonl"),
            &[
                edit_line(&w1, "I1", 1, &format!("{w1}/a.rs")),
                edit_line(&w2, "I1", 2, &format!("{w2}/a.rs")),
                edit_line(&w3, "I1", 3, &format!("{w3}/a.rs")),
                edit_line(&base, "I1", 4, &format!("{base}/a.rs")),
            ],
        );
        std::fs::remove_dir_all(&w3).unwrap();
        nested.pass(&base, &[&w1, &w2]);
        let first = nested.rows();
        assert_eq!(first.len(), 4);
        for _ in 0..3 {
            nested.pass(&base, &[&w1, &w2]);
            assert_eq!(nested.rows(), first);
        }
    });
}

/// Property: random families, random calls; at most one row per call, at the
/// deepest live-or-departed root containing it.
#[test]
fn adv_property_one_row_per_call_at_the_deepest_root() {
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for round in 0..25 {
        let nested = Nested::new();
        let base = nested.base.clone();
        let n = 1 + (next() % 6) as usize;
        let mut roots: Vec<(String, bool)> = Vec::new();
        for i in 0..n {
            let parent = if i > 0 && next() % 3 == 0 {
                roots[(next() as usize) % roots.len()].0.clone()
            } else {
                base.clone()
            };
            roots.push((format!("{parent}/.claude/worktrees/p{round}_{i}"), true));
        }
        for (p, _) in &roots {
            std::fs::create_dir_all(format!("{p}/src")).unwrap();
        }
        for (p, _) in roots.iter() {
            if next() % 3 == 0 {
                let _ = std::fs::remove_dir_all(p);
            }
        }
        for (p, live) in roots.iter_mut() {
            *live = std::fs::symlink_metadata(&*p).is_ok();
        }
        let live: Vec<String> = roots.iter().filter(|r| r.1).map(|r| r.0.clone()).collect();
        let mut lines = Vec::new();
        let mut expected: Vec<(String, String)> = Vec::new();
        for k in 0..30u32 {
            let pick = (next() as usize) % (roots.len() + 1);
            let dir = if pick == roots.len() {
                base.clone()
            } else {
                roots[pick].0.clone()
            };
            let file = format!("{dir}/f{round}_{k}.rs");
            lines.push(edit_line(&dir, "PR", k % 59, &file));
            // The deepest root the file lies under, live or removed: a removed
            // root nested in another removed root is its own (the deepest
            // convention match wins).
            let best = roots
                .iter()
                .map(|r| &r.0)
                .filter(|r| file.starts_with(&format!("{r}/")))
                .max_by_key(|r| r.len())
                .cloned()
                .unwrap_or_default();
            expected.push((file, best));
        }
        with_transcript_root(&nested.corpus, || {
            write_lines(&nested.file("PR.jsonl"), &lines);
            let sib: Vec<&str> = live.iter().map(String::as_str).collect();
            nested.pass(&base, &sib);
            nested.pass(&base, &sib);
        });
        let rows = nested.rows();
        for (file, want) in &expected {
            let got: Vec<_> = rows.iter().filter(|r| &r.1 == file).collect();
            assert_eq!(got.len(), 1, "round {round}: {file}: {got:?}");
            assert_eq!(
                got[0].0.clone().unwrap_or_default(),
                *want,
                "round {round}: {file} roots={roots:?}"
            );
        }
    }
}

fn perf_n(var: &str, default: u32) -> u32 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Timing, no threshold: lines over a live family, and distinct cwds. Set
/// GITPULSE_ADV_PERF_LINES=100000 and GITPULSE_ADV_PERF_CWDS=10000 for the full size.
#[test]
#[ignore = "timing only, no assertion on speed; benches/repo_read.rs measures this in release"]
fn adv_perf_pass_time_on_a_large_corpus() {
    let mut best_a = u128::MAX;
    let mut best_b = u128::MAX;
    for _ in 0..3 {
        let nested = Nested::new();
        let wts = nested.worktrees.clone();
        let base = nested.base.clone();
        with_transcript_root(&nested.corpus, || {
            let roots = [base.clone(), wts[0].clone(), wts[1].clone(), wts[2].clone()];
            let lines: Vec<String> = (0..perf_n("GITPULSE_ADV_PERF_LINES", 20_000))
                .map(|i| {
                    let r = &roots[0];
                    edit_line(
                        &format!("{r}/src"),
                        "PF",
                        i % 59,
                        &format!("{r}/src/f{}.rs", i % 500),
                    )
                })
                .collect();
            write_lines(&nested.file("PF.jsonl"), &lines);
            let sib: Vec<String> = wts.to_vec();
            let t = std::time::Instant::now();
            let out = pass::ingest_family_until(&base, &base, &sib, &mut |_| false);
            best_a = best_a.min(t.elapsed().as_millis());
            assert_eq!(
                out.recorded,
                i64::from(perf_n("GITPULSE_ADV_PERF_LINES", 20_000)),
                "{out:?}"
            );
        });
        let nested = Nested::new();
        let base = nested.base.clone();
        with_transcript_root(&nested.corpus, || {
            let lines: Vec<String> = (0..perf_n("GITPULSE_ADV_PERF_CWDS", 2_000))
                .map(|i| {
                    let c = format!("{base}/d{i}");
                    edit_line(&c, "PG", i % 59, &format!("{c}/a.rs"))
                })
                .collect();
            write_lines(&nested.file("PG.jsonl"), &lines);
            let t = std::time::Instant::now();
            let out = pass::ingest_family_until(&base, &base, &[], &mut |_| false);
            best_b = best_b.min(t.elapsed().as_millis());
            assert_eq!(
                out.recorded,
                i64::from(perf_n("GITPULSE_ADV_PERF_CWDS", 2_000)),
                "{out:?}"
            );
        });
    }
    println!("PERF 100k-lines min-of-3 = {best_a} ms; 10k-distinct-cwds min-of-3 = {best_b} ms");
}

/// A worktree past the participant cap owns its rows: the row is under its own
/// path with its own task, its cursor moves in the same transaction, and a pass
/// triggered by it afterwards owes nothing.
#[test]
fn a_worktree_beyond_the_cap_owns_its_rows_and_its_cursor_moves() {
    use crate::test_support::{git_in, trust_repo};
    let nested = Nested::new();
    let base = nested.base.clone();
    nested.init_git();
    let mut wts: Vec<String> = (0..68)
        .map(|i| {
            let p = format!("{base}/.claude/worktrees/live{i:02}");
            std::fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();
    // The last two are real worktrees, so a task can be bound to them.
    for name in ["real68", "real69"] {
        let p = format!("{base}/.claude/worktrees/{name}");
        git_in(
            std::path::Path::new(&base),
            &["worktree", "add", "--detach", &p],
        );
        trust_repo(std::path::Path::new(&p));
        ledger::bindings::bind(&base, &p, &format!("T-{name}")).unwrap();
        wts.push(p);
    }
    let real69 = wts[69].clone();
    with_transcript_root(&nested.corpus, || {
        let lines: Vec<String> = wts
            .iter()
            .map(|w| edit_line_at(w, "B2", &now_stamp(), &format!("{w}/a.rs")))
            .collect();
        write_lines(&nested.file("B2.jsonl"), &lines);
        let first = pass::ingest_family_until(&base, &base, &wts, &mut |_| false);
        assert_eq!(first.recorded, 70, "{first:?}");
        assert_eq!(first.rehosted_calls, 7, "{first:?}");

        let rows = nested.rows();
        for w in &wts {
            assert_eq!(
                rows.iter()
                    .filter(|r| r.0.as_deref() == Some(w.as_str()))
                    .count(),
                1,
                "{w}: {rows:?}"
            );
        }
        let task = |w: &str| {
            rows.iter()
                .find(|r| r.0.as_deref() == Some(w))
                .and_then(|r| r.2.clone())
        };
        assert_eq!(task(&real69).as_deref(), Some("T-real69"));
        assert_eq!(task(&wts[68]).as_deref(), Some("T-real68"));
        // The hosted, off-cap owner's task came from its own history, the same
        // read every other worktree gets; one nothing was bound to decides none.
        let basis = |w: &str| {
            let event = ledger::tail(&base, 0, 5000)
                .unwrap()
                .into_iter()
                .find(|event| {
                    event.worktree_path.as_deref() == Some(w)
                        && transcript::ACTIONS.contains(&event.action.as_str())
                })
                .unwrap();
            let detail: serde_json::Value =
                serde_json::from_str(event.detail_json.as_deref().unwrap()).unwrap();
            detail["task_basis"].clone()
        };
        assert_eq!(basis(&real69), "bound");
        assert_eq!(basis(&wts[0]), "unresolved");

        // A pass triggered by a worktree that was past the cap: it has its own
        // cursor now, and the family's order has changed who has a slot.
        let mut family = vec![base.clone()];
        family.extend(wts.iter().filter(|w| **w != real69).cloned());
        let second = pass::ingest_family_until(&base, &real69, &family, &mut |_| false);
        assert_eq!(second.recorded, 0, "{second:?}");
        assert_eq!(nested.rows(), rows);
    });
}

/// More new removed worktrees than one pass enters: every row is still the
/// owner's, the overflow is counted, and no owner past the bound gets a cursor.
#[test]
fn departed_owners_past_the_bound_still_record_under_their_own_path() {
    let nested = Nested::new();
    let base = nested.base.clone();
    with_transcript_root(&nested.corpus, || {
        let lines: Vec<String> = (0..300)
            .map(|i| {
                let g = format!("{base}/.claude/worktrees/gone{i}");
                edit_line(&g, "V1", (i % 59) as u32, &format!("{g}/a.rs"))
            })
            .collect();
        write_lines(&nested.file("V1.jsonl"), &lines);
        let out = nested.pass(&base, &[]);
        assert_eq!(out.recorded, 300, "{out:?}");
        assert_eq!(out.departed_overflow, 44, "{out:?}");
        let rows = nested.rows();
        assert!(rows.iter().all(|r| r.0.is_some()), "{rows:?}");
        let key = ledger::transcript_key(&nested.file("V1.jsonl"));
        let cursors = (0..300)
            .filter(|i| {
                let g = format!("{base}/.claude/worktrees/gone{i}");
                ledger::transcript_progress(&base, &g)
                    .unwrap()
                    .offsets
                    .contains_key(&key)
            })
            .count();
        assert_eq!(cursors, 256);
        // Replayed: the host's cursor keeps every row exactly-once.
        assert_eq!(nested.pass(&base, &[]).recorded, 0);
    });
    assert_eq!(nested.rows().len(), 300);
}

/// A directory that is not a Git checkout has no family to resolve: it still
/// gets its pass for itself alone, with no removed worktree recognised.
#[test]
fn a_non_git_directory_still_gets_its_trigger_only_pass() {
    let nested = Nested::new();
    let w = nested.worktrees[0].clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("U1.jsonl"),
            &[edit_line(&w, "U1", 1, &format!("{w}/a.rs"))],
        );
        let out = catch_up_into_bounded(&nested.base, &nested.base, None);
        assert_eq!(out.recorded, 1, "{out:?}");
        assert!(out.family_unresolved, "{out:?}");
        assert_eq!(out.family_unresolved_cycles, 0, "{out:?}");
        let rows = nested.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, None, "{rows:?}");
        let progress = ledger::transcript_progress(&nested.base, &w).unwrap();
        assert!(progress.offsets.is_empty(), "{progress:?}");
    });
}

/// A Git checkout with one real worktree `w` (bound to a task) under it, and
/// one transcript call in `w`; the checkout's trust is then revoked so its
/// family cannot be resolved.
fn unresolved_git_family(nested: &Nested) -> String {
    use crate::test_support::{git_in, trust_repo};
    let base = std::path::Path::new(&nested.base);
    nested.init_git();
    let w = nested.worktrees[0].clone();
    std::fs::remove_dir_all(&w).unwrap();
    git_in(base, &["worktree", "add", "--detach", &w]);
    trust_repo(std::path::Path::new(&w));
    ledger::bindings::bind(&nested.base, &w, "T-W").unwrap();
    write_lines(
        &nested.file("X1.jsonl"),
        &[edit_line_at(&w, "X1", &now_stamp(), &format!("{w}/a.rs"))],
    );
    crate::repository_trust::revoke(&nested.base).unwrap();
    w
}

/// A Git checkout whose family cannot be resolved may have live worktrees
/// nested under it, so a pass for the trigger alone would record their calls
/// under it and move only its cursor. The cycle writes nothing and says so,
/// and the count of consecutive skipped cycles grows.
#[test]
fn an_unresolved_git_family_writes_nothing() {
    let nested = Nested::new();
    with_transcript_root(&nested.corpus, || {
        let w = unresolved_git_family(&nested);
        let first = catch_up_into_bounded(&nested.base, &nested.base, None);
        assert!(first.family_unresolved, "{first:?}");
        // An untrusted checkout fails the reflog phase first, and that error
        // stands; the skip's own message is what any other failure would carry.
        assert!(!first.error.is_empty(), "{first:?}");
        assert_eq!(first.recorded, 0, "{first:?}");
        assert_eq!(first.transcripts, 0, "{first:?}");
        assert_eq!(first.family_unresolved_cycles, 1, "{first:?}");
        assert_eq!(nested.rows(), vec![]);
        for path in [&nested.base, &w] {
            let progress = ledger::transcript_progress(&nested.base, path).unwrap();
            assert!(progress.offsets.is_empty(), "{path}: {progress:?}");
        }
        let second = catch_up_into_bounded(&nested.base, &nested.base, None);
        assert_eq!(second.family_unresolved_cycles, 2, "{second:?}");
    });
}

/// An unresolved cycle and then a resolved one record the call once: under the
/// worktree it ran in, with that worktree's task.
#[test]
fn an_unresolved_pass_then_a_resolved_pass_records_each_call_once() {
    let nested = Nested::new();
    with_transcript_root(&nested.corpus, || {
        let w = unresolved_git_family(&nested);
        let skipped = catch_up_into_bounded(&nested.base, &nested.base, None);
        assert!(skipped.family_unresolved, "{skipped:?}");
        crate::test_support::trust_repo(std::path::Path::new(&nested.base));
        let resolved = catch_up_into_bounded(&nested.base, &nested.base, None);
        assert!(!resolved.family_unresolved, "{resolved:?}");
        assert_eq!(resolved.family_unresolved_cycles, 0, "{resolved:?}");
        assert_eq!(
            nested.rows(),
            vec![(
                Some(w.clone()),
                format!("{w}/a.rs"),
                Some("T-W".to_string())
            )]
        );
        let again = catch_up_into_bounded(&nested.base, &nested.base, None);
        assert_eq!(again.recorded, 0, "{again:?}");
    });
}

/// The failure is reported, so an unresolved family never reads as a clean one.
#[test]
fn an_unresolved_family_is_reported() {
    let nested = Nested::new();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("U2.jsonl"),
            &[edit_line(
                &nested.base,
                "U2",
                1,
                &format!("{}/a.rs", nested.base),
            )],
        );
        let unresolved = catch_up_into_bounded(&nested.base, &nested.base, None);
        assert!(unresolved.family_unresolved, "{unresolved:?}");
        assert!(
            CatchUp::default()
                .with_transcripts(&unresolved)
                .family_unresolved
        );
        let resolved = nested.pass(&nested.base, &[]);
        assert!(!resolved.family_unresolved, "{resolved:?}");
    });
}

/// The detail object of the one transcript row naming `needle`.
fn row_detail(base: &str, needle: &str) -> serde_json::Value {
    let event = ledger::tail(base, 0, 5000)
        .unwrap()
        .into_iter()
        .find(|event| event.object.as_deref().is_some_and(|o| o.contains(needle)))
        .unwrap_or_else(|| panic!("no row for {needle}"));
    serde_json::from_str(event.detail_json.as_deref().unwrap()).unwrap()
}

/// A removed worktree is attributed from its own path's bind history, with no
/// family authentication (a bind row exists only for a path that was
/// authenticated when it was bound): a call after its bind carries the task, a
/// call from before it does not, and neither takes the main checkout's task.
#[test]
fn a_departed_worktree_resolves_from_its_own_history() {
    use crate::test_support::{git_in, trust_repo};
    let main = tempfile::tempdir().unwrap();
    let base = main.path().canonicalize().unwrap();
    git_in(&base, &["init", "-b", "main"]);
    std::fs::write(base.join("seed.txt"), "seed").unwrap();
    git_in(&base, &["add", "seed.txt"]);
    git_in(&base, &["commit", "-m", "seed"]);
    let linked = base.join(".claude/worktrees/w");
    git_in(
        &base,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    trust_repo(&linked);
    let w = linked
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let anchor = base.to_string_lossy().into_owned();
    ledger::bindings::bind(&anchor, &anchor, "T-MAIN").unwrap();
    ledger::bindings::bind(&anchor, &w, "T-W").unwrap();

    let corpus = tempfile::tempdir().unwrap();
    let corpus_dir = corpus.path().canonicalize().unwrap();
    let file = corpus_dir.join("slug/D1.jsonl");
    with_transcript_root(&corpus_dir, || {
        git_in(&base, &["worktree", "remove", "--force", &w]);
        write_lines(
            &file,
            &[
                edit_line_at(&w, "D1", &now_stamp(), &format!("{w}/after.rs")),
                edit_line(&w, "D1", 1, &format!("{w}/before.rs")),
            ],
        );
        let out = catch_up_into_bounded(&anchor, &anchor, None);
        assert_eq!(out.recorded >= 2, true, "{out:?}");
    });
    let task = |needle: &str| {
        ledger::tail(&anchor, 0, 5000)
            .unwrap()
            .into_iter()
            .find(|event| event.object.as_deref().is_some_and(|o| o.contains(needle)))
            .map(|event| (event.worktree_path, event.task_id))
            .unwrap()
    };
    assert_eq!(task("after.rs"), (Some(w.clone()), Some("T-W".into())));
    assert_eq!(task("before.rs"), (Some(w.clone()), None));
    assert_eq!(row_detail(&anchor, "after.rs")["task_basis"], "bound");
    assert_eq!(row_detail(&anchor, "before.rs")["task_basis"], "unresolved");
}

/// A removed worktree nothing was ever bound to has no history to read: its
/// calls are `unresolved`, never the host's or the main checkout's task.
#[test]
fn a_departed_worktree_with_no_history_is_unresolved() {
    let nested = Nested::new();
    let base = nested.base.clone();
    nested.init_git();
    ledger::bindings::bind(&base, &base, "T-MAIN").unwrap();
    let w = nested.worktrees[0].clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("D2.jsonl"),
            &[edit_line_at(&w, "D2", &now_stamp(), &format!("{w}/a.rs"))],
        );
        std::fs::remove_dir_all(&w).unwrap();
        nested.pass(&base, &[]);
    });
    let rows = nested.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0.as_deref(), Some(w.as_str()), "{rows:?}");
    assert_eq!(rows[0].2, None, "{rows:?}");
    assert_eq!(row_detail(&base, "a.rs")["task_basis"], "unresolved");
}

/// The baseline floor compares instants. A call spelled with a far-east offset
/// reads as a later wall clock than the baseline's UTC text while being an
/// earlier instant, so comparing the raw strings kept a call the old watermark
/// had already recorded, and recorded it twice.
#[test]
fn the_baseline_floor_compares_instants_not_text() {
    let nested = Nested::new();
    let base = nested.base.clone();
    ledger::append(ledger::Draft {
        repo_path: base.clone(),
        action: "session.edit".into(),
        object: Some("legacy".into()),
        ..Default::default()
    })
    .unwrap();
    let now = ledger::ids::now_millis();
    let spelled = |ms: u64| ledger::ids::iso8601_utc(ms + 14 * 3_600_000).replace('Z', "+14:00");
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("F1.jsonl"),
            &[
                // An hour before the baseline, spelled at +14:00.
                edit_line_at(
                    &base,
                    "F1",
                    &spelled(now - 3_600_000),
                    &format!("{base}/old.rs"),
                ),
                // Half a minute after it.
                edit_line_at(
                    &base,
                    "F1",
                    &spelled(now + 30_000),
                    &format!("{base}/new.rs"),
                ),
            ],
        );
        let out = nested.pass(&base, &[]);
        assert_eq!(out.recorded, 1, "{out:?}");
    });
    let rows: Vec<_> = nested
        .rows()
        .into_iter()
        .filter(|row| row.1 != "legacy")
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].1.ends_with("new.rs"), "{rows:?}");
}
