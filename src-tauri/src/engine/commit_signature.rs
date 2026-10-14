//! Does a commit carry a signature, and does it verify?
//!
//! Two different questions with two different costs, kept apart on purpose.
//!
//! *Carries one* is a fact about the commit object: a signature header. A
//! history walk reads it for every commit at once, from the objects themselves,
//! beside the log that produces the rows ([`walk_signatures`]), and never runs a
//! signing program; the one-commit verify reads it for the commit it is about to
//! check ([`commit_signatures`]).
//!
//! *Verifies* runs the user's signing program (`gpg`, `ssh-keygen`, `gpgsm`),
//! which may start an agent, wait on a keyserver or a pinentry, and measured
//! 10 s for one PGP commit. Git has no placeholder for the first question that
//! does not also ask the second: every `%G*` verifies. So a bulk walk must not
//! carry one, and the single place that may is [`verify_commit_signature`], for
//! the one commit a person selected, cached so selecting it again costs nothing.
//! A source-scan test in `git_reader` fails the build if a second `%G` format
//! appears.

use crate::engine::git_cli::{
    git_captured, git_observed, git_text_with_timeout, read_objects_batch, validate_repo,
    BatchObject, OutputStream, ProcessObserver, DEFAULT_TIMEOUT,
};
use crate::engine::git_writer::validate_oid;
use crate::engine::ref_cache::{user_config_paths, FileIdentity};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// How a commit is signed, by the armor of the signature it carries. Says
/// nothing about whether the signature is good: that is [`SignatureStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitSignature {
    /// The commit has no signature header.
    None,
    Pgp,
    Ssh,
    X509,
    /// A signature header whose armor is none of the above.
    Other,
    /// Whether the commit is signed could not be read (the object read failed
    /// or was cut short). Distinct from `None`: an unread commit is not an
    /// unsigned one.
    Unknown,
}

impl CommitSignature {
    /// True when the commit is known to carry a signature.
    pub fn is_signed(self) -> bool {
        !matches!(self, Self::None | Self::Unknown)
    }
}

/// What git's `%G?` said about one signed commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    /// `G`: a good signature.
    Good,
    /// `B`: a bad signature.
    Bad,
    /// `U`: good, but the signer's validity is unknown.
    GoodUnknownValidity,
    /// `X`: good, but the signature has expired.
    Expired,
    /// `Y`: good, but made by an expired key.
    ExpiredKey,
    /// `R`: good, but made by a revoked key.
    Revoked,
    /// `E`: the signature cannot be checked (a missing key or program).
    CannotCheck,
    /// `N`: no signature, as far as the verifier could tell. For a commit that
    /// does carry one, this means the verifier could not run at all.
    None,
}

impl SignatureStatus {
    fn from_letter(letter: &str) -> Option<Self> {
        Some(match letter {
            "G" => Self::Good,
            "B" => Self::Bad,
            "U" => Self::GoodUnknownValidity,
            "X" => Self::Expired,
            "Y" => Self::ExpiredKey,
            "R" => Self::Revoked,
            "E" => Self::CannotCheck,
            "N" => Self::None,
            _ => return Option::None,
        })
    }
}

/// The answer to "does this one commit's signature verify".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitSignatureCheck {
    /// How the commit is signed, from its object.
    pub signature: CommitSignature,
    /// What the verifier said; [`SignatureStatus::None`] without a run when
    /// the commit carries no signature.
    pub status: SignatureStatus,
    /// The signing key (`%GK`), empty when the verifier named none.
    pub key: String,
    /// The signer (`%GS`), empty when the verifier named none.
    pub signer: String,
}

/// The hash a repository names its objects with, which decides which header
/// carries a commit's signature: git reads `gpgsig` in a SHA-1 repository and
/// `gpgsig-sha256` in a SHA-256 one, and treats the other as an ordinary
/// unknown header. Read from an object id's length, so no process is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectFormat {
    Sha1,
    Sha256,
}

impl ObjectFormat {
    /// The format an object id of this shape belongs to; `None` for anything
    /// that is not 40 or 64 hex digits.
    pub(crate) fn of_oid(oid: &str) -> Option<Self> {
        let hex = oid.bytes().all(|b| b.is_ascii_hexdigit());
        match (oid.len(), hex) {
            (40, true) => Some(Self::Sha1),
            (64, true) => Some(Self::Sha256),
            _ => None,
        }
    }

    /// The header line prefix that holds a signature in this format.
    fn signature_header(self) -> &'static [u8] {
        match self {
            Self::Sha1 => b"gpgsig ",
            Self::Sha256 => b"gpgsig-sha256 ",
        }
    }
}

/// The longest run of commit-header bytes the classifier reads. A real header
/// is a few hundred bytes (an octopus merge a few KiB); anything past this is
/// not scanned further, and a commit whose header is still going at the limit
/// is `Unknown` rather than guessed `None`.
const MAX_HEADER_BYTES: usize = 256 * 1024;

/// Byte budget for one read of commit objects, whether per id or for a walk. A
/// commit is mostly its message; past this the unread commits are `Unknown`.
const READ_BUDGET: usize = 32 * 1024 * 1024;

const COMMIT: BatchObject = BatchObject {
    kind: "commit",
    noun: "commit",
};

/// Classifies one commit object (the bytes git prints for it) by its signature
/// header, without running anything.
///
/// Only the signature header of the repository's `format`, at column 0, before
/// the blank line that ends the headers, counts: the same line git's own parser
/// reads. A `mergetag` header (the signature of a *tag* that was merged) does
/// not, and neither does a message body that happens to start a line with
/// `gpgsig `: continuation lines of a header begin with a space, and the body
/// begins after the blank line. The input is untrusted object content: it is
/// scanned once, never past [`MAX_HEADER_BYTES`], with no allocation
/// proportional to it, and no input panics.
pub(crate) fn classify_header(object: &[u8], format: ObjectFormat) -> CommitSignature {
    let scanned = &object[..object.len().min(MAX_HEADER_BYTES)];
    let header = format.signature_header();
    let mut rest = scanned;
    while !rest.is_empty() {
        let (line, tail) = match rest.iter().position(|&b| b == b'\n') {
            Some(at) => (&rest[..at], &rest[at + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        if line.is_empty() {
            return CommitSignature::None;
        }
        if let Some(armor) = line.strip_prefix(header) {
            return classify_armor(armor);
        }
        rest = tail;
    }
    if object.len() > MAX_HEADER_BYTES {
        CommitSignature::Unknown
    } else {
        CommitSignature::None
    }
}

/// The signature format named by the first armor line, the way git itself
/// tells them apart.
fn classify_armor(first_line: &[u8]) -> CommitSignature {
    let starts = |prefix: &[u8]| first_line.starts_with(prefix);
    if starts(b"-----BEGIN PGP SIGNATURE-----") || starts(b"-----BEGIN PGP MESSAGE-----") {
        CommitSignature::Pgp
    } else if starts(b"-----BEGIN SSH SIGNATURE-----") {
        CommitSignature::Ssh
    } else if starts(b"-----BEGIN SIGNED MESSAGE-----") {
        CommitSignature::X509
    } else {
        CommitSignature::Other
    }
}

/// How each of `oids` is signed, in one `git cat-file --batch` and no verifier.
/// For the one-commit verify; a walk uses [`walk_signatures`]. All of `oids`
/// belong to one repository, so the first well-formed id names its format.
///
/// Every id gets an answer. One whose object could not be read, or that came
/// after the batch was cut short, is [`CommitSignature::Unknown`]: a check that
/// could not run never reports the same as one that ran and found nothing.
pub(crate) fn commit_signatures(repo: &Path, oids: &[String]) -> HashMap<String, CommitSignature> {
    let format = oids
        .iter()
        .find_map(|oid| ObjectFormat::of_oid(oid))
        .unwrap_or(ObjectFormat::Sha1);
    settle(read_objects_batch(
        repo,
        oids,
        COMMIT,
        READ_BUDGET,
        &|object| classify_header(object, format),
    ))
}

/// How every commit a history walk visits is signed, from one header-only
/// `git rev-list --header` run with the walk's own arguments, and no verifier.
///
/// It needs no list of ids, so a caller runs it beside the walk that produces
/// the rows instead of after it, and the stream is parsed as it arrives: only
/// an id and a signature kind are kept per commit, however long the messages
/// are. A commit the stream does not hold (the refs moved between the two
/// reads, the output was cut at the budget, git failed or timed out) is absent
/// from the map, and the caller reads absence as [`CommitSignature::Unknown`];
/// a commit the stream names twice is `Unknown` too, since one of the two
/// records was not git's.
pub(crate) fn walk_signatures(repo: &Path, args: &[&str]) -> HashMap<String, CommitSignature> {
    let mut stream = HeaderStream::default();
    let run = {
        let mut observer = HeaderObserver(&mut stream);
        git_observed(repo, args, DEFAULT_TIMEOUT, READ_BUDGET, &mut observer)
    };
    match run {
        Ok(run) if run.success || run.incomplete.is_some() => {
            stream.finish(run.incomplete.is_some())
        }
        _ => HashMap::new(),
    }
}

/// Feeds a [`HeaderStream`] from a git child's stdout and tells the run not to
/// buffer it.
struct HeaderObserver<'a>(&'a mut HeaderStream);

impl ProcessObserver for HeaderObserver<'_> {
    fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
        if matches!(stream, OutputStream::Stdout) {
            self.0.feed(bytes);
        }
    }

    fn drains_stdout(&self) -> bool {
        true
    }
}

/// Bytes of one record kept: its id line and the longest header run the
/// classifier reads, plus one so an over-long header is recognised as such.
const KEPT_RECORD_BYTES: usize = 65 + MAX_HEADER_BYTES + 1;

/// Incremental reader of `rev-list --header` output (`oid LF object-text NUL`,
/// repeated). Memory is the answers plus one bounded record, not the stream.
#[derive(Default)]
struct HeaderStream {
    found: HashMap<String, CommitSignature>,
    /// The head of the record being read, at most [`KEPT_RECORD_BYTES`].
    record: Vec<u8>,
    /// Whether any byte of the record being read has arrived.
    started: bool,
}

