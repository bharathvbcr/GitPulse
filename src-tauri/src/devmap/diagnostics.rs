//! DevMap command diagnostics. No stdin or successful query payload is logged.
//! Only stage numbers are logged live. Raw stderr is redacted as a whole at
//! completion so multiline credentials cannot leak through per-line records.
//!
//! Those stage numbers are also the only progress a build ever produces. A
//! cold index of a large repository is minutes of an empty pane otherwise, so
//! the same parser that records a stage emits it — the stage *number* and
//! nothing else, never the line it came from, because a stderr line is exactly
//! the surface this module redacts at completion rather than forwards live.

use super::cli::ResolvedDevmap;
use crate::engine::git_cli::{BoundedRun, OutputStream, ProcessObserver};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const PROGRESS_LINES: usize = 64;
const LINE_BYTES: usize = 4096;

/// Per-build stage progress, for the Map pane's status strip.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DevmapBuildProgress {
    /// Absolute repository path this build is for.
    pub repository: String,
    /// 1-based stage the kernel has reached.
    pub stage: u8,
    pub total_stages: u8,
}

static APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

/// Wired once at startup, beside the other emitters.
pub fn set_app_handle(handle: tauri::AppHandle) {
    let _ = APP.set(handle);
}

pub(super) struct CommandLog {
    id: String,
    /// Only builds report stages; status and preview runs never emit progress.
    repository: Option<String>,
    started: Instant,
    pending: Vec<u8>,
    stderr: Vec<u8>,
    stderr_observed: usize,
    lines: usize,
    stages: usize,
    oversized: bool,
}

impl CommandLog {
    pub(super) fn start(
        binary: &ResolvedDevmap,
        repo: &Path,
        args: &[String],
        deadline: Duration,
    ) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let log = Self {
            // A `--json` status run has no stages and a preview must not
            // masquerade as a build in the UI; only the command that reports
            // `[n/5]` gets an identity to emit under.
            repository: args
                .first()
                .filter(|first| *first == "build")
                .map(|_| repo.to_string_lossy().into_owned()),
            id: format!(
                "{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ),
            started: Instant::now(),
            pending: Vec::new(),
            stderr: Vec::new(),
            stderr_observed: 0,
            lines: 0,
            stages: 0,
            oversized: false,
        };
        log.record(json!({"event": "started", "repository": repo.to_string_lossy(), "repository_path_lossy": repo.to_str().is_none(), "binary": binary.path,
            "lookup": binary.lookup, "args": args, "deadline_ms": deadline.as_millis()}));
        log
    }

    fn record(&self, mut event: Value) {
        event["run_id"] = json!(self.id);
        event["elapsed_ms"] = json!(self.started.elapsed().as_millis());
        crate::logging::record_devmap(&format!("run_id={} {event}", self.id));
    }

    fn line(&mut self) {
        self.lines = self.lines.saturating_add(1);
        if !self.oversized {
            if let [b'[', stage @ b'1'..=b'5', b'/', b'5', b']', b' ', ..] = self.pending.as_slice()
            {
                self.stages = self.stages.saturating_add(1);
                if self.stages <= PROGRESS_LINES {
                    let stage = stage - b'0';
                    self.record(json!({"event": "stage", "stage": stage, "total_stages": 5}));
                    self.emit_stage(stage);
                }
            }
        }
        self.pending.clear();
        self.oversized = false;
    }

    /// Publish one stage to the UI. Bounded by the same `PROGRESS_LINES` cap as
    /// the log, and silent when no app handle exists (tests, the MCP binary).
    fn emit_stage(&self, stage: u8) {
        let Some(repository) = self.repository.as_ref() else {
            return;
        };
        use tauri::Emitter;
        if let Some(app) = APP.get() {
            let _ = app.emit(
                "devmap-build-progress",
                DevmapBuildProgress {
                    repository: repository.clone(),
                    stage,
                    total_stages: 5,
                },
            );
        }
    }

