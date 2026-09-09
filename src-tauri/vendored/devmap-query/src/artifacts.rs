//! Artifact writers with tmp+rename (V14) and fingerprint skip-on-unchanged.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::escape::html_escape;
use crate::manifest::CONSUMER_MAP_ENGINE;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactFingerprint {
    pub generated_head: String,
    pub built_at: u64,
    pub fingerprint: String,
}

/// Write bytes via tmp+rename; returns true when content changed on disk.
/// Distinguishes concurrent temp files written by one process.
static WRITE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<bool> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;

    // A *unique* temp name per writer. `path.with_extension("tmp")` is shared by
    // every concurrent process writing the same artifact: two `dev map` runs
    // against one repository both create `repo_map.tmp`, the first rename moves
    // it away, and the second fails with ENOENT. Measured at 24-way
    // concurrency: 8 of 24 workers died in `manifest` with
    // `No such file or directory (os error 2)`. The store itself survived —
    // SC28 hardened it — so this was the last unguarded writer.
    //
    // pid separates processes; the counter separates the two artifacts one
    // process writes in a single `manifest` run.
    let stamp = WRITE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let unique = format!(
        "{}.{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("artifact"),
        std::process::id(),
        stamp
    );
    let tmp = parent.join(unique);

    // Any early return past this point must not strand the temp file, so the
    // body is run once and the temp cleaned on failure.
    let result = (|| -> std::io::Result<bool> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
        if path.exists() {
            let existing = fs::read(path)?;
            if existing == content {
                return Ok(false);
            }
        }
        fs::rename(&tmp, path)?;
        Ok(true)
    })();
    if !matches!(result, Ok(true)) {
        fs::remove_file(&tmp).ok();
    }
    result
}

// ---- consumer-artifact stamp -------------------------------------------------
//
// `should_regenerate` below answers the same question for the HTML artifacts by
// reading the file back and looking for a marker in it. That is affordable for a
// visualizer page and is not for the pair `manifest` writes: `code_graph.json`
// is 22 MB on this repository, and by the time the marker could be compared the
// generation has already been read out of SQLite and serialized — 0.43 s of a
// 1.39 s `dev hook post-tool-use` spent producing bytes identical to the ones
// already on disk. `write_atomic` then declines the rename, so nothing changed
// and nothing was saved.
//
// The stamp moves the decision in front of all of that. It is a small sidecar
// naming (a) the binary that wrote the artifacts, (b) every input their content
// derives from, and (c) what each output looked like when it was written; when
// all three still hold, the store is never read.

/// What one written artifact looked like immediately after it was written.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRecord {
    /// Which artifact this is — `repo_map`, `code_graph`, `compact_graph`.
    ///
    /// The stamp used to identify an output by its path alone, which made it
    /// unusable to any consumer that had not reproduced the writer's exact
    /// spelling: the paths are recorded *as resolved*, so a default run stores
    /// `/repo/./.devcouncil/repo_map.json` — with the `./` the CLI's default
    /// argument leaves in — and a consumer joining the repository root with the
    /// relative path builds a string that names the same file and does not
    /// compare equal. The role is the stable key; the path is what the role
    /// resolved to on the run that wrote it.
    pub role: String,
    pub path: String,
    pub len: u64,
    pub mtime_ns: i128,
    /// Inode, 0 where the platform has none. Catches a file swapped for another
    /// of the same length whose mtime was restored with it.
    #[serde(default)]
    pub ino: u64,
    /// Inode change time, the field userspace cannot back-date. `-1` off unix,
    /// where there is none.
    ///
    /// `len`+`mtime_ns`+`ino` alone are all restorable: an in-place rewrite of
    /// the same number of bytes keeps the length and the inode, and
    /// `utimensat` — which is what `cp -p`, `rsync --times`, `tar -x` and
    /// `File::set_times` all reach for — puts the modification time back. The
    /// stamp then matched a file whose contents had changed, and the skip that
    /// quotes it reported `artifacts_unchanged` over bytes belonging to another
    /// generation, permanently: every later run compared against the same
    /// doctored stat. `ctime` moves on any write and cannot be set, which is
    /// the same reason `freshness::stat_key` carries it.
    #[serde(default = "unknown_ctime")]
    pub ctime_ns: i128,
    /// Content identity when the platform has no non-restorable change clock.
    /// Old records without either form of evidence cannot authorize a skip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<u64>,
}