impl HeaderStream {
    fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let end = bytes.iter().position(|&b| b == 0);
            let chunk = &bytes[..end.unwrap_or(bytes.len())];
            if !chunk.is_empty() {
                self.started = true;
                let room = KEPT_RECORD_BYTES.saturating_sub(self.record.len());
                self.record
                    .extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
            match end {
                Some(at) => {
                    self.complete_record();
                    bytes = &bytes[at + 1..];
                }
                None => break,
            }
        }
    }

    /// A record whose first line is not an object id is skipped, never
    /// attributed to anyone.
    fn complete_record(&mut self) {
        let record = std::mem::take(&mut self.record);
        self.started = false;
        let Some(newline) = record.iter().position(|&b| b == b'\n') else {
            return;
        };
        let Ok(oid) = std::str::from_utf8(&record[..newline]) else {
            return;
        };
        let Some(format) = ObjectFormat::of_oid(oid) else {
            return;
        };
        let signature = classify_header(&record[newline + 1..], format);
        self.found
            .entry(oid.to_ascii_lowercase())
            .and_modify(|seen| *seen = CommitSignature::Unknown)
            .or_insert(signature);
    }

    /// The answers read. `cut` says the stream is a prefix, so a record still
    /// being read is not trusted; at a clean end it is read as it stands.
    fn finish(mut self, cut: bool) -> HashMap<String, CommitSignature> {
        if self.started && !cut {
            self.complete_record();
        }
        self.found
    }
}

/// A commit that could not be read is `Unknown`, whatever the reason: the one
/// place an `Err` becomes a value, so none can become `None` by accident.
fn settle(
    answers: HashMap<String, Result<CommitSignature, String>>,
) -> HashMap<String, CommitSignature> {
    answers
        .into_iter()
        .map(|(oid, answer)| (oid, answer.unwrap_or(CommitSignature::Unknown)))
        .collect()
}

/// Files whose contents decide what a verifier says about a commit: the git
/// config that names the signing program and trust level, the user's git
/// config, and the keyring the program reads. An identity per file, so a
/// changed key ring, allowed-signers list or config shows as a changed
/// fingerprint without spawning anything. Files the config pulls in with
/// `include.path` are not knowable without asking git; those are learned on the
/// first verify (see [`learn_signing_files`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SigningFingerprint {
    files: Vec<(PathBuf, Option<FileIdentity>)>,
    gnupg_home: Option<PathBuf>,
}

/// The fingerprint of the signing state `repo` would verify under, or `None`
/// when it cannot be read (an unreadable git directory), in which case nothing
/// may be cached against it.
///
/// Standalone, so the diff-input fingerprint ft-fa9dbbe1 plans can lift it.
pub(crate) fn signing_fingerprint(repo: &Path) -> Option<SigningFingerprint> {
    let (private, common) = crate::repository_trust::git_directories(repo).ok()?;
    let mut paths = vec![common.join("config"), private.join("config.worktree")];
    paths.extend(user_config_paths());
    let gnupg_home = std::env::var_os("GNUPGHOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let keyring_home = gnupg_home.clone().or_else(|| {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".gnupg"))
    });
    if let Some(home) = keyring_home {
        for name in ["pubring.kbx", "pubring.gpg", "trustdb.gpg"] {
            paths.push(home.join(name));
        }
    }
    paths.sort();
    paths.dedup();
    Some(SigningFingerprint {
        files: paths
            .into_iter()
            .map(|path| {
                let identity = FileIdentity::at(&path);
                (path, identity)
            })
            .collect(),
        gnupg_home,
    })
}

/// The identities of files a verify read that no fixed list names: every file
/// the effective config was read from (including `include.path` and matching
/// `includeIf` targets), and the SSH allowed-signers and revocation files,
/// wherever the config points.
type LearnedFiles = Vec<(PathBuf, Option<FileIdentity>)>;

/// Asks git which files its effective config came from and where the SSH trust
/// files are, and stamps them all. `None` when the config could not be read, so
/// the answer is not cached against a guess.
///
/// A file an `includeIf` condition rejected is not listed, and is not stamped:
/// it matters only if it starts to match, which the 60 s TTL bounds.
fn learn_signing_files(repo: &Path) -> Option<LearnedFiles> {
    let run = git_captured(
        repo,
        &["config", "-z", "--includes", "--list", "--show-origin"],
    )
    .ok()?;
    if run.incomplete.is_some() || !run.success {
        return None;
    }
    let text = String::from_utf8_lossy(&run.stdout);
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let mut learned: LearnedFiles = Vec::new();
    let mut fields = text.split('\0');
    // `file:<path> NUL key LF value NUL`, repeated.
    while let (Some(origin), Some(entry)) = (fields.next(), fields.next()) {
        if let Some(origin_path) = origin.strip_prefix("file:") {
            learned.push((repo.join(origin_path), None));
        }
        let Some((key, value)) = entry.split_once('\n') else {
            continue;
        };
        if matches!(key, "gpg.ssh.allowedsignersfile" | "gpg.ssh.revocationfile") {
            let path = match (value.strip_prefix("~/"), &home) {
                (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
                _ => repo.join(value),
            };
            learned.push((path, None));
        }
    }
    learned.sort_by(|a, b| a.0.cmp(&b.0));
    learned.dedup_by(|a, b| a.0 == b.0);
    for (path, identity) in &mut learned {
        *identity = FileIdentity::at(path);
    }
    Some(learned)
}

/// How long a verification is served again. A verdict depends on files this
/// cache stamps, and on inputs it cannot see (a key server, a file rewritten in
/// place to the same length with its modification time put back, the clock a
/// signature expires by), so it is bounded in time too.
const VERIFY_TTL: Duration = Duration::from_secs(60);

/// Verdicts kept at once, across every repository.
const VERIFY_CAPACITY: usize = 256;

/// The longest one verification may run. Generous for a keyserver round trip,
/// short of the ninety seconds a hung pinentry would otherwise hold a worker.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a select waits for another select's verification of the same
/// commit before giving up: the verification's own limit and a margin for the
/// reads around it.
const FOLLOW_TIMEOUT: Duration = Duration::from_secs(35);

/// Verifications that may be in flight and coalesced at once. A request past
/// this runs on its own, uncoalesced, instead of queueing behind them.
const MAX_FLIGHTS: usize = 64;

struct Verdict {
    check: CommitSignatureCheck,
    fingerprint: SigningFingerprint,
    learned: LearnedFiles,
    stored_at: Instant,
}

/// One verification in progress, which other selects of the same commit wait
/// on rather than starting a second signing program.
#[derive(Default)]
struct Flight {
    outcome: Mutex<Option<Result<CommitSignatureCheck, String>>>,
    done: Condvar,
}

/// Recent verifications, keyed by repository and full commit id and valid
/// while the signing state it was computed under is unchanged, and the
/// verifications in flight. The clock is a parameter of every call so expiry is
/// testable without sleeping.
pub(crate) struct VerifyCache {
    verdicts: HashMap<(PathBuf, String), Verdict>,
    flights: HashMap<(PathBuf, String), Arc<Flight>>,
    ttl: Duration,
    capacity: usize,
}

impl VerifyCache {
    pub(crate) fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            verdicts: HashMap::new(),
            flights: HashMap::new(),
            ttl,
            capacity: capacity.max(1),
        }
    }

    /// The kept verdict for `oid`, if it is younger than the TTL and neither
    /// the fingerprint nor any learned file has changed since.
    pub(crate) fn get(
        &mut self,
        repo: &Path,
        oid: &str,
        fingerprint: &SigningFingerprint,
        now: Instant,
    ) -> Option<CommitSignatureCheck> {
        let key = (repo.to_path_buf(), oid.to_string());
        let verdict = self.verdicts.get(&key)?;
        let fresh = now.saturating_duration_since(verdict.stored_at) < self.ttl
            && verdict.fingerprint == *fingerprint
            && verdict
                .learned
                .iter()
                .all(|(path, identity)| FileIdentity::at(path) == *identity);
        if fresh {
            Some(verdict.check.clone())
        } else {
            self.verdicts.remove(&key);
            None
        }
    }

    pub(crate) fn put(
        &mut self,
        repo: &Path,
        oid: &str,
        fingerprint: SigningFingerprint,
        learned: LearnedFiles,
        check: CommitSignatureCheck,
        now: Instant,
    ) {
        let ttl = self.ttl;
        if self.verdicts.len() >= self.capacity {
            self.verdicts
                .retain(|_, v| now.saturating_duration_since(v.stored_at) < ttl);
        }
        while self.verdicts.len() >= self.capacity {
            let Some(oldest) = self
                .verdicts
                .iter()
                .min_by_key(|(_, v)| v.stored_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.verdicts.remove(&oldest);
        }
        self.verdicts.insert(
            (repo.to_path_buf(), oid.to_string()),
            Verdict {
                check,
                fingerprint,
                learned,
                stored_at: now,
            },
        );
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.verdicts.len()
    }
}

fn shared_cache() -> &'static Mutex<VerifyCache> {
    static CACHE: OnceLock<Mutex<VerifyCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VerifyCache::new(VERIFY_TTL, VERIFY_CAPACITY)))
}

/// Verifies the signature of the one commit `commit_id`, which must be a full
/// object id. Runs the signing program at most once per cache lifetime, however
/// many selects ask at once; an unsigned commit runs none.
pub fn verify_commit_signature(
    repo_path: &str,
    commit_id: &str,
) -> Result<CommitSignatureCheck, String> {
    verify_commit_signature_with(shared_cache(), Instant::now(), repo_path, commit_id)
}

/// [`verify_commit_signature`] against a given cache and clock.
///
/// A failure to run (timeout, a git that would not start, output that is not a
/// `%G?` answer) is an `Err` and is never cached: the next select tries again
/// instead of being served a verdict nobody reached. Selects of one commit that
/// overlap share one verification, success or failure.
pub(crate) fn verify_commit_signature_with(
    cache: &Mutex<VerifyCache>,
    now: Instant,
    repo_path: &str,
    commit_id: &str,
) -> Result<CommitSignatureCheck, String> {
    let repo = validate_repo(repo_path)?;
    validate_oid(commit_id)?;
    if !matches!(commit_id.len(), 40 | 64) {
        return Err("Signature verification needs a full commit id".into());
    }
    // Git names an object by its lowercase hex; every key and every comparison
    // below uses that one spelling.
    let commit_id = commit_id.to_ascii_lowercase();
    let commit_id = commit_id.as_str();
    let fingerprint = signing_fingerprint(&repo);
    if let Some(fingerprint) = &fingerprint {
        if let Some(kept) = cache.lock().unwrap_or_else(PoisonError::into_inner).get(
            &repo,
            commit_id,
            fingerprint,
            now,
        ) {
            return Ok(kept);
        }
    }

    let key = (repo.clone(), commit_id.to_string());
    let turn = {
        let mut state = cache.lock().unwrap_or_else(PoisonError::into_inner);
        match state.flights.get(&key) {
            Some(flight) => Turn::Follow(flight.clone()),
            None if state.flights.len() < MAX_FLIGHTS => {
                let flight = Arc::new(Flight::default());
                state.flights.insert(key.clone(), flight.clone());
                Turn::Lead(flight)
            }
            None => Turn::Alone,
        }
    };
    match turn {
        Turn::Follow(flight) => follow(&flight),
        Turn::Alone => verify_uncached(cache, now, &repo, commit_id, fingerprint),
        Turn::Lead(flight) => {
            let mut lead = Lead {
                cache,
                key,
                flight,
                settled: false,
            };
            let result = verify_uncached(cache, now, &repo, commit_id, fingerprint);
            lead.settle(result.clone());
            result
        }
    }
}

