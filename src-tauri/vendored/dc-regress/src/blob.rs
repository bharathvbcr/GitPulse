//! Blob reads for one analysis: identity and content of `<rev>:<path>`, each
//! resolved once, through two long-lived `git cat-file` processes.
//!
//! The reads used to be three spawns each — `rev-parse` the revision,
//! `rev-parse` the blob, `git show` it — and the same `(rev, path)` was read
//! twice, once for the graph's basis and once for the content. A change of a
//! few hundred files cost a thousand processes, most of them starting a git
//! that then listed the repository's config before reading anything.
//!
//! A [`BlobReader`] spawns at most one `cat-file --batch-check` (identity) and
//! one `cat-file --batch` (content) per analysis, however many files are read,
//! and remembers what it was told:
//!
//! * a revision is resolved to a commit id once ([`crate::history::commit_oid`]),
//!   so only an object id ever reaches a request, and a request is
//!   `<commit-oid>:<path>` — a name git reads as a path in that tree and never
//!   as an option;
//! * a `(commit, path)` identity is asked once. Content hands its identity to
//!   the memo as it passes, so the read that follows an identity query costs
//!   no second answer and a basis check after a content read costs nothing.
//!
//! Both processes are started through [`dc_proc::git_with_program`], so they
//! carry every guard a one-shot read does. `cat-file` runs no filter driver,
//! textconv or hook; with `GIT_NO_LAZY_FETCH` set a missing object in a partial
//! clone is `missing`, not a fetch.
//!
//! A path git cannot be asked about on one line — it contains a newline — is
//! refused alone. Each request waits at most
//! [`crate::history::HISTORY_DEADLINE`]; a process that stops answering is
//! killed and every later request says so rather than waiting again.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dc_proc::{git_with_program, spawn_session, Session};

use crate::history::ResolvedBlob;
use crate::history::{commit_oid, HistoryRefusal, CONTENT_CAP, HISTORY_DEADLINE};
use crate::BlobIdentity;

/// Entries the memos keep. A walk past it forgets and asks again — bounded
/// memory is worth more than a perfect hit rate on a sweep this large.
const MEMO_CAP: usize = 100_000;

/// Longest header line accepted from `cat-file`; it echoes the request on a
/// miss, and a request is a path.
const HEADER_LIMIT: u64 = 64 * 1024;

/// How the listening side of a request wakes to look at its caller's
/// patience.
const POLL: Duration = Duration::from_millis(50);

/// What `cat-file` answered for one request.
#[derive(Debug)]
enum Record {
    Missing,
    Ambiguous,
    Object {
        oid: String,
        kind: String,
        size: u64,
        /// The bytes, when this lane carries them and they fit the cap.
        body: Option<Vec<u8>>,
    },
}

type Answer = Result<Record, String>;

/// One running `cat-file` and the thread reading its answers.
struct Lane {
    session: Session,
    stdin: ChildStdin,
    answers: Receiver<Answer>,
    /// Set when the process stopped answering; every later request repeats it.
    broken: Option<HistoryRefusal>,
}

impl Lane {
    fn start(program: &OsStr, repo: &Path, with_content: bool) -> Result<Lane, HistoryRefusal> {
        let flag = if with_content {
            "--batch"
        } else {
            "--batch-check"
        };
        let mut command = git_with_program(program, repo, ["cat-file", flag]).map_err(|f| {
            HistoryRefusal::GitUnavailable {
                reason: f.to_string(),
            }
        })?;
        let mut session = spawn_session(&mut command, Duration::from_secs(10)).map_err(|f| {
            HistoryRefusal::GitUnavailable {
                reason: f.to_string(),
            }
        })?;
        let (Some(stdin), Some(stdout)) = (session.take_stdin(), session.take_stdout()) else {
            return Err(HistoryRefusal::GitUnavailable {
                reason: "git cat-file started without its pipes".to_string(),
            });
        };
        let (tx, answers) = mpsc::channel();
        std::thread::spawn(move || read_answers(stdout, with_content, tx));
        Ok(Lane {
            session,
            stdin,
            answers,
            broken: None,
        })
    }