/// A stamp written before `ctime_ns` existed has no value for it. `-1` is the
/// same value an unreadable clock yields and matches no real `ctime`, so such a
/// stamp is a miss and its artifacts regenerate once — which is the fail-closed
/// direction.
fn unknown_ctime() -> i128 {
    -1
}

impl ArtifactRecord {
    fn of(role: &str, path: &Path) -> std::io::Result<Self> {
        let meta = fs::metadata(path)?;
        let change_time = ctime_ns(&meta);
        let content_hash = if change_time < 0 {
            use std::io::Read;
            let limit = crate::host::DEFAULT_ARTIFACT_BYTES;
            let mut content = String::new();
            fs::File::open(path)?
                .take(limit + 1)
                .read_to_string(&mut content)?;
            if content.len() as u64 > limit {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "artifact exceeds the bounded fingerprint read limit",
                ));
            }
            Some(devmap_extract::content_hash(&content))
        } else {
            None
        };
        Ok(Self {
            role: role.to_string(),
            path: path.to_string_lossy().into_owned(),
            len: meta.len(),
            mtime_ns: mtime_ns(&meta),
            ino: ino_of(&meta),
            ctime_ns: change_time,
            content_hash,
        })
    }

    /// Whether the file this record names is still, byte for byte, the file it
    /// was taken from.
    ///
    /// The one owner of that question: [`ArtifactStamp::still_current`] asks it
    /// of every output before allowing a skip, and a consumer asks it before
    /// believing the engine the stamp names.
    ///
    /// Fail-closed. A path that will not stat is **not** a match, because "the
    /// file is gone" and "the file is the one we wrote" are the two answers this
    /// may never conflate.
    pub fn still_describes_disk(&self) -> bool {
        if self.ctime_ns < 0 && self.content_hash.is_none() {
            return false;
        }
        ArtifactRecord::of(&self.role, Path::new(&self.path)).is_ok_and(|current| &current == self)
    }
}

fn mtime_ns(meta: &fs::Metadata) -> i128 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos() as i128)
        .unwrap_or(-1)
}

#[cfg(unix)]
fn ino_of(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn ino_of(_meta: &fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn ctime_ns(meta: &fs::Metadata) -> i128 {
    use std::os::unix::fs::MetadataExt;
    meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128
}

/// No portable non-restorable change clock off Unix. Records hash bounded
/// content there instead of treating two unknown clocks as evidence of equality.
#[cfg(not(unix))]
fn ctime_ns(_meta: &fs::Metadata) -> i128 {
    -1
}

/// The layout of the sidecar. A stamp written under a different layout is not
/// read as though it were this one; it is a miss, and the artifacts regenerate.
///
/// `3` is the first version that is *readable*. Under `2` every `inputs` value
/// was a Rust `Debug` rendering — `"Some(\"c2:8261…\")"`, `"None"`, `"1"` — so
/// the file was JSON in syntax only and nothing outside this crate could take a
/// value out of it without reimplementing `Debug for Option<String>`. The values
/// are now real JSON: strings are strings, numbers are numbers, and a digest
/// that could not be computed is `null` rather than the four characters `None`.
const ARTIFACT_STAMP_VERSION: u32 = 3;

/// The sidecar: what produced the consumer artifacts, and from what.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactStamp {
    pub version: u32,
    /// Identity of the kernel that wrote these artifacts — see
    /// [`writer_identity`]. A rebuilt kernel regenerates once, on purpose: the
    /// same generation emitted by a different binary is a different artifact,
    /// and that is exactly what an extractor or emitter change *is*.
    pub writer: String,
    /// The `map_engine` these artifacts declare of themselves — the one identity
    /// a consumer asking "did the kernel write this" is looking for.
    ///
    /// Taken from the manifest writer's own constant rather than passed in, so
    /// the sidecar cannot name an engine the artifacts do not.
    pub map_engine: String,
    /// The head the artifacts were written from, or `null` where no digest could
    /// be computed.
    ///
    /// Also one of the [`Self::inputs`] the skip decision compares. Repeated
    /// here as a typed field because a *reader* must not have to know which keys
    /// the input bag happens to carry: that bag is free to gain and lose keys,
    /// and every change to it is only ever meant to cost a regeneration.
    pub generated_head: Option<String>,
    /// Every input the artifacts' bytes derive from, named. A map rather than a
    /// struct so that adding an input can only ever cause a regeneration:
    /// an unknown key on either side makes the maps unequal, where a new struct
    /// field would quietly default and compare equal to a stamp that never
    /// carried it.
    ///
    /// The values are `serde_json::Value`, which keeps the property the `{:?}`
    /// renderings were reaching for — `Value::Null` and `Value::String("")` are
    /// unequal, so a digest that could not be computed still cannot compare
    /// equal to one that came out empty — while leaving the file readable.
    pub inputs: BTreeMap<String, serde_json::Value>,
    pub outputs: Vec<ArtifactRecord>,
}

/// `path:len:mtime_ns` of the running binary.
///
/// Two builds of this workspace both report `devmap 0.1.0`, so the version
/// string is not an identity. The executable's own stat is: same bytes on disk,
/// same emitter. Unreadable — a binary deleted or replaced under a running
/// process — yields a value that matches nothing, so the artifacts regenerate
/// rather than being trusted to a writer that cannot be identified.
pub fn writer_identity() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return "unidentified-writer".to_string();
    };
    match fs::metadata(&exe) {
        Ok(meta) => format!("{}:{}:{}", exe.display(), meta.len(), mtime_ns(&meta)),
        Err(_) => "unidentified-writer".to_string(),
    }
}