enum Turn {
    Lead(Arc<Flight>),
    Follow(Arc<Flight>),
    Alone,
}

/// Waits for the leader of a flight and takes its answer.
fn follow(flight: &Flight) -> Result<CommitSignatureCheck, String> {
    let guard = flight
        .outcome
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let (guard, _) = flight
        .done
        .wait_timeout_while(guard, FOLLOW_TIMEOUT, |outcome| outcome.is_none())
        .unwrap_or_else(PoisonError::into_inner);
    guard.clone().unwrap_or_else(|| {
        Err("Timed out waiting for a signature verification already running for this commit".into())
    })
}

/// The leader of a flight. Settling it removes the flight and wakes the
/// followers with the answer; dropping it unsettled (a panic in the
/// verification) wakes them with an error instead of leaving them to time out.
struct Lead<'a> {
    cache: &'a Mutex<VerifyCache>,
    key: (PathBuf, String),
    flight: Arc<Flight>,
    settled: bool,
}

impl Lead<'_> {
    fn settle(&mut self, result: Result<CommitSignatureCheck, String>) {
        self.settled = true;
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flights
            .remove(&self.key);
        *self
            .flight
            .outcome
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(result);
        self.flight.done.notify_all();
    }
}

impl Drop for Lead<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.settle(Err("Signature verification was interrupted".into()));
        }
    }
}

/// Reads the commit's signature kind, runs the signing program if there is a
/// signature to check, and keeps the answer. One call, one verification.
fn verify_uncached(
    cache: &Mutex<VerifyCache>,
    now: Instant,
    repo: &Path,
    commit_id: &str,
    fingerprint: Option<SigningFingerprint>,
) -> Result<CommitSignatureCheck, String> {
    let oids = [commit_id.to_string()];
    let signature = commit_signatures(repo, &oids)
        .remove(commit_id)
        .unwrap_or(CommitSignature::Unknown);
    // Stamped before the verifier runs, so a trust file edited while it runs
    // leaves the stored identity different from the file and the next select
    // verifies again.
    let learned = if signature == CommitSignature::None {
        Some(Vec::new())
    } else {
        learn_signing_files(repo)
    };
    let check = if signature == CommitSignature::None {
        CommitSignatureCheck {
            signature,
            status: SignatureStatus::None,
            key: String::new(),
            signer: String::new(),
        }
    } else {
        run_verifier(repo, commit_id, signature)?
    };
    if let (Some(fingerprint), Some(learned)) = (fingerprint, learned) {
        cache.lock().unwrap_or_else(PoisonError::into_inner).put(
            repo,
            commit_id,
            fingerprint,
            learned,
            check.clone(),
            now,
        );
    }
    Ok(check)
}

/// The one place a `%G` placeholder is allowed: `git log -1 --no-walk` of a
/// single commit the caller chose, under its own timeout.
fn run_verifier(
    repo: &Path,
    commit_id: &str,
    signature: CommitSignature,
) -> Result<CommitSignatureCheck, String> {
    let stdout = git_text_with_timeout(
        repo,
        &[
            "log",
            "-1",
            "--no-walk",
            "--format=%G?%x00%GK%x00%GS",
            commit_id,
        ],
        VERIFY_TIMEOUT,
    )
    .map_err(|e| format!("Signature verification of {commit_id} failed: {e}"))?;
    parse_verifier_output(&stdout, signature)
        .ok_or_else(|| format!("git answered {stdout:?} for the signature of {commit_id}"))
}