    fn ask(&mut self, spec: &str) -> Result<Record, HistoryRefusal> {
        if let Some(broken) = &self.broken {
            return Err(broken.clone());
        }
        let sent = self
            .stdin
            .write_all(spec.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush());
        if let Err(error) = sent {
            return Err(self.break_with(HistoryRefusal::GitUnavailable {
                reason: format!("git cat-file stopped accepting requests: {error}"),
            }));
        }
        let give_up = Instant::now() + HISTORY_DEADLINE;
        loop {
            match self.answers.recv_timeout(POLL) {
                Ok(Ok(record)) => return Ok(record),
                Ok(Err(message)) => {
                    return Err(self.break_with(HistoryRefusal::GitUnavailable {
                        reason: format!("git cat-file answered unreadably: {message}"),
                    }))
                }
                Err(RecvTimeoutError::Timeout) if Instant::now() < give_up => {}
                Err(RecvTimeoutError::Timeout) => {
                    return Err(self.break_with(HistoryRefusal::Deadline {
                        what: "git cat-file".to_string(),
                    }))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.break_with(HistoryRefusal::GitUnavailable {
                        reason: "git cat-file ended before answering".to_string(),
                    }))
                }
            }
        }
    }

    fn break_with(&mut self, refusal: HistoryRefusal) -> HistoryRefusal {
        self.session.kill();
        self.broken = Some(refusal.clone());
        refusal
    }
}

/// Reads `cat-file` answers off `stdout` until it closes, one per request.
fn read_answers(stdout: ChildStdout, with_content: bool, tx: Sender<Answer>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = Vec::new();
        match reader
            .by_ref()
            .take(HEADER_LIMIT)
            .read_until(b'\n', &mut line)
        {
            Ok(0) => return,
            Ok(_) if line.last() != Some(&b'\n') => {
                let _ = tx.send(Err("a header line without an end".to_string()));
                return;
            }
            Ok(_) => {}
            Err(error) => {
                let _ = tx.send(Err(error.to_string()));
                return;
            }
        }
        line.pop();
        let header = String::from_utf8_lossy(&line).into_owned();
        let answer = if header.ends_with(" missing") {
            Ok(Record::Missing)
        } else if header.ends_with(" ambiguous") {
            Ok(Record::Ambiguous)
        } else {
            match parse_header(&header) {
                Some((oid, kind, size)) => read_body(&mut reader, with_content, oid, kind, size),
                None => Err(format!("unrecognised header {header:?}")),
            }
        };
        let failed = answer.is_err();
        if tx.send(answer).is_err() || failed {
            return;
        }
    }
}

fn parse_header(header: &str) -> Option<(String, String, u64)> {
    let mut fields = header.split(' ');
    let (oid, kind, size) = (fields.next()?, fields.next()?, fields.next()?);
    if fields.next().is_some() || !matches!(oid.len(), 40 | 64) {
        return None;
    }
    if !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        oid.to_ascii_lowercase(),
        kind.to_string(),
        size.parse().ok()?,
    ))
}

fn read_body(
    reader: &mut BufReader<ChildStdout>,
    with_content: bool,
    oid: String,
    kind: String,
    size: u64,
) -> Answer {
    if !with_content {
        return Ok(Record::Object {
            oid,
            kind,
            size,
            body: None,
        });
    }
    // The body and its trailing newline are always consumed, so the next
    // answer starts where it should; one too large to keep is discarded as it
    // streams past.
    let fits = usize::try_from(size).is_ok_and(|s| s <= CONTENT_CAP);
    let body = if fits {
        let mut body = vec![0u8; size as usize];
        reader.read_exact(&mut body).map_err(|e| e.to_string())?;
        Some(body)
    } else {
        let copied = std::io::copy(&mut reader.by_ref().take(size), &mut std::io::sink())
            .map_err(|e| e.to_string())?;
        if copied != size {
            return Err("a body shorter than its header said".to_string());
        }
        None
    };
    let mut newline = [0u8; 1];
    reader.read_exact(&mut newline).map_err(|e| e.to_string())?;
    if newline[0] != b'\n' {
        return Err("a body not followed by a newline".to_string());
    }
    Ok(Record::Object {
        oid,
        kind,
        size,
        body,
    })
}

#[derive(Default)]
struct State {
    commits: HashMap<String, String>,
    identities: HashMap<(String, String), Result<String, HistoryRefusal>>,
    check: Option<Result<Lane, HistoryRefusal>>,
    content: Option<Result<Lane, HistoryRefusal>>,
}

/// The blobs of one analysis. See the module documentation.
pub struct BlobReader {
    program: OsString,
    repo: PathBuf,
    state: Mutex<State>,
}

impl std::fmt::Debug for BlobReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobReader")
            .field("program", &self.program)
            .field("repo", &self.repo)
            .finish_non_exhaustive()
    }
}

impl BlobReader {
    pub fn new(program: &OsStr, repo: &Path) -> Self {
        Self {
            program: program.to_os_string(),
            repo: repo.to_path_buf(),
            state: Mutex::new(State::default()),
        }
    }

    /// Whether this reader reads `repo` through `program`, so a caller handed
    /// one can tell whether it may use it.
    pub fn reads(&self, program: &OsStr, repo: &Path) -> bool {
        self.program == program && self.repo == repo
    }

