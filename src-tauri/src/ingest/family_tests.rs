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

/// Transcript content is untrusted: a hostile cwd never panics, never invents
/// a departed worktree, and is counted rather than silently attributed.
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
        assert_eq!(out.unverified_departed, 5, "{out:?}");
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
        write_lines(&file, &[edit_line(&w, "G1", 1, &format!("{w}/a.rs"))]);
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
                + &edit_line(&anchor, "G1", 3, &format!("{anchor}/m.rs"))
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
    });
}

/// The existence check is injected: one that fails is counted and the call
/// keeps its live attribution, where one that says "gone" makes a departed row.
#[test]
fn a_failing_existence_check_is_counted_and_the_call_keeps_its_live_root() {
    let nested = Nested::new();
    let w = nested.worktrees[0].clone();
    let siblings: Vec<String> = Vec::new();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("E1.jsonl"),
            &[command_line(&format!("{w}/src"), "E1", 1, "ls")],
        );
        let broken = |_: &str| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let out = pass::ingest_family_with(
            &nested.base,
            &nested.base,
            &siblings,
            &mut |_| false,
            &broken,
        );
        assert_eq!(out.unverified_departed, 1, "{out:?}");
        assert_eq!(out.recorded, 1, "{out:?}");
    });
    assert_eq!(nested.rows()[0].0, None);

    // Everything below `.claude` reported gone: the worktree itself departed.
    let nested = Nested::new();
    let w = nested.worktrees[0].clone();
    with_transcript_root(&nested.corpus, || {
        write_lines(
            &nested.file("E2.jsonl"),
            &[command_line(&format!("{w}/src"), "E2", 1, "ls")],
        );
        let gone = |path: &str| Ok(!path.starts_with(&w));
        let out =
            pass::ingest_family_with(&nested.base, &nested.base, &siblings, &mut |_| false, &gone);
        assert_eq!(out.unverified_departed, 0, "{out:?}");
    });
    assert_eq!(nested.rows()[0].0.as_deref(), Some(w.as_str()));
}