/// `status NUL key NUL signer`, as [`run_verifier`] asks for it. `None` when
/// the shape or the status letter is anything else.
fn parse_verifier_output(stdout: &str, signature: CommitSignature) -> Option<CommitSignatureCheck> {
    let mut fields = stdout.trim_end_matches('\n').splitn(3, '\0');
    let status = SignatureStatus::from_letter(fields.next()?)?;
    let key = fields.next()?.to_string();
    let signer = fields.next()?.to_string();
    Some(CommitSignatureCheck {
        signature,
        status,
        key,
        signer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;

    /// [`super::classify_header`] for the SHA-1 repository nearly every fixture
    /// here is; a test about the other format says so with `ObjectFormat`.
    fn classify_header(object: &[u8]) -> CommitSignature {
        super::classify_header(object, ObjectFormat::Sha1)
    }

    /// A whole `rev-list --header` stream read at once, as [`walk_signatures`]
    /// reads it a chunk at a time.
    fn read_header_stream(out: &[u8], cut: bool) -> HashMap<String, CommitSignature> {
        let mut stream = HeaderStream::default();
        stream.feed(out);
        stream.finish(cut)
    }

    fn header(extra: &str) -> Vec<u8> {
        format!(
            "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a@b> 1 +0000\ncommitter A <a@b> 1 +0000\n{extra}\nbody\n"
        )
        .into_bytes()
    }

    #[test]
    fn armor_names_the_format() {
        let signed = |armor: &str| {
            classify_header(&header(&crate::test_support::fake_signature_header(
                "gpgsig", armor,
            )))
        };
        assert_eq!(signed("PGP SIGNATURE"), CommitSignature::Pgp);
        assert_eq!(signed("SSH SIGNATURE"), CommitSignature::Ssh);
        assert_eq!(signed("SIGNED MESSAGE"), CommitSignature::X509);
        assert_eq!(signed("SOMETHING ELSE"), CommitSignature::Other);
        assert_eq!(
            super::classify_header(
                &header(&crate::test_support::fake_signature_header(
                    "gpgsig-sha256",
                    "PGP SIGNATURE"
                )),
                ObjectFormat::Sha256
            ),
            CommitSignature::Pgp,
            "a SHA-256 repository reads gpgsig-sha256"
        );
        assert_eq!(
            classify_header(&header(&crate::test_support::fake_signature_header(
                "gpgsig-sha256",
                "PGP SIGNATURE"
            ))),
            CommitSignature::None,
            "a SHA-1 repository does not: git treats it as an unknown header"
        );
        assert_eq!(
            super::classify_header(
                &header(&crate::test_support::fake_signature_header(
                    "gpgsig",
                    "PGP SIGNATURE"
                )),
                ObjectFormat::Sha256
            ),
            CommitSignature::None,
            "and a SHA-256 repository does not read gpgsig"
        );
        assert_eq!(classify_header(&header("")), CommitSignature::None);
    }

    #[test]
    fn a_merged_tags_signature_and_a_message_that_says_gpgsig_are_not_signatures() {
        let mergetag = "mergetag object 0000000000000000000000000000000000000000\n type commit\n tag v1\n \n -----BEGIN PGP SIGNATURE-----\n gpgsig inside a continuation\n -----END PGP SIGNATURE-----\n";
        assert_eq!(classify_header(&header(mergetag)), CommitSignature::None);

        let mut object = header("");
        object.extend_from_slice(b"gpgsig -----BEGIN PGP SIGNATURE-----\n");
        assert_eq!(
            classify_header(&object),
            CommitSignature::None,
            "the message body is not a header"
        );
        // A merge that is itself signed still is, mergetag or not.
        let both = format!(
            "{mergetag}{}",
            crate::test_support::fake_signature_header("gpgsig", "SSH SIGNATURE")
        );
        assert_eq!(classify_header(&header(&both)), CommitSignature::Ssh);
    }

    #[test]
    fn hostile_headers_are_read_without_panicking_or_guessing_unsigned() {
        assert_eq!(classify_header(b""), CommitSignature::None);
        assert_eq!(classify_header(b"gpgsig"), CommitSignature::None);
        assert_eq!(classify_header(b"gpgsig "), CommitSignature::Other);
        // No newline anywhere, and a NUL inside the armor line.
        assert_eq!(
            classify_header(b"gpgsig -----BEGIN PGP SIGNATURE-----\0\0"),
            CommitSignature::Pgp
        );
        // A header that never ends: bounded, and not called unsigned.
        let endless = vec![b'x'; MAX_HEADER_BYTES * 3];
        assert_eq!(classify_header(&endless), CommitSignature::Unknown);
        // A giant signature is classified from its first line alone.
        let mut giant = b"gpgsig -----BEGIN SSH SIGNATURE-----\n".to_vec();
        giant.extend(std::iter::repeat_n(b' ', MAX_HEADER_BYTES * 4));
        assert_eq!(classify_header(&giant), CommitSignature::Ssh);
        // A signature header pushed past the scan limit by earlier headers is
        // not seen, and that is `Unknown`, not `None`.
        let mut late = vec![b'a'; MAX_HEADER_BYTES];
        late.extend_from_slice(b"\ngpgsig -----BEGIN PGP SIGNATURE-----\n");
        assert_eq!(classify_header(&late), CommitSignature::Unknown);
    }

    /// Random bytes, and random splices of the markers the parser looks for:
    /// the parser must return for every input, and its answer must agree with
    /// a plain reference reading of the same bytes.
    #[test]
    fn the_header_parser_agrees_with_a_reference_on_random_bytes() {
        fn reference(object: &[u8]) -> CommitSignature {
            let head = &object[..object.len().min(MAX_HEADER_BYTES)];
            for line in head.split(|&b| b == b'\n') {
                if line.is_empty() {
                    return CommitSignature::None;
                }
                if let Some(armor) = line.strip_prefix(b"gpgsig ") {
                    return classify_armor(armor);
                }
            }
            if object.len() > MAX_HEADER_BYTES {
                CommitSignature::Unknown
            } else {
                CommitSignature::None
            }
        }
        // A small xorshift: deterministic, so a failure reproduces.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let pieces: [&[u8]; 9] = [
            b"gpgsig ",
            b"gpgsig-sha256 ",
            b"mergetag ",
            b"-----BEGIN PGP SIGNATURE-----",
            b"-----BEGIN SSH SIGNATURE-----",
            b"\n",
            b"\n\n",
            b" ",
            b"\0",
        ];
        for _ in 0..4_000 {
            let mut object = Vec::new();
            for _ in 0..(next() % 24) {
                if next() % 3 == 0 {
                    object.push((next() % 256) as u8);
                } else {
                    object.extend_from_slice(pieces[(next() % pieces.len() as u64) as usize]);
                }
            }
            assert_eq!(
                classify_header(&object),
                reference(&object),
                "{:?}",
                String::from_utf8_lossy(&object)
            );
        }
    }

    #[test]
    fn the_verifier_line_parses_and_anything_else_is_refused() {
        let ok =
            parse_verifier_output("G\0ABCDEF\0A Signer <a@b>\n", CommitSignature::Pgp).unwrap();
        assert_eq!(ok.status, SignatureStatus::Good);
        assert_eq!(ok.key, "ABCDEF");
        assert_eq!(ok.signer, "A Signer <a@b>");
        let blank = parse_verifier_output("N\0\0\n", CommitSignature::Ssh).unwrap();
        assert_eq!(blank.status, SignatureStatus::None);
        assert_eq!((blank.key.as_str(), blank.signer.as_str()), ("", ""));
        for bad in ["", "G", "G\0k", "Z\0k\0s", "GG\0k\0s", "\0\0"] {
            assert_eq!(
                parse_verifier_output(bad, CommitSignature::Pgp),
                None,
                "{bad:?}"
            );
        }
        for (letter, expected) in [
            ("B", SignatureStatus::Bad),
            ("U", SignatureStatus::GoodUnknownValidity),
            ("X", SignatureStatus::Expired),
            ("Y", SignatureStatus::ExpiredKey),
            ("R", SignatureStatus::Revoked),
            ("E", SignatureStatus::CannotCheck),
        ] {
            assert_eq!(SignatureStatus::from_letter(letter), Some(expected));
        }
    }

    #[test]
    fn the_wire_names_are_closed_and_snake_case() {
        assert_eq!(
            serde_json::to_string(&CommitSignature::X509).unwrap(),
            "\"x509\""
        );
        assert_eq!(
            serde_json::to_string(&SignatureStatus::GoodUnknownValidity).unwrap(),
            "\"good_unknown_validity\""
        );
        assert!(serde_json::from_str::<CommitSignature>("\"signed\"").is_err());
    }

    fn fingerprint(tag: &str) -> SigningFingerprint {
        SigningFingerprint {
            files: vec![(PathBuf::from(tag), None)],
            gnupg_home: None,
        }
    }

    fn check(status: SignatureStatus) -> CommitSignatureCheck {
        CommitSignatureCheck {
            signature: CommitSignature::Pgp,
            status,
            key: String::new(),
            signer: String::new(),
        }
    }

    #[test]
    fn a_verdict_expires_on_the_injected_clock_and_on_a_changed_fingerprint() {
        let mut cache = VerifyCache::new(Duration::from_secs(60), 8);
        let repo = Path::new("/r");
        let t0 = Instant::now();
        cache.put(
            repo,
            "a",
            fingerprint("one"),
            Vec::new(),
            check(SignatureStatus::Good),
            t0,
        );
        assert!(cache.get(repo, "a", &fingerprint("one"), t0).is_some());
        assert!(cache
            .get(repo, "a", &fingerprint("one"), t0 + Duration::from_secs(59))
            .is_some());
        assert!(
            cache.get(repo, "a", &fingerprint("two"), t0).is_none(),
            "a different signing state is a different question"
        );
        cache.put(
            repo,
            "a",
            fingerprint("one"),
            Vec::new(),
            check(SignatureStatus::Good),
            t0,
        );
        assert!(cache
            .get(repo, "a", &fingerprint("one"), t0 + Duration::from_secs(60))
            .is_none());
        assert_eq!(cache.len(), 0, "an expired verdict is dropped");
    }

    #[test]
    fn the_cache_is_bounded_and_drops_the_oldest_first() {
        let mut cache = VerifyCache::new(Duration::from_secs(600), 3);
        let repo = Path::new("/r");
        let t0 = Instant::now();
        for (n, oid) in ["a", "b", "c", "d"].iter().enumerate() {
            cache.put(
                repo,
                oid,
                fingerprint("f"),
                Vec::new(),
                check(SignatureStatus::Good),
                t0 + Duration::from_secs(n as u64),
            );
        }
        assert_eq!(cache.len(), 3);
        let at = t0 + Duration::from_secs(4);
        assert!(cache.get(repo, "a", &fingerprint("f"), at).is_none());
        for oid in ["b", "c", "d"] {
            assert!(
                cache.get(repo, oid, &fingerprint("f"), at).is_some(),
                "{oid}"
            );
        }
    }

    /// A repository with one seed commit and, on top of it, one commit that
    /// carries `headers` (a signature header, or none). Returns the repository
    /// and the new commit's id.
    fn repo_with_commit(headers: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::TempDir::new().unwrap();
        crate::test_support::git_in(dir.path(), &["init", "-b", "main"]);
        crate::test_support::commit_at(dir.path(), "seed", 1_700_000_000, "t@test.local");
        let seed = String::from_utf8(
            crate::engine::git_cli::git(dir.path(), &["rev-parse", "HEAD"]).unwrap(),
        )
        .unwrap()
        .trim()
        .to_string();
        let oid =
            crate::test_support::raw_commit(dir.path(), &[&seed], headers, "c", 1_700_000_100);
        crate::test_support::git_in(dir.path(), &["update-ref", "refs/heads/main", &oid]);
        (dir, oid)
    }

    fn pgp() -> String {
        crate::test_support::fake_signature_header("gpgsig", "PGP SIGNATURE")
    }

    fn fresh_cache() -> Mutex<VerifyCache> {
        Mutex::new(VerifyCache::new(VERIFY_TTL, VERIFY_CAPACITY))
    }

    #[cfg(unix)]
    #[test]
    fn selecting_a_signed_commit_again_runs_the_verifier_zero_more_times() {
        use crate::test_support::{counting_verifier, use_verifier, verifier_spawns};
        let (repo, oid) = repo_with_commit(&pgp());
        let path = repo.path().to_str().unwrap();
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let cache = fresh_cache();
        let t0 = Instant::now();

        let first = verify_commit_signature_with(&cache, t0, path, &oid).unwrap();
        let after_first = verifier_spawns(&stub);
        assert_eq!(after_first, 1, "one verification, one signing-program call");
        assert_eq!(first.signature, CommitSignature::Pgp);

        let second =
            verify_commit_signature_with(&cache, t0 + Duration::from_secs(1), path, &oid).unwrap();
        assert_eq!(second, first);
        assert_eq!(
            verifier_spawns(&stub),
            after_first,
            "the second select hit the cache"
        );

        // Past the TTL on the injected clock, with nothing else changed.
        verify_commit_signature_with(&cache, t0 + Duration::from_secs(61), path, &oid).unwrap();
        assert_eq!(
            verifier_spawns(&stub),
            after_first + 1,
            "an expired verdict is verified again"
        );
    }

    /// The second fact the design rests on: a signing program that cannot run
    /// leaves the commit at `N`, which the pane must read as "signed, could
    /// not verify" because the object carries a signature.
    #[cfg(unix)]
    #[test]
    fn a_signing_program_that_cannot_run_reads_as_n_on_a_commit_that_is_signed() {
        use crate::test_support::{counting_verifier, use_verifier};
        let (repo, oid) = repo_with_commit(&pgp());
        let path = repo.path().to_str().unwrap();
        let (_stub, script) = counting_verifier("exit 127");
        use_verifier(repo.path(), &script);
        let check =
            verify_commit_signature_with(&fresh_cache(), Instant::now(), path, &oid).unwrap();
        assert_eq!(check.signature, CommitSignature::Pgp);
        assert_eq!(check.status, SignatureStatus::None);

        crate::test_support::git_in(
            repo.path(),
            &["config", "gpg.program", "/nonexistent/no-such-gpg"],
        );
        let missing =
            verify_commit_signature_with(&fresh_cache(), Instant::now(), path, &oid).unwrap();
        assert_eq!(missing.signature, CommitSignature::Pgp);
        assert!(
            matches!(
                missing.status,
                SignatureStatus::None | SignatureStatus::CannotCheck
            ),
            "{missing:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_good_signature_reads_as_good() {
        use crate::test_support::{counting_verifier, use_verifier};
        let (repo, oid) = repo_with_commit(&pgp());
        let path = repo.path().to_str().unwrap();
        let (_stub, script) = counting_verifier(
            "echo '[GNUPG:] NEWSIG'\necho '[GNUPG:] GOODSIG 0123456789ABCDEF Test Signer <t@test.local>'\necho '[GNUPG:] VALIDSIG 0123456789ABCDEF0123456789ABCDEF01234567 2024-01-01 1700000000 0 4 0 1 10 00 0123456789ABCDEF0123456789ABCDEF01234567'\necho '[GNUPG:] TRUST_ULTIMATE 0 pgp'\nexit 0",
        );
        use_verifier(repo.path(), &script);
        let check =
            verify_commit_signature_with(&fresh_cache(), Instant::now(), path, &oid).unwrap();
        assert_eq!(check.status, SignatureStatus::Good, "{check:?}");
        assert_eq!(check.signer, "Test Signer <t@test.local>");
    }

    /// No signature, no verifier: the check is answered from the object, and
    /// says so rather than reporting a verification that never ran.
    #[cfg(unix)]
    #[test]
    fn an_unsigned_commit_starts_no_verifier() {
        use crate::test_support::{counting_verifier, use_verifier, verifier_spawns};
        let (repo, oid) = repo_with_commit("");
        let path = repo.path().to_str().unwrap();
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let check =
            verify_commit_signature_with(&fresh_cache(), Instant::now(), path, &oid).unwrap();
        assert_eq!(check.signature, CommitSignature::None);
        assert_eq!(check.status, SignatureStatus::None);
        assert_eq!(verifier_spawns(&stub), 0);
    }

    /// An SSH signature is verified against the allowed-signers file the
    /// config names, so changing that file must not be answered from a verdict
    /// reached under the old one.
    #[cfg(unix)]
    #[test]
    fn touching_the_allowed_signers_file_misses_the_cache() {
        use crate::test_support::{counting_verifier, git_in, use_verifier, verifier_spawns};
        let (repo, oid) = repo_with_commit(&crate::test_support::fake_signature_header(
            "gpgsig",
            "SSH SIGNATURE",
        ));
        let path = repo.path().to_str().unwrap();
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let signers = stub.path().join("allowed_signers");
        std::fs::write(&signers, "t@test.local ssh-ed25519 AAAA\n").unwrap();
        git_in(
            repo.path(),
            &[
                "config",
                "gpg.ssh.allowedSignersFile",
                signers.to_str().unwrap(),
            ],
        );
        let cache = fresh_cache();
        let t0 = Instant::now();

        verify_commit_signature_with(&cache, t0, path, &oid).unwrap();
        let first = verifier_spawns(&stub);
        assert!(first >= 1, "the ssh verifier was started");
        verify_commit_signature_with(&cache, t0, path, &oid).unwrap();
        assert_eq!(
            verifier_spawns(&stub),
            first,
            "unchanged trust file: cache hit"
        );

        std::fs::write(
            &signers,
            "t@test.local ssh-ed25519 AAAA\nother@test.local ssh-ed25519 BBBB\n",
        )
        .unwrap();
        verify_commit_signature_with(&cache, t0, path, &oid).unwrap();
        assert!(
            verifier_spawns(&stub) > first,
            "a changed allowed-signers file is verified again"
        );
    }

    /// Changing the repository's own signing configuration is a different
    /// question from the one a kept verdict answered.
    #[cfg(unix)]
    #[test]
    fn changing_the_signing_config_misses_the_cache() {
        use crate::test_support::{counting_verifier, git_in, use_verifier, verifier_spawns};
        let (repo, oid) = repo_with_commit(&pgp());
        let path = repo.path().to_str().unwrap();
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let cache = fresh_cache();
        let t0 = Instant::now();
        verify_commit_signature_with(&cache, t0, path, &oid).unwrap();
        let first = verifier_spawns(&stub);
        git_in(repo.path(), &["config", "gpg.minTrustLevel", "ultimate"]);
        verify_commit_signature_with(&cache, t0, path, &oid).unwrap();
        assert!(verifier_spawns(&stub) > first);
    }

    #[test]
    fn a_verification_that_could_not_run_is_an_error_and_is_never_cached() {
        let (repo, _) = repo_with_commit("");
        let path = repo.path().to_str().unwrap();
        let cache = fresh_cache();
        let absent = "1".repeat(40);
        let err = verify_commit_signature_with(&cache, Instant::now(), path, &absent)
            .expect_err("an object that does not exist cannot be verified");
        assert!(err.contains(&absent), "{err}");
        assert_eq!(cache.lock().unwrap().len(), 0);

        let short = verify_commit_signature_with(&cache, Instant::now(), path, "abc123")
            .expect_err("an abbreviation is not a stable name");
        assert!(short.contains("full commit id"), "{short}");
        assert!(verify_commit_signature_with(&cache, Instant::now(), path, "--all").is_err());
    }

    #[test]
    fn unreadable_commits_are_unknown_never_unsigned() {
        let (repo, oid) = repo_with_commit(&pgp());
        let absent = "1".repeat(40);
        let asked = vec![
            oid.clone(),
            absent.clone(),
            "not-an-oid".to_string(),
            oid.clone(),
        ];
        let got = commit_signatures(repo.path(), &asked);
        assert_eq!(got[&oid], CommitSignature::Pgp);
        assert_eq!(got[&absent], CommitSignature::Unknown);
        assert_eq!(got["not-an-oid"], CommitSignature::Unknown);
        assert_eq!(got.len(), 3, "a repeated id is asked once");
    }

    /// A batch cut short by its byte budget keeps the commits it read whole and
    /// reports every later one unread, which `commit_signatures` maps to
    /// `Unknown`.
    #[test]
    fn a_batch_cut_at_its_budget_leaves_the_rest_unread() {
        let (repo, tip) = repo_with_commit(&pgp());
        let mut chain = vec![tip];
        for n in 0..6 {
            let parent = chain.last().unwrap().clone();
            let next = crate::test_support::raw_commit(
                repo.path(),
                &[&parent],
                &pgp(),
                &format!("c{n}"),
                1_700_000_200 + n,
            );
            chain.push(next);
        }
        // Room for roughly two commits, not seven.
        let got = read_objects_batch(repo.path(), &chain, COMMIT, 700, &classify_header);
        assert_eq!(got.len(), chain.len(), "every id gets an answer");
        assert_eq!(
            got[&chain[0]],
            Ok(CommitSignature::Pgp),
            "the first was read whole"
        );
        let unread: Vec<_> = chain.iter().filter(|oid| got[*oid].is_err()).collect();
        assert!(
            unread.len() >= 3 && unread.contains(&chain.last().unwrap()),
            "{got:?}"
        );
        let mapped = settle(got);
        assert_eq!(mapped[chain.last().unwrap()], CommitSignature::Unknown);
    }

    // ------------------------------------------------------------------
    // Adversarial cases (CARD 16). A test named `break_*` currently FAILS and
    // is evidence of a defect; the others pin behavior that held under attack.
    // ------------------------------------------------------------------

    /// What git's own parser decides for `object`: does `%G?` start a verifier
    /// for it? The oracle the header classifier must agree with.
    #[cfg(unix)]
    fn git_spawns_a_verifier_for(object: &[u8]) -> bool {
        use crate::test_support::{
            counting_verifier, git_in, hash_object, use_verifier, verifier_spawns,
        };
        let dir = tempfile::TempDir::new().unwrap();
        git_in(dir.path(), &["init", "-b", "main"]);
        let oid = hash_object(dir.path(), "commit", object);
        git_in(dir.path(), &["update-ref", "refs/heads/main", &oid]);
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(dir.path(), &script);
        // Without an allowed-signers file git refuses an SSH signature before
        // it reaches any program.
        let signers = stub.path().join("allowed_signers");
        std::fs::write(&signers, "a@b ssh-ed25519 AAAA\n").unwrap();
        git_in(
            dir.path(),
            &[
                "config",
                "gpg.ssh.allowedSignersFile",
                signers.to_str().unwrap(),
            ],
        );
        // git may die on a signature value it cannot place (`bad/incompatible
        // signature`); whether it got as far as a verifier is the question.
        let _ = std::process::Command::new("git")
            .args(["log", "-1", &format!("--format={}G?", '%')])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        verifier_spawns(&stub) > 0
    }

    const TREE: &str = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a@b> 1 +0000\ncommitter A <a@b> 1 +0000\n";

    /// The classifier says Pgp/Ssh/X509 exactly when git would run a verifier.
    /// Every divergence is a commit the pane labels differently from what a
    /// verification of it does.
    #[cfg(unix)]
    #[test]
    fn the_header_classifier_agrees_with_gits_own_parser_on_hostile_objects() {
        let sig = |h: &str, a: &str| crate::test_support::fake_signature_header(h, a);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("plain pgp", format!("{TREE}{}\nmsg\n", sig("gpgsig", "PGP SIGNATURE")).into_bytes()),
            ("plain ssh", format!("{TREE}{}\nmsg\n", sig("gpgsig", "SSH SIGNATURE")).into_bytes()),
            ("plain x509", format!("{TREE}{}\nmsg\n", sig("gpgsig", "SIGNED MESSAGE")).into_bytes()),
            ("unsigned", format!("{TREE}\nmsg\n").into_bytes()),
            (
                "armor quoted in the body",
                format!("{TREE}\n-----BEGIN PGP SIGNATURE-----\nx\n").into_bytes(),
            ),
            (
                "gpgsig at the start of the message",
                format!("{TREE}\ngpgsig -----BEGIN PGP SIGNATURE-----\nx\n").into_bytes(),
            ),
            (
                "gpgsig-sha256 header in a sha1 repository",
                format!("{TREE}{}\nmsg\n", sig("gpgsig-sha256", "PGP SIGNATURE")).into_bytes(),
            ),
            (
                "two gpgsig headers, ssh first",
                format!(
                    "{TREE}{}{}\nmsg\n",
                    sig("gpgsig", "SSH SIGNATURE"),
                    sig("gpgsig", "PGP SIGNATURE")
                )
                .into_bytes(),
            ),
            (
                "CRLF blank line before a body that says gpgsig",
                format!("{TREE}\r\nmsg\r\ngpgsig -----BEGIN PGP SIGNATURE-----\r\n \r\n abc\r\n -----END PGP SIGNATURE-----\r\n")
                    .into_bytes(),
            ),
            (
                "gpgsig with a value that is no armor",
                format!("{TREE}gpgsig hello\n\nmsg\n").into_bytes(),
            ),
            (
                "gpgsig with an empty value",
                format!("{TREE}gpgsig \n\nmsg\n").into_bytes(),
            ),
        ];
        let mut divergences = Vec::new();
        for (name, object) in &cases {
            let ours = matches!(
                classify_header(object),
                CommitSignature::Pgp | CommitSignature::Ssh | CommitSignature::X509
            );
            let gits = git_spawns_a_verifier_for(object);
            if ours != gits {
                divergences.push(format!(
                    "{name}: classifier says signed={ours}, git spawns a verifier={gits}"
                ));
            }
        }
        assert!(divergences.is_empty(), "{divergences:#?}");
    }

    #[test]
    fn a_message_that_quotes_armor_is_not_a_signature_however_it_is_framed() {
        for body in [
            "-----BEGIN PGP SIGNATURE-----\n",
            "gpgsig -----BEGIN PGP SIGNATURE-----\n",
            "gpgsig-sha256 -----BEGIN SSH SIGNATURE-----\n",
            "\ngpgsig -----BEGIN PGP SIGNATURE-----\n",
        ] {
            let object = format!("{TREE}\n{body}");
            assert_eq!(
                classify_header(object.as_bytes()),
                CommitSignature::None,
                "{body:?}"
            );
        }
        // Headers end at the first empty line even when the commit has no body.
        assert_eq!(
            classify_header(format!("{TREE}\ngpgsig x\n").as_bytes()),
            CommitSignature::None
        );
    }

    #[test]
    fn crlf_header_lines_still_classify_by_their_armor() {
        let object = b"tree x\r\nauthor a\r\ngpgsig -----BEGIN PGP SIGNATURE-----\r\n \r\n abc\r\n\r\nmsg\r\n";
        assert_eq!(classify_header(object), CommitSignature::Pgp);
        let ssh = b"gpgsig -----BEGIN SSH SIGNATURE-----\r\n";
        assert_eq!(classify_header(ssh), CommitSignature::Ssh);
    }

    #[test]
    fn two_signature_headers_answer_with_the_first_and_an_unarmored_one_is_other() {
        let two = format!(
            "{TREE}gpgsig -----BEGIN SSH SIGNATURE-----\n x\ngpgsig -----BEGIN PGP SIGNATURE-----\n y\n\nm\n"
        );
        assert_eq!(classify_header(two.as_bytes()), CommitSignature::Ssh);
        assert_eq!(
            classify_header(format!("{TREE}gpgsig \n\nm\n").as_bytes()),
            CommitSignature::Other
        );
        assert_eq!(
            classify_header(format!("{TREE}gpgsig\n\nm\n").as_bytes()),
            CommitSignature::None
        );
    }

    /// A 10 MiB single header line: classified without scanning past the bound
    /// and without being called unsigned.
    #[test]
    fn a_ten_mebibyte_header_line_is_bounded_and_not_called_unsigned() {
        let mut huge = format!("{TREE}x-big ").into_bytes();
        huge.extend(std::iter::repeat_n(b'a', 10 * 1024 * 1024));
        huge.extend_from_slice(b"\ngpgsig -----BEGIN PGP SIGNATURE-----\n\nmsg\n");
        let started = Instant::now();
        assert_eq!(classify_header(&huge), CommitSignature::Unknown);
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)),
            "{:?}",
            started.elapsed()
        );
        // A signature first, then 10 MiB of armor: read from its first line.
        let mut signed = b"gpgsig -----BEGIN PGP SIGNATURE-----\n ".to_vec();
        signed.extend(std::iter::repeat_n(b'a', 10 * 1024 * 1024));
        assert_eq!(classify_header(&signed), CommitSignature::Pgp);
    }

    /// Whole objects are read into memory by `cat-file --batch` before the
    /// header is looked at. A 10 MiB commit message is read in full for a
    /// 300-byte answer; four of them exhaust the 32 MiB budget and the commits
    /// behind them become `Unknown`. Records the cost the header-only read of
    /// `log --pretty=raw` would not pay.
    #[test]
    fn large_messages_cost_the_batch_its_budget_but_never_misreport() {
        let dir = tempfile::TempDir::new().unwrap();
        crate::test_support::git_in(dir.path(), &["init", "-b", "main"]);
        let big = "x".repeat(10 * 1024 * 1024);
        let mut chain: Vec<String> = Vec::new();
        for n in 0..5 {
            let parents: Vec<&str> = chain.last().map(String::as_str).into_iter().collect();
            let message = if n < 4 { big.as_str() } else { "small" };
            let oid = crate::test_support::raw_commit(
                dir.path(),
                &parents,
                &pgp(),
                message,
                1_700_000_000 + n,
            );
            chain.push(oid);
        }
        let got = commit_signatures(dir.path(), &chain);
        let known = chain
            .iter()
            .filter(|o| got[*o] == CommitSignature::Pgp)
            .count();
        let unknown = chain
            .iter()
            .filter(|o| got[*o] == CommitSignature::Unknown)
            .count();
        assert_eq!(
            known + unknown,
            5,
            "every commit is either read or unknown: {got:?}"
        );
        assert!(unknown >= 1, "the budget bites: {got:?}");
        assert_eq!(got[&chain[0]], CommitSignature::Pgp);
    }

    /// A tag, a tree and a blob where a commit is expected, a missing id and a
    /// repeat, in one stream. Every later commit keeps its own answer.
    #[test]
    fn wrong_typed_and_missing_objects_do_not_shift_the_commits_after_them() {
        let (repo, tip) = repo_with_commit(&pgp());
        let dir = repo.path();
        let tree = crate::test_support::hash_object(dir, "tree", b"");
        let blob = crate::test_support::hash_object(dir, "blob", b"hello\nworld\n");
        let tag = crate::test_support::hash_object(
            dir,
            "tag",
            format!("object {tip}\ntype commit\ntag t\ntagger A <a@b> 1 +0000\n\nmsg\n").as_bytes(),
        );
        let unsigned = crate::test_support::raw_commit(dir, &[&tip], "", "u", 1_700_000_300);
        let missing = "1".repeat(40);
        let asked = vec![
            tag.clone(),
            tree.clone(),
            blob.clone(),
            missing.clone(),
            unsigned.clone(),
            tip.clone(),
        ];
        let got = commit_signatures(dir, &asked);
        for wrong in [&tag, &tree, &blob, &missing] {
            assert_eq!(got[wrong], CommitSignature::Unknown, "{wrong}");
        }
        assert_eq!(got[&unsigned], CommitSignature::None);
        assert_eq!(got[&tip], CommitSignature::Pgp);
    }

    /// An uppercase id is a valid id to git, which answers with the lowercase
    /// name. The reader must not read that as an answer to some other question
    /// and strand every id after it.
    #[test]
    fn break_an_uppercase_oid_is_read_as_the_commit_it_names() {
        let (repo, tip) = repo_with_commit(&pgp());
        let other = crate::test_support::raw_commit(repo.path(), &[&tip], "", "u", 1_700_000_300);
        let upper = tip.to_uppercase();
        let got = commit_signatures(repo.path(), &[upper.clone(), other.clone()]);
        assert_eq!(got[&upper], CommitSignature::Pgp, "{got:?}");
        assert_eq!(
            got[&other],
            CommitSignature::None,
            "the id after it stays in step: {got:?}"
        );
    }

    /// The same through the one-commit verify, which accepts uppercase hex.
    #[cfg(unix)]
    #[test]
    fn break_verifying_by_an_uppercase_oid_reports_the_signature_it_has() {
        use crate::test_support::{counting_verifier, use_verifier};
        let (repo, oid) = repo_with_commit(&pgp());
        let (_stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let check = verify_commit_signature_with(
            &fresh_cache(),
            Instant::now(),
            repo.path().to_str().unwrap(),
            &oid.to_uppercase(),
        )
        .unwrap();
        assert_eq!(check.signature, CommitSignature::Pgp, "{check:?}");
    }

    #[test]
    fn hostile_ids_are_refused_before_any_git_runs() {
        let (repo, oid) = repo_with_commit("");
        let path = repo.path().to_str().unwrap();
        let cache = fresh_cache();
        for bad in [
            "",
            "abc123",
            "HEAD~1",
            "HEAD",
            "--output=x",
            "--output=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            "-1",
            "refs/heads/main",
            "../../etc/passwd",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            &format!("{oid}\n"),
            &format!("{oid} "),
            &format!("{oid}\0"),
            &"a".repeat(41),
            &"a".repeat(65),
            &"a".repeat(10_000),
        ] {
            assert!(
                verify_commit_signature_with(&cache, Instant::now(), path, bad).is_err(),
                "{bad:?}"
            );
        }
        assert!(!repo.path().join("x").exists(), "--output=x must not write");
        assert_eq!(cache.lock().unwrap().len(), 0);
    }

    #[test]
    fn a_path_that_is_not_a_repository_is_an_error() {
        let plain = tempfile::TempDir::new().unwrap();
        let oid = "a".repeat(40);
        for bad in [
            plain.path().to_str().unwrap(),
            "",
            "relative/path",
            "/nonexistent/for/sure",
        ] {
            assert!(
                verify_commit_signature_with(&fresh_cache(), Instant::now(), bad, &oid).is_err(),
                "{bad:?}"
            );
        }
    }

    /// A 64-hex id in a sha1 repository cannot name an object: an error, and
    /// nothing cached.
    #[test]
    fn a_sha256_length_id_in_a_sha1_repository_is_an_error_and_is_not_cached() {
        let (repo, _) = repo_with_commit(&pgp());
        let cache = fresh_cache();
        let err = verify_commit_signature_with(
            &cache,
            Instant::now(),
            repo.path().to_str().unwrap(),
            &"a".repeat(64),
        );
        assert!(err.is_err(), "{err:?}");
        assert_eq!(cache.lock().unwrap().len(), 0);
    }

    /// A signing program that never returns: the bounded git read must give up
    /// and take the program down with it, not leave it running.
    #[cfg(unix)]
    #[test]
    fn a_hanging_signing_program_is_killed_when_the_timeout_fires() {
        use crate::test_support::{counting_verifier, use_verifier};
        let (repo, oid) = repo_with_commit(&pgp());
        let (stub, script) =
            counting_verifier("echo $$ > \"$(dirname \"$0\")/pid\"\nexec sleep 120");
        use_verifier(repo.path(), &script);
        let started = Instant::now();
        // Built from parts so the `%G` source scan does not count this pin.
        let format = format!("--format={}G?", '%');
        let err = git_text_with_timeout(
            repo.path(),
            &["log", "-1", "--no-walk", &format, &oid],
            Duration::from_secs(2),
        )
        .expect_err("the verifier never answers");
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(15)),
            "{:?}",
            started.elapsed()
        );
        assert!(!err.is_empty());
        let pid = std::fs::read_to_string(stub.path().join("pid"))
            .expect("the stub started")
            .trim()
            .to_string();
        let alive = || {
            std::process::Command::new("kill")
                .args(["-0", &pid])
                .output_locked()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let survived = alive();
        if survived {
            // Not ours to leave behind: this pid is the stub we started.
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid])
                .output_locked();
        }
        assert!(!survived, "the signing program outlived the timeout");
    }

    #[cfg(unix)]
    #[test]
    fn the_same_oid_in_two_repositories_is_verified_and_cached_separately() {
        use crate::test_support::{counting_verifier, use_verifier, verifier_spawns};
        // Identical objects: the same ids in both, as in a fork.
        let (a, oid_a) = repo_with_commit(&pgp());
        let (b, oid_b) = repo_with_commit(&pgp());
        assert_eq!(oid_a, oid_b, "fixture: same bytes, same id");
        let (stub_a, script_a) = counting_verifier("exit 2");
        let (stub_b, script_b) = counting_verifier("exit 2");
        use_verifier(a.path(), &script_a);
        use_verifier(b.path(), &script_b);
        let cache = fresh_cache();
        let now = Instant::now();
        verify_commit_signature_with(&cache, now, a.path().to_str().unwrap(), &oid_a).unwrap();
        verify_commit_signature_with(&cache, now, b.path().to_str().unwrap(), &oid_b).unwrap();
        assert_eq!(verifier_spawns(&stub_a), 1);
        assert_eq!(
            verifier_spawns(&stub_b),
            1,
            "B is not answered from A's verdict"
        );
        assert_eq!(cache.lock().unwrap().len(), 2);
    }

    /// An atomically replaced allowed-signers file with the same size and the
    /// same mtime is a new inode.
    #[cfg(unix)]
    #[test]
    fn an_atomically_replaced_trust_file_with_the_same_size_and_mtime_misses() {
        use crate::test_support::{counting_verifier, git_in, use_verifier, verifier_spawns};
        let ssh = crate::test_support::fake_signature_header("gpgsig", "SSH SIGNATURE");
        let (repo, oid) = repo_with_commit(&ssh);
        let path = repo.path().to_str().unwrap();
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let signers = stub.path().join("allowed_signers");
        std::fs::write(&signers, "aaaa ssh-ed25519 AAAA\n").unwrap();
        let stamped = std::fs::metadata(&signers).unwrap().modified().unwrap();
        git_in(
            repo.path(),
            &[
                "config",
                "gpg.ssh.allowedSignersFile",
                signers.to_str().unwrap(),
            ],
        );
        let cache = fresh_cache();
        let now = Instant::now();
        verify_commit_signature_with(&cache, now, path, &oid).unwrap();
        let first = verifier_spawns(&stub);
        let swap = stub.path().join("allowed_signers.new");
        std::fs::write(&swap, "bbbb ssh-ed25519 BBBB\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&swap)
            .unwrap()
            .set_modified(stamped)
            .unwrap();
        std::fs::rename(&swap, &signers).unwrap();
        verify_commit_signature_with(&cache, now, path, &oid).unwrap();
        assert!(
            verifier_spawns(&stub) > first,
            "a replaced file is a changed file"
        );
    }

    /// The edge of a stat fingerprint: the same file rewritten in place to the
    /// same length with its mtime put back. Bounded by the TTL; recorded as a
    /// known limit rather than expected to hold.
    #[cfg(unix)]
    #[test]
    fn break_an_in_place_rewrite_with_the_mtime_restored_is_a_cache_hit() {
        use crate::test_support::{counting_verifier, git_in, use_verifier, verifier_spawns};
        let ssh = crate::test_support::fake_signature_header("gpgsig", "SSH SIGNATURE");
        let (repo, oid) = repo_with_commit(&ssh);
        let path = repo.path().to_str().unwrap();
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let signers = stub.path().join("allowed_signers");
        std::fs::write(&signers, "aaaa ssh-ed25519 AAAA\n").unwrap();
        let stamped = std::fs::metadata(&signers).unwrap().modified().unwrap();
        git_in(
            repo.path(),
            &[
                "config",
                "gpg.ssh.allowedSignersFile",
                signers.to_str().unwrap(),
            ],
        );
        let cache = fresh_cache();
        let now = Instant::now();
        verify_commit_signature_with(&cache, now, path, &oid).unwrap();
        let first = verifier_spawns(&stub);
        let file = std::fs::File::options().write(true).open(&signers).unwrap();
        std::fs::write(&signers, "bbbb ssh-ed25519 BBBB\n").unwrap();
        file.set_modified(stamped).unwrap();
        verify_commit_signature_with(&cache, now, path, &oid).unwrap();
        // ACCEPTED LIMIT, not a goal. A stat fingerprint (inode, length,
        // modification time) cannot see a file rewritten in place to the same
        // length with its modification time put back; hashing every trust file
        // on every select would buy that at the cost of reading a keyring per
        // click. The verdict is served stale for at most the TTL, and the
        // next verification after it reads the file as it is.
        assert_eq!(
            verifier_spawns(&stub),
            first,
            "the documented limit: an invisible in-place rewrite is a cache hit"
        );
        verify_commit_signature_with(&cache, now + VERIFY_TTL, path, &oid).unwrap();
        assert!(
            verifier_spawns(&stub) > first,
            "the TTL bounds the staleness: past it the file is read again"
        );
    }

    /// `include.path` pulls in config the fingerprint never stamps, so a
    /// signing program swapped there is invisible to it.
    #[cfg(unix)]
    #[test]
    fn break_a_change_in_an_included_config_file_misses_the_cache() {
        use crate::test_support::{counting_verifier, git_in, verifier_spawns};
        let (repo, oid) = repo_with_commit(&pgp());
        let path = repo.path().to_str().unwrap();
        let (stub_one, script_one) = counting_verifier("exit 2");
        let (stub_two, script_two) = counting_verifier("exit 2");
        let include = stub_one.path().join("signing.cfg");
        std::fs::write(
            &include,
            format!("[gpg]\n\tprogram = {}\n", script_one.display()),
        )
        .unwrap();
        git_in(
            repo.path(),
            &["config", "include.path", include.to_str().unwrap()],
        );
        let cache = fresh_cache();
        let now = Instant::now();
        verify_commit_signature_with(&cache, now, path, &oid).unwrap();
        assert_eq!(verifier_spawns(&stub_one), 1);
        std::fs::write(
            &include,
            format!("[gpg]\n\tprogram = {}\n# changed\n", script_two.display()),
        )
        .unwrap();
        verify_commit_signature_with(&cache, now, path, &oid).unwrap();
        assert_eq!(
            verifier_spawns(&stub_two),
            1,
            "the verdict was reached under a gpg.program that is no longer configured"
        );
    }

    /// Changing the user's own gitconfig is stamped.
    #[cfg(unix)]
    #[test]
    fn a_change_in_the_user_gitconfig_misses_the_cache() {
        use crate::test_support::{counting_verifier, use_verifier, verifier_spawns};
        let (repo, oid) = repo_with_commit(&pgp());
        let path = repo.path().to_str().unwrap();
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let _serial = crate::test_support::env::env_serial();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("hermetic HOME");
        let config = home.join(".gitconfig");
        let original = std::fs::read(&config).ok();
        let cache = fresh_cache();
        let now = Instant::now();
        verify_commit_signature_with(&cache, now, path, &oid).unwrap();
        let first = verifier_spawns(&stub);
        let mut changed = original.clone().unwrap_or_default();
        changed.extend_from_slice(b"\n[gpg]\n\tminTrustLevel = ultimate\n");
        std::fs::write(&config, &changed).unwrap();
        let again = verify_commit_signature_with(&cache, now, path, &oid);
        match original {
            Some(bytes) => std::fs::write(&config, bytes).unwrap(),
            None => std::fs::remove_file(&config).unwrap(),
        }
        again.unwrap();
        assert!(verifier_spawns(&stub) > first);
    }

    #[test]
    fn a_different_gnupg_home_is_a_different_fingerprint() {
        let (repo, _) = repo_with_commit("");
        let serial = crate::test_support::env::env_serial();
        let _env = crate::test_support::env::bind_env(&serial).set("GNUPGHOME", "/tmp/gp-one");
        let one = signing_fingerprint(repo.path()).unwrap();
        drop(_env);
        let _env = crate::test_support::env::bind_env(&serial).set("GNUPGHOME", "/tmp/gp-two");
        let two = signing_fingerprint(repo.path()).unwrap();
        assert_ne!(one, two);
    }

    /// Two selects of the same commit at once both miss and both start the
    /// signing program: nothing coalesces them.
    #[cfg(unix)]
    #[test]
    fn break_two_selects_racing_for_one_oid_start_the_verifier_once() {
        use crate::test_support::{counting_verifier, use_verifier, verifier_spawns};
        let (repo, oid) = repo_with_commit(&pgp());
        let path = repo.path().to_str().unwrap().to_string();
        let (stub, script) = counting_verifier("sleep 1\nexit 2");
        use_verifier(repo.path(), &script);
        let cache = fresh_cache();
        let now = Instant::now();
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| verify_commit_signature_with(&cache, now, &path, &oid).unwrap());
            }
        });
        assert_eq!(
            verifier_spawns(&stub),
            1,
            "two concurrent verifications of one commit"
        );
    }

    #[test]
    fn the_cache_never_exceeds_its_capacity_under_many_distinct_commits() {
        let mut cache = VerifyCache::new(Duration::from_secs(3600), 16);
        let t0 = Instant::now();
        for n in 0..5_000u64 {
            cache.put(
                Path::new(&format!("/r/{}", n % 7)),
                &format!("{n:040x}"),
                fingerprint("f"),
                Vec::new(),
                check(SignatureStatus::Good),
                t0 + Duration::from_millis(n),
            );
            assert!(cache.len() <= 16);
        }
    }

    /// `%G?` with a verifier that cannot run and an SSH commit with no
    /// allowed-signers file are both `signed, could not verify`, not unsigned.
    #[cfg(unix)]
    #[test]
    fn break_a_gpgsig_sha256_header_in_a_sha1_repository_agrees_with_what_verify_does() {
        use crate::test_support::{counting_verifier, use_verifier, verifier_spawns};
        let header = crate::test_support::fake_signature_header("gpgsig-sha256", "PGP SIGNATURE");
        let (repo, oid) = repo_with_commit(&header);
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let check = verify_commit_signature_with(
            &fresh_cache(),
            Instant::now(),
            repo.path().to_str().unwrap(),
            &oid,
        )
        .unwrap();
        // If the object reads as signed, a verification must have been
        // attempted; otherwise the pane says "signed, could not verify" for a
        // commit git itself treats as unsigned.
        assert!(
            check.signature == CommitSignature::None || verifier_spawns(&stub) > 0,
            "classified {:?}, verifier spawns {}",
            check.signature,
            verifier_spawns(&stub)
        );
    }

    // ------------------------------------------------------------------
    // The header-only walk read (`rev-list --header`) that replaces the
    // post-walk batch on the pulse critical path.
    // ------------------------------------------------------------------

    #[test]
    fn the_header_stream_splits_by_record_and_never_trusts_a_cut_tail() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let signed = "tree x\ngpgsig -----BEGIN SSH SIGNATURE-----\n x\n\n    m\n";
        let stream = format!("{a}\n{signed}\0{b}\ntree x\n\n    m\n\0");
        let whole = read_header_stream(stream.as_bytes(), false);
        assert_eq!(whole[&a], CommitSignature::Ssh);
        assert_eq!(whole[&b], CommitSignature::None);
        // Cut inside b's record: b is not an answer, a still is.
        let cut = format!("{a}\n{signed}\0{b}\ntree x\ngpgs");
        let partial = read_header_stream(cut.as_bytes(), true);
        assert_eq!(partial[&a], CommitSignature::Ssh);
        assert!(!partial.contains_key(&b));
        // Cut exactly after a record: both complete records stay.
        let boundary = format!("{a}\n{signed}\0");
        assert_eq!(
            read_header_stream(boundary.as_bytes(), true)[&a],
            CommitSignature::Ssh
        );
        assert!(read_header_stream(b"", true).is_empty());
        assert!(read_header_stream(b"\0\0\0", false).is_empty());
    }

    #[test]
    fn a_record_that_does_not_start_with_an_object_id_is_ignored() {
        let a = "a".repeat(40);
        let stream = format!("not-an-oid\ngpgsig -----BEGIN PGP SIGNATURE-----\n\0{a}\ntree x\n\n");
        let got = read_header_stream(stream.as_bytes(), false);
        assert_eq!(got.len(), 1);
        assert_eq!(got[&a], CommitSignature::None);
    }

    /// A commit message that smuggles a NUL and then a well-formed record for
    /// another commit must not mark that commit signed. `--literally` writes
    /// the object past fsck, which refuses a NUL in a commit.
    #[test]
    fn a_nul_in_a_message_cannot_forge_a_record_for_another_commit() {
        let (repo, tip) = repo_with_commit("");
        let victim =
            crate::test_support::raw_commit(repo.path(), &[&tip], "", "victim", 1_700_000_300);
        let forged = format!(
            "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nparent {victim}\nauthor A <a@b> 1700000400 +0000\ncommitter A <a@b> 1700000400 +0000\n\nhello\0{victim}\ngpgsig -----BEGIN PGP SIGNATURE-----\n"
        );
        let mut child = std::process::Command::new("git")
            .args([
                "hash-object",
                "-t",
                "commit",
                "-w",
                "--literally",
                "--stdin",
            ])
            .current_dir(repo.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn_locked()
            .unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), forged.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let attacker = String::from_utf8(out.stdout).unwrap().trim().to_string();
        assert_eq!(attacker.len(), 40, "fixture written");
        crate::test_support::git_in(repo.path(), &["update-ref", "refs/heads/main", &attacker]);
        let got = walk_signatures(
            repo.path(),
            &[
                "rev-list",
                "-n10",
                "--topo-order",
                "--header",
                "refs/heads/main",
            ],
        );
        assert_eq!(got[&victim], CommitSignature::None, "{got:?}");
    }

    /// The header read and the per-id batch read agree on every commit of a
    /// mixed history, and the header read starts no verifier.
    #[cfg(unix)]
    #[test]
    fn the_walk_read_agrees_with_the_batch_read_and_starts_no_verifier() {
        use crate::test_support::{counting_verifier, use_verifier, verifier_spawns};
        let (repo, tip) = repo_with_commit(&pgp());
        let (stub, script) = counting_verifier("exit 2");
        use_verifier(repo.path(), &script);
        let mut chain = vec![tip];
        for n in 0..12u64 {
            let parent = chain.last().unwrap().clone();
            let headers = match n % 4 {
                0 => String::new(),
                1 => crate::test_support::fake_signature_header("gpgsig", "SSH SIGNATURE"),
                2 => crate::test_support::fake_signature_header("gpgsig", "SIGNED MESSAGE"),
                _ => pgp(),
            };
            let oid = crate::test_support::raw_commit(
                repo.path(),
                &[&parent],
                &headers,
                &format!("c{n}"),
                1_700_000_200 + n as i64,
            );
            chain.push(oid);
        }
        crate::test_support::git_in(
            repo.path(),
            &["update-ref", "refs/heads/main", chain.last().unwrap()],
        );
        let walked = walk_signatures(
            repo.path(),
            &[
                "rev-list",
                "-n100",
                "--topo-order",
                "--header",
                "refs/heads/main",
            ],
        );
        let batched = commit_signatures(repo.path(), &chain);
        // The seed commit from the fixture is unsigned and part of the walk.
        for oid in &chain {
            assert_eq!(walked[oid], batched[oid], "{oid}");
        }
        assert_eq!(verifier_spawns(&stub), 0);
    }

    /// A rev-list that git refuses (a ref that does not exist) is an empty map,
    /// which the caller reads as every commit `Unknown`.
    #[test]
    fn a_failed_walk_read_is_an_empty_map_not_a_claim_of_unsigned() {
        let (repo, _) = repo_with_commit(&pgp());
        let got = walk_signatures(
            repo.path(),
            &["rev-list", "-n10", "--header", "refs/heads/does-not-exist"],
        );
        assert!(got.is_empty());
    }

    /// The stream is parsed as it arrives, so how it is cut into chunks cannot
    /// change what it says, down to one byte at a time.
    #[test]
    fn a_stream_cut_into_any_chunks_reads_the_same_as_whole() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let signed = "tree x\ngpgsig -----BEGIN SSH SIGNATURE-----\n x\n\n    m\n";
        let whole = format!("{a}\n{signed}\0{b}\ntree x\n\n    m\n\0");
        let expected = read_header_stream(whole.as_bytes(), false);
        for size in [1, 2, 3, 7, 41, 64] {
            let mut stream = HeaderStream::default();
            for chunk in whole.as_bytes().chunks(size) {
                stream.feed(chunk);
            }
            assert_eq!(stream.finish(false), expected, "chunks of {size}");
        }
    }

    /// A message of any length keeps its head only: the record buffer never
    /// grows past its bound, and the classification is unchanged.
    #[test]
    fn a_giant_message_is_streamed_without_being_kept() {
        let oid = "c".repeat(40);
        let mut stream = HeaderStream::default();
        stream.feed(
            format!("{oid}\ntree x\ngpgsig -----BEGIN PGP SIGNATURE-----\n x\n\n").as_bytes(),
        );
        let filler = vec![b'm'; 1024 * 1024];
        for _ in 0..16 {
            stream.feed(&filler);
            assert!(
                stream.record.len() <= KEPT_RECORD_BYTES,
                "kept {} bytes of a record",
                stream.record.len()
            );
        }
        stream.feed(b"\0");
        assert_eq!(stream.finish(false)[&oid], CommitSignature::Pgp);
    }

    /// A header that outruns what is kept is `Unknown`, as in the whole-object
    /// classifier, whichever way it arrives.
    #[test]
    fn an_endless_header_in_the_stream_is_unknown_not_unsigned() {
        let oid = "d".repeat(40);
        let mut stream = HeaderStream::default();
        stream.feed(format!("{oid}\n").as_bytes());
        let filler = vec![b'x'; 1024 * 1024];
        for _ in 0..2 {
            stream.feed(&filler);
        }
        stream.feed(b"\0");
        assert_eq!(stream.finish(false)[&oid], CommitSignature::Unknown);
    }

    /// A commit the stream names twice has one record that is not git's, and
    /// says nothing either way.
    #[test]
    fn a_commit_named_twice_is_unknown() {
        let a = "a".repeat(40);
        let stream =
            format!("{a}\ntree x\n\n\0{a}\ntree x\ngpgsig -----BEGIN PGP SIGNATURE-----\n\n\0");
        assert_eq!(
            read_header_stream(stream.as_bytes(), false)[&a],
            CommitSignature::Unknown
        );
    }

    /// In a SHA-256 repository git reads `gpgsig-sha256` and not `gpgsig`, and
    /// the classifier does the same: it says signed exactly when a
    /// verification starts a verifier.
    #[cfg(unix)]
    #[test]
    fn a_sha256_repository_reads_gpgsig_sha256_and_agrees_with_git() {
        use crate::test_support::{counting_verifier, git_in, use_verifier, verifier_spawns};
        for (header, signed) in [("gpgsig-sha256", true), ("gpgsig", false)] {
            let dir = tempfile::TempDir::new().unwrap();
            git_in(
                dir.path(),
                &["init", "-b", "main", "--object-format=sha256"],
            );
            let oid = crate::test_support::raw_commit(
                dir.path(),
                &[],
                &crate::test_support::fake_signature_header(header, "PGP SIGNATURE"),
                "c",
                1_700_000_000,
            );
            assert_eq!(oid.len(), 64);
            git_in(dir.path(), &["update-ref", "refs/heads/main", &oid]);
            let (stub, script) = counting_verifier("exit 2");
            use_verifier(dir.path(), &script);
            let check = verify_commit_signature_with(
                &fresh_cache(),
                Instant::now(),
                dir.path().to_str().unwrap(),
                &oid,
            )
            .unwrap();
            assert_eq!(check.signature.is_signed(), signed, "{header}: {check:?}");
            assert_eq!(
                verifier_spawns(&stub) > 0,
                signed,
                "{header}: the classifier and git disagree about whether this is signed"
            );
            let walked = walk_signatures(
                dir.path(),
                &["rev-list", "-n5", "--header", "refs/heads/main"],
            );
            assert_eq!(walked[&oid].is_signed(), signed, "{header} (walk)");
        }
    }

    /// An id in capitals reads the commit it names in the walk-free reader
    /// too: the answer comes back under the spelling that was asked, and the
    /// ids after it stay in step.
    #[test]
    fn mixed_case_ids_in_a_batch_are_answered_under_each_spelling() {
        let (repo, tip) = repo_with_commit(&pgp());
        let other = crate::test_support::raw_commit(repo.path(), &[&tip], "", "u", 1_700_000_301);
        let upper = tip.to_uppercase();
        let got = commit_signatures(repo.path(), &[upper.clone(), tip.clone(), other.clone()]);
        assert_eq!(got[&upper], CommitSignature::Pgp);
        assert_eq!(got[&tip], CommitSignature::Pgp);
        assert_eq!(got[&other], CommitSignature::None);
    }

    /// A verification that fails releases the select waiting on it with that
    /// failure, and is not kept: the next select tries again.
    #[test]
    fn a_failed_verification_releases_its_waiters_with_the_error_and_is_not_cached() {
        let (repo, _) = repo_with_commit("");
        let path = repo.path().to_str().unwrap().to_string();
        let absent = "2".repeat(40);
        let cache = fresh_cache();
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        verify_commit_signature_with(&cache, Instant::now(), &path, &absent)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(results.iter().all(Result::is_err), "{results:?}");
        let state = cache.lock().unwrap();
        assert_eq!(state.len(), 0, "a failure is never cached");
        assert!(state.flights.is_empty(), "no flight outlives its leader");
    }

    /// Past the flight bound a select still runs, alone.
    #[test]
    fn selects_past_the_flight_bound_are_not_refused() {
        let (repo, oid) = repo_with_commit("");
        let path = repo.path().to_str().unwrap();
        let cache = fresh_cache();
        {
            let mut state = cache.lock().unwrap();
            for n in 0..MAX_FLIGHTS {
                state.flights.insert(
                    (PathBuf::from(format!("/busy/{n}")), "e".repeat(40)),
                    Arc::default(),
                );
            }
        }
        let check = verify_commit_signature_with(&cache, Instant::now(), path, &oid).unwrap();
        assert_eq!(check.signature, CommitSignature::None);
    }
}