impl ArtifactStamp {
    /// Stamp `outputs` — each a `(role, path)` — as they are on disk right now.
    pub fn of(
        inputs: BTreeMap<String, serde_json::Value>,
        generated_head: Option<String>,
        outputs: &[(&str, &Path)],
    ) -> std::io::Result<Self> {
        Ok(Self {
            version: ARTIFACT_STAMP_VERSION,
            writer: writer_identity(),
            map_engine: CONSUMER_MAP_ENGINE.to_string(),
            generated_head,
            inputs,
            outputs: outputs
                .iter()
                .map(|(role, path)| ArtifactRecord::of(role, path))
                .collect::<std::io::Result<Vec<_>>>()?,
        })
    }

    /// The record for one role, or `None` when the stamp does not describe it.
    ///
    /// Role, never path: see [`ArtifactRecord::role`] for why the path a stamp
    /// carries is not a key a consumer can reconstruct.
    pub fn record(&self, role: &str) -> Option<&ArtifactRecord> {
        self.outputs.iter().find(|record| record.role == role)
    }

    pub fn read(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let stamp: Self = serde_json::from_str(&text).ok()?;
        (stamp.version == ARTIFACT_STAMP_VERSION).then_some(stamp)
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        write_atomic(path, &json).map(|_| ())
    }

    /// True when the artifacts this stamp describes are still exactly the ones
    /// the current inputs would produce.
    ///
    /// Fail-closed in every direction: an unreadable sidecar, a missing output,
    /// a stat that will not answer, a different writer, one differing input —
    /// each is a miss, and a miss regenerates. The only way to skip is for every
    /// question to have been asked and answered the same.
    pub fn still_current(
        &self,
        inputs: &BTreeMap<String, serde_json::Value>,
        outputs: &[(&str, &Path)],
    ) -> bool {
        if self.writer != writer_identity() || &self.inputs != inputs {
            return false;
        }
        if self.map_engine != CONSUMER_MAP_ENGINE {
            return false;
        }
        if self.outputs.len() != outputs.len() {
            return false;
        }
        outputs
            .iter()
            .zip(self.outputs.iter())
            .all(|((role, path), record)| {
                record.role == *role
                    && record.path == path.to_string_lossy()
                    && record.still_describes_disk()
            })
    }
}

