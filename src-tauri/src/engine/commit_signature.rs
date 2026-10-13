//! Does a commit carry a signature, and does it verify?
//!
//! Two different questions with two different costs, kept apart on purpose.
//!
//! *Carries one* is a fact about the commit object: a `gpgsig` header. It is
//! read for a whole walk at once, from the objects themselves, and never runs a
//! signing program ([`commit_signatures`]).
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
    git_captured, git_text_with_timeout, read_objects_batch, validate_repo, BatchObject,
};
use crate::engine::git_writer::validate_oid;
use crate::engine::ref_cache::{user_config_paths, FileIdentity};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
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

/// The longest run of commit-header bytes the classifier reads. A real header
/// is a few hundred bytes (an octopus merge a few KiB); anything past this is
/// not scanned further, and a commit whose header is still going at the limit
/// is `Unknown` rather than guessed `None`.
const MAX_HEADER_BYTES: usize = 256 * 1024;

/// Byte budget for one batch of commit reads. A signature walk asks for up to
/// [`crate::engine::budget`]'s pulse commit count, and a commit is mostly its
/// message; past this the unread commits are `Unknown`.
const BATCH_BUDGET: usize = 32 * 1024 * 1024;

const COMMIT: BatchObject = BatchObject {
    kind: "commit",
    noun: "commit",
};

/// Classifies one commit object (the bytes `cat-file` prints for it) by its
/// signature header, without running anything.
///
/// Only a `gpgsig` or `gpgsig-sha256` header at column 0, before the blank line
/// that ends the headers, counts. A `mergetag` header (the signature of a
/// *tag* that was merged) does not, and neither does a message body that
/// happens to start a line with `gpgsig `: continuation lines of a header begin
/// with a space, and the body begins after the blank line. The input is
/// untrusted object content: it is scanned once, never past
/// [`MAX_HEADER_BYTES`], with no allocation proportional to it, and no input
/// panics.
pub(crate) fn classify_header(object: &[u8]) -> CommitSignature {
    let scanned = &object[..object.len().min(MAX_HEADER_BYTES)];
    let mut rest = scanned;
    while !rest.is_empty() {
        let (line, tail) = match rest.iter().position(|&b| b == b'\n') {
            Some(at) => (&rest[..at], &rest[at + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        if line.is_empty() {
            return CommitSignature::None;
        }
        for header in [&b"gpgsig "[..], &b"gpgsig-sha256 "[..]] {
            if let Some(armor) = line.strip_prefix(header) {
                return classify_armor(armor);
            }
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
///
/// Every id gets an answer. One whose object could not be read, or that came
/// after the batch was cut short, is [`CommitSignature::Unknown`]: a check that
/// could not run never reports the same as one that ran and found nothing.
pub(crate) fn commit_signatures(repo: &Path, oids: &[String]) -> HashMap<String, CommitSignature> {
    settle(read_objects_batch(
        repo,
        oids,
        COMMIT,
        BATCH_BUDGET,
        &classify_header,
    ))
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
/// fingerprint without spawning anything.
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

/// The identities of files a verify read that no fixed list names: the SSH
/// allowed-signers and revocation files, wherever the config points.
type LearnedFiles = Vec<(PathBuf, Option<FileIdentity>)>;

/// Asks git where the SSH trust files are, and stamps them. `None` when the
/// config could not be read, so the answer is not cached against a guess.
fn learn_trust_files(repo: &Path) -> Option<LearnedFiles> {
    let run = git_captured(
        repo,
        &[
            "config",
            "-z",
            "--includes",
            "--get-regexp",
            r"^gpg\.ssh\.(allowedsignersfile|revocationfile)$",
        ],
    )
    .ok()?;
    // Exit 1 is git's "no such key", which is an answer: nothing to stamp.
    if run.incomplete.is_some() || !(run.success || run.status_code == 1) {
        return None;
    }
    let text = String::from_utf8_lossy(&run.stdout);
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let mut learned: LearnedFiles = Vec::new();
    for record in text.split('\0').filter(|record| !record.is_empty()) {
        let Some((_, value)) = record.split_once('\n') else {
            continue;
        };
        let path = match (value.strip_prefix("~/"), &home) {
            (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
            _ => repo.join(value),
        };
        let identity = FileIdentity::at(&path);
        learned.push((path, identity));
    }
    learned.sort_by(|a, b| a.0.cmp(&b.0));
    learned.dedup_by(|a, b| a.0 == b.0);
    Some(learned)
}

/// How long a verification is served again. A verdict depends on files this
/// cache stamps, and on inputs it cannot see (a key server, an `include`d
/// config, the clock a signature expires by), so it is bounded in time too.
const VERIFY_TTL: Duration = Duration::from_secs(60);

/// Verdicts kept at once, across every repository.
const VERIFY_CAPACITY: usize = 256;

/// The longest one verification may run. Generous for a keyserver round trip,
/// short of the ninety seconds a hung pinentry would otherwise hold a worker.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

struct Verdict {
    check: CommitSignatureCheck,
    fingerprint: SigningFingerprint,
    learned: LearnedFiles,
    stored_at: Instant,
}

/// Recent verifications, keyed by repository and full commit id and valid
/// while the signing state it was computed under is unchanged. The clock is a
/// parameter of every call so expiry is testable without sleeping.
pub(crate) struct VerifyCache {
    verdicts: HashMap<(PathBuf, String), Verdict>,
    ttl: Duration,
    capacity: usize,
}

impl VerifyCache {
    pub(crate) fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            verdicts: HashMap::new(),
            ttl,
            capacity: capacity.max(1),
        }
    }

    /// The kept verdict for `oid`, if it is younger than the TTL and neither
    /// the fingerprint nor any learned trust file has changed since.
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
/// object id. Runs the signing program at most once per cache lifetime; an
/// unsigned commit runs none.
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
/// instead of being served a verdict nobody reached.
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

    let oids = [commit_id.to_string()];
    let signature = commit_signatures(&repo, &oids)
        .remove(commit_id)
        .unwrap_or(CommitSignature::Unknown);
    // Stamped before the verifier runs, so a trust file edited while it runs
    // leaves the stored identity different from the file and the next select
    // verifies again.
    let learned = if signature == CommitSignature::None {
        Some(Vec::new())
    } else {
        learn_trust_files(&repo)
    };
    let check = if signature == CommitSignature::None {
        CommitSignatureCheck {
            signature,
            status: SignatureStatus::None,
            key: String::new(),
            signer: String::new(),
        }
    } else {
        run_verifier(&repo, commit_id, signature)?
    };
    if let (Some(fingerprint), Some(learned)) = (fingerprint, learned) {
        cache.lock().unwrap_or_else(PoisonError::into_inner).put(
            &repo,
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
            classify_header(&header(&crate::test_support::fake_signature_header(
                "gpgsig-sha256",
                "PGP SIGNATURE"
            ))),
            CommitSignature::Pgp,
            "a SHA-256 repository's header counts"
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
                if let Some(armor) = line
                    .strip_prefix(b"gpgsig ")
                    .or_else(|| line.strip_prefix(b"gpgsig-sha256 "))
                {
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
}