    /// The commit `rev` names, as an object id. Asked of git once per `rev`.
    pub fn commit(&self, rev: &str) -> Result<String, HistoryRefusal> {
        if let Some(known) = self.lock().commits.get(rev) {
            return Ok(known.clone());
        }
        // Outside the lock: resolving may spawn, and nothing else needs the
        // memo meanwhile.
        let oid = commit_oid(&self.program, &self.repo, rev)?;
        let mut state = self.lock();
        if state.commits.len() >= MEMO_CAP {
            state.commits.clear();
        }
        state.commits.insert(rev.to_string(), oid.clone());
        Ok(oid)
    }

    /// The object id of `path` at `rev`.
    pub fn identity(&self, rev: &str, path: &str) -> Result<BlobIdentity, HistoryRefusal> {
        let commit = self.commit(rev)?;
        let mut state = self.lock();
        let key = (commit.clone(), path.to_string());
        if let Some(known) = state.identities.get(&key) {
            return known.clone().map(BlobIdentity::Blob);
        }
        let spec = request(&commit, path)?;
        let state = &mut *state;
        let lane = lane_of(&mut state.check, &self.program, &self.repo, false)?;
        let answer = lane.ask(&spec);
        let identity = blob_of(answer, rev, path).map(|(oid, _, _)| oid);
        remember(&mut state.identities, key, &identity);
        identity.map(BlobIdentity::Blob)
    }

    /// The text of `path` at `rev` and the object id of those bytes.
    pub fn read(&self, rev: &str, path: &str) -> Result<ResolvedBlob, HistoryRefusal> {
        let commit = self.commit(rev)?;
        let mut state = self.lock();
        let spec = request(&commit, path)?;
        let state = &mut *state;
        let lane = lane_of(&mut state.content, &self.program, &self.repo, true)?;
        let answer = lane.ask(&spec);
        let blob = blob_of(answer, rev, path);
        let identity = blob
            .as_ref()
            .map(|(oid, _, _)| oid.clone())
            .map_err(Clone::clone);
        remember(&mut state.identities, (commit, path.to_string()), &identity);
        let (oid, size, body) = blob?;
        let Some(body) = body else {
            return Err(HistoryRefusal::OutputCapped {
                what: format!("git cat-file ({size} bytes)"),
                cap: CONTENT_CAP,
            });
        };
        match String::from_utf8(body) {
            Ok(content) => Ok(ResolvedBlob {
                content,
                identity: BlobIdentity::Blob(oid),
            }),
            Err(_) => Err(HistoryRefusal::NotText {
                path: path.to_string(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// `<commit>:<path>`, or the refusal for a path one line cannot carry.
fn request(commit: &str, path: &str) -> Result<String, HistoryRefusal> {
    if path.contains(['\n', '\0']) {
        return Err(HistoryRefusal::GitRefused {
            what: "git cat-file".to_string(),
            stderr: format!(
                "the path {path:?} contains a newline or NUL, which a one-line request cannot carry"
            ),
        });
    }
    Ok(format!("{commit}:{path}"))
}

fn lane_of<'a>(
    slot: &'a mut Option<Result<Lane, HistoryRefusal>>,
    program: &OsStr,
    repo: &Path,
    with_content: bool,
) -> Result<&'a mut Lane, HistoryRefusal> {
    slot.get_or_insert_with(|| Lane::start(program, repo, with_content))
        .as_mut()
        .map_err(|refusal| refusal.clone())
}

/// A blob's id, size and body from an answer, or why it is not one.
fn blob_of(
    answer: Result<Record, HistoryRefusal>,
    rev: &str,
    path: &str,
) -> Result<(String, u64, Option<Vec<u8>>), HistoryRefusal> {
    let refused = |stderr: String| HistoryRefusal::GitRefused {
        what: "git cat-file".to_string(),
        stderr,
    };
    match answer? {
        Record::Missing => Err(refused(format!("path {path:?} does not exist in {rev}"))),
        Record::Ambiguous => Err(refused(format!("{path:?} at {rev} is ambiguous"))),
        Record::Object {
            oid,
            kind,
            size,
            body,
        } => {
            if kind == "blob" {
                Ok((oid, size, body))
            } else {
                Err(refused(format!(
                    "{path:?} at {rev} is a {kind}, not a file"
                )))
            }
        }
    }
}

/// Remembers an answer git gave about the repository, and not one about
/// the read: a deadline or a dead process may not repeat.
fn remember(
    memo: &mut HashMap<(String, String), Result<String, HistoryRefusal>>,
    key: (String, String),
    answer: &Result<String, HistoryRefusal>,
) {
    let durable = match answer {
        Ok(_) => true,
        Err(HistoryRefusal::GitRefused { .. }) => true,
        Err(_) => false,
    };
    if !durable {
        return;
    }
    if memo.len() >= MEMO_CAP {
        memo.clear();
    }
    memo.insert(key, answer.clone());
}