/// Skip regeneration when fingerprint matches existing artifact header (V14).
pub fn should_regenerate(path: &Path, fp: &ArtifactFingerprint) -> bool {
    if !path.exists() {
        return true;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return true;
    };
    let marker = format!("fingerprint:{}", fp.fingerprint);
    let escaped_marker = format!("fingerprint:{}", html_escape(&fp.fingerprint));
    !text.contains(&marker) && !text.contains(&escaped_marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fp() -> ArtifactFingerprint {
        ArtifactFingerprint {
            generated_head: "abc123".into(),
            built_at: 1,
            fingerprint: "fp-test".into(),
        }
    }

    #[test]
    fn test_v14_atomic_write_and_fingerprint_skip() {
        // closes V14
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("devmap-artifact-{stamp}.html"));
        let html = format!("<!-- fingerprint:{} -->\n<p>artifact</p>", fp().fingerprint);
        assert!(write_atomic(&path, html.as_bytes()).unwrap());
        assert!(!should_regenerate(&path, &fp()));
        let fp2 = ArtifactFingerprint {
            fingerprint: "other".into(),
            ..fp()
        };
        assert!(should_regenerate(&path, &fp2));
        let _ = fs::remove_file(&path);
    }

    fn stamp_dir(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "devmap-stamp-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).expect("temp dir");
        base
    }

    fn inputs(generation: u32) -> std::collections::BTreeMap<String, serde_json::Value> {
        let mut map = std::collections::BTreeMap::new();
        map.insert("generation_id".to_string(), generation.into());
        map.insert("content_fingerprint".to_string(), "c2:abc".into());
        map
    }

    /// The whole point of the sidecar: after a write, the same inputs skip; any
    /// change to an input, or to an artifact on disk, does not.
    #[test]
    fn the_artifact_stamp_skips_only_when_every_question_answers_the_same() {
        let dir = stamp_dir("current");
        let map = dir.join("repo_map.json");
        let graph = dir.join("code_graph.json");
        let sidecar = dir.join("devmap.sqlite.artifacts.json");
        fs::write(&map, br#"{"map_engine":"devmap-rust"}"#).unwrap();
        fs::write(&graph, br#"{"meta":{}}"#).unwrap();
        let outputs = [("repo_map", map.as_path()), ("code_graph", graph.as_path())];

        // Nothing written yet: there is no stamp, so nothing may be skipped.
        assert!(ArtifactStamp::read(&sidecar).is_none());

        ArtifactStamp::of(inputs(7), Some("abc123".to_string()), &outputs)
            .unwrap()
            .write(&sidecar)
            .unwrap();
        let stamp = ArtifactStamp::read(&sidecar).expect("the stamp reads back");
        assert!(stamp.still_current(&inputs(7), &outputs));

        // A moved generation is a different artifact.
        assert!(!stamp.still_current(&inputs(8), &outputs));

        // An input this run does not know about must not compare equal to a
        // stamp that never carried it.
        let mut extra = inputs(7);
        extra.insert("pending_count".to_string(), 3.into());
        assert!(!stamp.still_current(&extra, &outputs));

        // A role that moved to a different file is a different artifact set,
        // even when both paths still stat exactly as recorded.
        let swapped = [("repo_map", graph.as_path()), ("code_graph", map.as_path())];
        assert!(!stamp.still_current(&inputs(7), &swapped));

        // An artifact edited or replaced under us is not the one we wrote.
        fs::write(&graph, br#"{"meta":{"tampered":true}}"#).unwrap();
        assert!(!stamp.still_current(&inputs(7), &outputs));

        // …and one that is simply gone certainly is not.
        fs::remove_file(&graph).unwrap();
        assert!(!stamp.still_current(&inputs(7), &outputs));

        let _ = fs::remove_dir_all(&dir);
    }

    /// A corrupt, truncated or foreign-layout sidecar must read as "no stamp",
    /// never as a stamp that happens to match.
    #[test]
    fn an_unreadable_stamp_is_a_miss_not_a_match() {
        let dir = stamp_dir("corrupt");
        let sidecar = dir.join("stamp.json");
        for body in [
            "".as_bytes(),
            b"not json at all",
            br#"{"version": 999, "writer": "x", "inputs": {}, "outputs": []}"#,
            br#"{"version": 1, "writer": "x"}"#,
            // The version-2 layout: string inputs, no engine, no roles. It is a
            // miss, so the artifacts regenerate once and the next stamp is
            // readable — rather than being read as a stamp whose engine and
            // roles happen to be absent.
            br#"{"version": 2, "writer": "x",
                 "inputs": {"generation_id": "7"},
                 "outputs": [{"path": "/tmp/repo_map.json", "len": 2,
                              "mtime_ns": 1, "ino": 1, "ctime_ns": 1}]}"#,
        ] {
            fs::write(&sidecar, body).unwrap();
            assert!(
                ArtifactStamp::read(&sidecar).is_none(),
                "must not parse: {}",
                String::from_utf8_lossy(body)
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// A stamp written by another binary is not this binary's evidence.
    #[test]
    fn a_stamp_from_a_different_writer_never_matches() {
        let dir = stamp_dir("writer");
        let artifact = dir.join("repo_map.json");
        fs::write(&artifact, b"{}").unwrap();
        let outputs = [("repo_map", artifact.as_path())];
        let mut stamp = ArtifactStamp::of(inputs(1), None, &outputs).unwrap();
        assert!(stamp.still_current(&inputs(1), &outputs));
        stamp.writer = "/some/other/devmap:123:456".to_string();
        assert!(!stamp.still_current(&inputs(1), &outputs));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A stamp naming an engine that is not this kernel's is not this kernel's
    /// evidence either.
    ///
    /// `map_engine` is what a consumer reads out of the sidecar *instead of*
    /// parsing the artifact, so a stamp whose engine has been edited must not
    /// go on authorising skips: the skip is what keeps the artifacts — and the
    /// engine they declare — on disk unexamined.
    #[test]
    fn a_stamp_naming_another_engine_never_matches() {
        let dir = stamp_dir("engine");
        let artifact = dir.join("repo_map.json");
        fs::write(&artifact, b"{}").unwrap();
        let outputs = [("repo_map", artifact.as_path())];
        let mut stamp = ArtifactStamp::of(inputs(1), None, &outputs).unwrap();
        assert_eq!(stamp.map_engine, CONSUMER_MAP_ENGINE);
        assert!(stamp.still_current(&inputs(1), &outputs));
        stamp.map_engine = "some-other-mapper".to_string();
        assert!(!stamp.still_current(&inputs(1), &outputs));
        let _ = fs::remove_dir_all(&dir);
    }

    /// The record for a role is found by role, and it is the one that answers
    /// for the file on disk.
    #[test]
    fn a_consumer_reads_an_artifact_by_role_and_checks_it_against_disk() {
        let dir = stamp_dir("role");
        let map = dir.join("repo_map.json");
        let graph = dir.join("code_graph.json");
        fs::write(&map, br#"{"map_engine":"devmap-rust"}"#).unwrap();
        fs::write(&graph, br#"{"meta":{}}"#).unwrap();
        let outputs = [("repo_map", map.as_path()), ("code_graph", graph.as_path())];
        let stamp = ArtifactStamp::of(inputs(1), None, &outputs).unwrap();

        let record = stamp.record("code_graph").expect("the role is described");
        assert_eq!(record.path, graph.to_string_lossy());
        assert!(record.still_describes_disk());
        assert!(stamp.record("no_such_role").is_none());

        // Once the file moves, the stamp no longer answers for it — which is
        // what stops a consumer quoting an engine for bytes it never saw.
        fs::write(&graph, br#"{"meta":{"tampered":true}}"#).unwrap();
        assert!(!stamp
            .record("code_graph")
            .expect("still described")
            .still_describes_disk());
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// Concurrent writers to one artifact must all succeed.
    ///
    /// `path.with_extension("tmp")` gave every writer the same temp name: the
    /// first rename moved it away and the rest failed with ENOENT. Measured
    /// through the CLI at 24-way concurrency, **8 of 24** `dev map` workers died
    /// in `manifest` with `No such file or directory (os error 2)`. The store
    /// itself was already safe (SC28); this was the last unguarded writer.
    ///
    /// Threads, not processes, so the test is cheap — the pid component of the
    /// temp name is constant here, which means this exercises exactly the
    /// same-process collision the sequence counter exists to prevent.
    #[test]
    fn concurrent_writers_to_one_artifact_all_succeed() {
        let dir = tempdir();
        let target = Arc::new(dir.join("repo_map.json"));

        let handles: Vec<_> = (0..16)
            .map(|worker| {
                let target = Arc::clone(&target);
                thread::spawn(move || {
                    let body = format!("{{\"worker\": {worker}}}");
                    write_atomic(&target, body.as_bytes())
                })
            })
            .collect();

        for (worker, handle) in handles.into_iter().enumerate() {
            let outcome = handle.join().expect("writer panicked");
            assert!(
                outcome.is_ok(),
                "writer {worker} failed: {:?}",
                outcome.err()
            );
        }

        // Exactly one payload survives, and it is one a writer actually wrote —
        // never a truncated or interleaved file.
        let final_text = fs::read_to_string(target.as_path()).expect("artifact must exist");
        assert!(
            (0..16).any(|worker| final_text == format!("{{\"worker\": {worker}}}")),
            "surviving artifact is not any writer's complete payload: {final_text}"
        );

        // No temp file may outlive the write; a stray one is what the next run
        // would trip over.
        let strays: Vec<_> = fs::read_dir(&dir)
            .expect("readable dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "temp files left behind: {strays:?}");
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "devmap-artifacts-{}-{}",
            std::process::id(),
            WRITE_SEQUENCE.load(std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&base).expect("temp dir");
        base
    }
}
