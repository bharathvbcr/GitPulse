//! Artifact writers with tmp+rename (V14) and fingerprint skip-on-unchanged.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::escape::{html_escape, json_script_escape};

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

/// Minimal subsystem map HTML with esc() at every sink (V1).
pub fn render_subsystem_map_html(
    title: &str,
    subsystems: &[(&str, &[String])],
    fp: &ArtifactFingerprint,
) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "<!-- fingerprint:{} -->\n<h1>{}</h1>\n<p>head={} built_at={} fp={}</p>\n",
        html_escape(&fp.fingerprint),
        html_escape(title),
        html_escape(&fp.generated_head),
        fp.built_at,
        html_escape(&fp.fingerprint)
    ));
    for (area, files) in subsystems {
        body.push_str(&format!("<h2>{}</h2>\n<ul>\n", html_escape(area)));
        for f in *files {
            body.push_str(&format!("<li>{}</li>\n", html_escape(f)));
        }
        body.push_str("</ul>\n");
    }
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body>{}</body></html>",
        html_escape(title),
        body
    )
}

/// Symbol explorer payload embedded in script tag (V2) with escaped title (V1).
pub fn render_symbol_explorer_html(
    title: &str,
    payload_json: &str,
    fp: &ArtifactFingerprint,
) -> String {
    let safe_title = html_escape(title);
    let safe_json = json_script_escape(payload_json);
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{safe_title}</title></head><body>\
         <!-- fingerprint:{} -->\
         <h1>{safe_title}</h1>\
         <p>staleness: head={} fp={}</p>\
         <script type=\"application/json\" id=\"payload\">{safe_json}</script>\
         </body></html>",
        html_escape(&fp.fingerprint),
        html_escape(&fp.generated_head),
        html_escape(&fp.fingerprint)
    )
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
        let html = render_subsystem_map_html("Test", &[("core", &["a.py".to_string()])], &fp());
        assert!(write_atomic(&path, html.as_bytes()).unwrap());
        assert!(!should_regenerate(&path, &fp()));
        let fp2 = ArtifactFingerprint {
            fingerprint: "other".into(),
            ..fp()
        };
        assert!(should_regenerate(&path, &fp2));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_v1_hostile_name_in_subsystem_html() {
        // closes V1
        let hostile = "x<img src=x onerror=alert(1)>.ts";
        let html = render_subsystem_map_html(hostile, &[("area", &[hostile.to_string()])], &fp());
        assert!(!html.contains("<img"));
        assert!(html.contains("x&lt;img"));
    }

    #[test]
    fn test_v1_v2_symbol_payload_is_inert_and_remains_valid_json() {
        let payload = serde_json::json!({
            "name": "</script><img src=x onerror=alert(1)>",
            "ampersand": "a&b"
        });
        let raw = serde_json::to_string(&payload).unwrap();
        let html = render_symbol_explorer_html("Symbols", &raw, &fp());
        let marker = "<script type=\"application/json\" id=\"payload\">";
        let start = html.find(marker).unwrap() + marker.len();
        let end = html[start..].find("</script>").unwrap() + start;
        let embedded = &html[start..end];

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(embedded).unwrap(),
            payload
        );
        assert_eq!(html.matches("</script>").count(), 1);
        assert!(!html.contains("<img"));
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