    pub(super) fn finish(
        mut self,
        result: &Result<BoundedRun, String>,
        protocol_error: Option<&str>,
    ) {
        if !self.pending.is_empty() || self.oversized {
            self.line();
        }
        let mut event = json!({"event": "finished", "stderr_lines_observed": self.lines,
            "stage_updates_shown": self.stages.min(PROGRESS_LINES),
            "stage_updates_omitted": self.stages.saturating_sub(PROGRESS_LINES)});
        event["protocol_error"] = json!(protocol_error);
        match result {
            Ok(run) => {
                event["exit_code"] = json!(run.status_code);
                event["ok"] = json!(
                    run.success
                        && run.incomplete.is_none()
                        && run.stderr_incomplete.is_none()
                        && protocol_error.is_none()
                );
                event["stdout_bytes"] = json!(run.stdout.len());
                event["stderr_bytes"] = json!(run.stderr.len());
                event["stdout_incomplete"] =
                    json!(run.incomplete.as_ref().map(|why| why.describe()));
                event["stderr_incomplete"] =
                    json!(run.stderr_incomplete.as_ref().map(|why| why.describe()));
                // The complete captured stderr is redacted before the existing
                // logger bounds it. It preserves errors after sampled progress.
                event["stderr"] = json!(String::from_utf8_lossy(&run.stderr));
                match serde_json::from_slice::<Value>(&run.stdout) {
                    Ok(report) => {
                        for key in [
                            "error",
                            "diagnostic_context",
                            "timings",
                            "progress_output",
                            "generation_id",
                            "files_indexed",
                            "files_failed",
                            "unchanged",
                        ] {
                            if let Some(value) = report.get(key) {
                                event[key] = value.clone();
                            }
                        }
                    }
                    Err(error) => event["json_error"] = json!(error.to_string()),
                }
            }
            Err(error) => {
                event["ok"] = json!(false);
                event["error"] = json!(error);
                event["exit_code"] = Value::Null;
                // The runner returns no BoundedRun on timeout/spawn failure.
                // Retain the observed prefix and state exactly what it covers.
                let truncated = self.stderr_observed > self.stderr.len();
                if truncated {
                    let end = self
                        .stderr
                        .iter()
                        .rposition(|&byte| byte == b'\n')
                        .map_or(0, |i| i + 1);
                    self.stderr.truncate(end);
                }
                event["stderr_observed_prefix"] = json!(String::from_utf8_lossy(&self.stderr));
                event["stderr_observed_bytes"] = json!(self.stderr_observed);
                event["stderr_retained_bytes"] = json!(self.stderr.len());
                event["stderr_prefix_truncated"] = json!(truncated);
                event["stderr_incomplete"] =
                    json!("only bytes observed before the runner failed; EOF not established");
            }
        }
        self.record(event);
    }
}

impl ProcessObserver for CommandLog {
    fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
        if !matches!(stream, OutputStream::Stderr) {
            return;
        }
        self.stderr_observed = self.stderr_observed.saturating_add(bytes.len());
        let take = bytes
            .len()
            .min((64 * 1024_usize).saturating_sub(self.stderr.len()));
        self.stderr.extend_from_slice(&bytes[..take]);
        for &byte in bytes {
            if byte == b'\n' {
                self.line();
            } else if self.pending.len() < LINE_BYTES {
                self.pending.push(byte);
            } else {
                self.oversized = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devmap::cli::DevmapLookup;

    fn log() -> CommandLog {
        CommandLog::start(
            &ResolvedDevmap {
                path: "/diagnostics/devmap".into(),
                lookup: DevmapLookup::PathSearch,
            },
            Path::new("/diagnostics/repo"),
            &["build".into()],
            Duration::from_secs(5),
        )
    }

    fn entries(id: &str) -> String {
        crate::logging::diagnostic_tail(500)
            .into_iter()
            .filter(|line| line.contains(&format!("\"run_id\":\"{id}\"")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_failed_runner_keeps_observed_stderr_without_claiming_eof() {
        let mut log = log();
        let id = log.id.clone();
        log.output(OutputStream::Stdout, b"PRIVATE_QUERY_OUTPUT");
        for chunk in "[2/5] resolving\nfailed to open café.sqlite"
            .as_bytes()
            .chunks(3)
        {
            log.output(OutputStream::Stderr, chunk);
        }
        log.finish(&Err("devmap timed out after 5s".into()), None);
        let text = entries(&id);
        assert!(text.contains("café.sqlite"));
        assert!(text.contains("EOF not established"));
        assert!(text.contains("\"stage\":2"));
        assert!(text.contains("\"ok\":false"));
        assert!(!text.contains("PRIVATE_QUERY_OUTPUT"));
    }

    #[test]
    fn output_floods_are_bounded_and_count_the_omitted_stage_updates() {
        let mut log = log();
        let id = log.id.clone();
        for _ in 0..70 {
            log.output(OutputStream::Stderr, b"[1/5] scanning\n");
        }
        log.output(OutputStream::Stderr, &vec![b'x'; 1024 * 1024]);
        assert!(log.pending.len() <= LINE_BYTES);
        assert!(log.stderr.len() <= 64 * 1024);
        log.finish(&Err("devmap timed out after 5s".into()), None);
        let text = entries(&id);
        assert!(text.contains("\"stage_updates_shown\":64"));
        assert!(text.contains("\"stage_updates_omitted\":6"));
        assert!(text.contains("\"stderr_prefix_truncated\":true"));
        assert!(text.len() < 64 * 1024);
    }
}
