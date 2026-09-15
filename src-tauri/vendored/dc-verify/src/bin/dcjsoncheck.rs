//! `dcjsoncheck` — runs commands for real and audits what lands on stdout.
//!
//! DevCouncil's `--json` contract is one JSON object on stdout with every
//! diagnostic on stderr. Python's `CliRunner` cannot prove it: under Click 8.4
//! `result.output` is the two streams merged, so a banner that leaked onto
//! stdout reads exactly like one that went to stderr. Only a real process, with
//! two real file descriptors, settles it.
//!
//! So this spawns each command as a subprocess, captures stdout separately from
//! stderr, and requires stdout to be exactly one JSON value (or, with
//! `mode=jsonl`, one per line). It is the ratchet: point it at a manifest of
//! invocations and it fails the build when any of them regresses.
//!
//! Input is a manifest on stdin, one invocation per line, fields separated by
//! TAB — deliberately not a shell string, so nothing here has to quote, and no
//! invocation can accidentally acquire a shell:
//!
//! ```text
//! # comment lines and blank lines are ignored
//! mode=json<TAB>uv<TAB>run<TAB>dev<TAB>cost<TAB>budget<TAB>--json
//! mode=jsonl<TAB>uv<TAB>run<TAB>dev<TAB>trace<TAB>tail<TAB>--since<TAB>0
//! ```
//!
//! Output is one JSON object on stdout — this binary holds itself to the
//! contract it audits — and exit 0 when every invocation passed, 1 when any
//! failed its contract, 2 when the run itself could not be carried out. That
//! last split is the point: a manifest that could not be read must never exit 0
//! like a manifest whose invocations all passed.

use std::env;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

use dc_verify::json_stdout::{ContractError, holds_exactly_one_value, holds_json_lines};

fn main() -> ExitCode {
    let options = match Options::from_args(env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => return fail_hard(&message),
    };

    let manifest = match read_manifest() {
        Ok(manifest) => manifest,
        Err(message) => return fail_hard(&message),
    };

    let invocations = match parse_manifest(&manifest) {
        Ok(invocations) if invocations.is_empty() => {
            // An empty manifest is an operational failure, not a clean pass.
            // "Nothing to check" reported as success is how a broken harness
            // comes to mean "everything is fine".
            return fail_hard("the manifest listed no invocations");
        }
        Ok(invocations) => invocations,
        Err(message) => return fail_hard(&message),
    };

    let mut results = Vec::with_capacity(invocations.len());
    for invocation in &invocations {
        match run_one(invocation, options.cwd.as_deref(), options.deadline) {
            Ok(result) => results.push(result),
            Err(message) => return fail_hard(&message),
        }
    }

    let failed = results.iter().filter(|r| !r.passed).count();
    println!("{}", report(&results, failed));
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Reports an operational failure — the run could not be carried out — as
/// exit 2 with a payload, never as a silent success or an empty stdout.
fn fail_hard(message: &str) -> ExitCode {
    println!("{{\"ok\":false,\"error\":{}}}", quote(message));
    ExitCode::from(2)
}

struct Options {
    cwd: Option<PathBuf>,
    deadline: Duration,
}

impl Options {
    fn from_args(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut cwd = None;
        let mut deadline = INVOCATION_DEADLINE;
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cwd" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--cwd needs a directory".to_string())?;
                    cwd = Some(PathBuf::from(value));
                }
                // A harness that runs someone else's commands needs the bound
                // to be the operator's: CI wants it well under its own job
                // timeout, and a slow integration check may want it higher.
                "--deadline-secs" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--deadline-secs needs a whole number".to_string())?;
                    let secs: u64 = value
                        .parse()
                        .map_err(|_| format!("--deadline-secs {value:?} is not a whole number"))?;
                    if secs == 0 {
                        return Err("--deadline-secs must be at least 1".to_string());
                    }
                    deadline = Duration::from_secs(secs);
                }
                other => {
                    return Err(format!(
                        "unknown argument {other:?}; \
usage: dcjsoncheck [--cwd DIR] [--deadline-secs N] < manifest"
                    ));
                }
            }
        }
        Ok(Self { cwd, deadline })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// stdout must be exactly one JSON value.
    Json,
    /// stdout must be one JSON value per non-empty line.
    JsonLines,
}

#[derive(Debug)]
struct Invocation {
    line_number: usize,
    mode: Mode,
    argv: Vec<String>,
}

#[derive(Debug)]
struct Outcome {
    label: String,
    passed: bool,
    exit_code: Option<i32>,
    detail: Option<String>,
}

fn parse_manifest(text: &str) -> Result<Vec<Invocation>, String> {
    let mut invocations = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw.trim_end_matches(['\r']);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let mode = match fields.next() {
            Some("mode=json") => Mode::Json,
            Some("mode=jsonl") => Mode::JsonLines,
            Some(other) => {
                return Err(format!(
                    "line {line_number}: first field must be mode=json or mode=jsonl, got {other:?}"
                ));
            }
            None => unreachable!("split always yields at least one field"),
        };
        let argv: Vec<String> = fields
            .filter(|field| !field.is_empty())
            .map(str::to_string)
            .collect();
        if argv.is_empty() {
            return Err(format!(
                "line {line_number}: no command after the mode field"
            ));
        }
        invocations.push(Invocation {
            line_number,
            mode,
            argv,
        });
    }
    Ok(invocations)
}

/// The most a manifest may weigh. One byte over is taken so the cap can be
/// *detected*: a manifest stopped exactly at the limit is very likely still a
/// well-formed prefix, and running its first N invocations while silently
/// dropping the rest is a checked-everything report over a partial list.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// The most of one command's stdout or stderr that is kept. Past this the
/// invocation has already failed its contract — a JSON contract check reads one
/// value or a line stream, not a megabyte — so the excess is drained and
/// dropped rather than buffered whole.
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// Default wall clock for one checked invocation, overridable with
/// `--deadline-secs`.
///
/// These are CLI calls that print a JSON value and exit; a command still alive
/// after this is wedged, not working. Without a deadline the harness inherited
/// the hang, and "the manifest never finished" is not a contract result.
const INVOCATION_DEADLINE: Duration = Duration::from_secs(120);

fn read_manifest() -> Result<String, String> {
    let mut manifest = String::new();
    let read = std::io::stdin()
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut manifest)
        .map_err(|err| format!("could not read the manifest from stdin: {err}"))?;
    if read as u64 > MAX_MANIFEST_BYTES {
        return Err(format!(
            "the manifest is larger than the {MAX_MANIFEST_BYTES}-byte limit"
        ));
    }
    Ok(manifest)
}

fn run_one(
    invocation: &Invocation,
    cwd: Option<&std::path::Path>,
    deadline: Duration,
) -> Result<Outcome, String> {
    let mut command = Command::new(&invocation.argv[0]);
    command.args(&invocation.argv[1..]);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    // The workspace's one bounded runner. It owns the properties a harness
    // cannot get right by hand: both pipes drained on their own threads (a
    // reader that takes stdout to EOF *before* touching stderr deadlocks the
    // moment a child fills the stderr pipe), a wall-clock deadline, and a kill
    // that reaches the process group rather than the direct child alone. It
    // also closes stdin, so a command that reads stdin — `dev apply-patch`
    // does — still sees EOF instead of blocking the harness forever.
    let label = invocation.argv.join(" ");
    let captured = match dc_proc::run_bounded(
        &mut command,
        dc_proc::Bounds {
            deadline,
            stdout_cap: MAX_CAPTURE_BYTES,
            stderr_cap: MAX_CAPTURE_BYTES,
        },
    ) {
        Ok(captured) => captured,
        // A command that never exits is a failed invocation, not a harness
        // error: the manifest named it, it ran, and it did not answer.
        Err(dc_proc::Failure::Deadline { deadline, .. }) => {
            return Ok(Outcome {
                label,
                passed: false,
                exit_code: None,
                detail: Some(format!("did not exit within {deadline:?} and was killed")),
            });
        }
        Err(err) => return Err(format!("line {}: {err}", invocation.line_number)),
    };

    let exit_code = captured.status.code();
    // Output past the cap is a contract failure in its own right: the check
    // below would be reading a prefix and calling it the whole answer.
    if captured.stdout_truncated {
        return Ok(Outcome {
            label,
            passed: false,
            exit_code,
            detail: Some(format!(
                "stdout exceeded the {MAX_CAPTURE_BYTES}-byte capture limit"
            )),
        });
    }

    // Invalid UTF-8 on stdout is itself a contract violation, so it is checked
    // rather than papered over — but it is reported as a failed invocation, not
    // as a harness error, because the command did run.
    let stdout = match String::from_utf8(captured.stdout) {
        Ok(text) => text,
        Err(_) => {
            return Ok(Outcome {
                label,
                passed: false,
                exit_code,
                detail: Some("stdout was not valid UTF-8".to_string()),
            });
        }
    };

    let verdict = match invocation.mode {
        Mode::Json => holds_exactly_one_value(&stdout),
        Mode::JsonLines => holds_json_lines(&stdout),
    };
    Ok(match verdict {
        Ok(()) => Outcome {
            label,
            passed: true,
            exit_code,
            detail: None,
        },
        Err(err) => Outcome {
            label,
            passed: false,
            exit_code,
            detail: Some(describe(&err)),
        },
    })
}

fn describe(err: &ContractError) -> String {
    err.to_string()
}

fn report(results: &[Outcome], failed: usize) -> String {
    let mut json = String::from("{\"ok\":");
    json.push_str(if failed == 0 { "true" } else { "false" });
    // Both numbers, always: a caller must never have to infer coverage from a
    // failure count, and "0 failures" out of an unstated total says nothing.
    json.push_str(&format!(
        ",\"checked\":{},\"failed\":{},\"invocations\":[",
        results.len(),
        failed
    ));
    for (index, result) in results.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"command\":{},\"passed\":{},\"exit_code\":{},\"error\":{}}}",
            quote(&result.label),
            result.passed,
            match result.exit_code {
                Some(code) => code.to_string(),
                None => "null".to_string(),
            },
            match &result.detail {
                Some(detail) => quote(detail),
                None => "null".to_string(),
            }
        ));
    }
    json.push_str("]}");
    json
}

/// Minimal RFC 8259 string escaping — this crate has not earned a JSON
/// dependency, and the output it produces is its own small, fixed shape.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_skips_comments_and_blank_lines() {
        let manifest = "# a comment\n\nmode=json\techo\t{}\n   \nmode=jsonl\techo\t{}\n";
        let parsed = parse_manifest(manifest).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].mode, Mode::Json);
        assert_eq!(parsed[0].argv, vec!["echo", "{}"]);
        assert_eq!(parsed[1].mode, Mode::JsonLines);
        assert_eq!(parsed[1].line_number, 5);
    }

    #[test]
    fn manifest_rejects_a_missing_or_unknown_mode() {
        assert!(parse_manifest("echo\t{}\n").is_err());
        assert!(parse_manifest("mode=yaml\techo\t{}\n").is_err());
        assert!(parse_manifest("mode=json\n").is_err());
    }

    #[test]
    fn manifest_reports_the_line_a_bad_entry_came_from() {
        let err = parse_manifest("mode=json\techo\t{}\nmode=nope\techo\n").unwrap_err();
        assert!(err.starts_with("line 2:"), "got {err:?}");
    }

    /// A command that never exits used to hang the harness with it. `exec` so
    /// the deadline's group kill reaches the sleep itself rather than a shell
    /// that outlives it.
    #[test]
    fn a_command_that_never_exits_is_a_failed_invocation_not_a_hung_harness() {
        let invocation = Invocation {
            line_number: 1,
            mode: Mode::Json,
            argv: vec!["sh".into(), "-c".into(), "exec sleep 120".into()],
        };
        let started = std::time::Instant::now();
        let outcome = run_one(&invocation, None, Duration::from_secs(1)).unwrap();
        let elapsed = started.elapsed();

        assert!(
            !outcome.passed,
            "a command that never answered is not a pass"
        );
        assert_eq!(
            outcome.exit_code, None,
            "there is no exit status for a child that was killed"
        );
        let detail = outcome.detail.unwrap_or_default();
        assert!(
            detail.contains("did not exit"),
            "the detail must say what happened, got {detail:?}"
        );
        assert!(
            elapsed < Duration::from_secs(30),
            "returned after {elapsed:?}; the deadline did not bound it"
        );
    }

    /// A child that fills stderr while stdout stays open deadlocks any reader
    /// that drains stdout to EOF first. Both pipes must be drained
    /// concurrently, which is what the shared runner does.
    #[test]
    fn a_child_flooding_stderr_does_not_deadlock_the_reader() {
        let invocation = Invocation {
            line_number: 1,
            mode: Mode::Json,
            argv: vec![
                "sh".into(),
                "-c".into(),
                // Far past any pipe buffer, written before stdout closes.
                "yes xxxxxxxxxxxxxxxx | head -c 4000000 >&2; echo '{\"ok\":true}'".into(),
            ],
        };
        let started = std::time::Instant::now();
        let outcome = run_one(&invocation, None, Duration::from_secs(30)).unwrap();
        let elapsed = started.elapsed();

        assert!(
            outcome.passed,
            "stderr flood changed the verdict: {:?}",
            outcome.detail
        );
        assert!(
            elapsed < Duration::from_secs(25),
            "took {elapsed:?}; a deadlocked reader only ends at the deadline"
        );
    }

    /// The cap is a refusal, not a silent truncation: checking a prefix and
    /// reporting a pass is the failure mode the bound exists to prevent.
    #[test]
    fn stdout_past_the_capture_cap_fails_the_invocation() {
        let invocation = Invocation {
            line_number: 1,
            mode: Mode::Json,
            argv: vec![
                "sh".into(),
                "-c".into(),
                format!(
                    "yes xxxxxxxxxxxxxxxx | head -c {}",
                    MAX_CAPTURE_BYTES + (1 << 20)
                ),
            ],
        };
        let outcome = run_one(&invocation, None, Duration::from_secs(60)).unwrap();
        assert!(!outcome.passed);
        let detail = outcome.detail.unwrap_or_default();
        assert!(
            detail.contains("capture limit"),
            "the detail must name the bound, got {detail:?}"
        );
    }

    #[test]
    fn the_deadline_flag_is_parsed_and_validated() {
        let ok = Options::from_args(["--deadline-secs".to_string(), "7".to_string()].into_iter())
            .unwrap();
        assert_eq!(ok.deadline, Duration::from_secs(7));
        assert_eq!(
            Options::from_args([].into_iter()).unwrap().deadline,
            INVOCATION_DEADLINE
        );
        for bad in ["0", "-1", "abc", ""] {
            assert!(
                Options::from_args(["--deadline-secs".to_string(), bad.to_string()].into_iter())
                    .is_err(),
                "--deadline-secs {bad:?} was accepted"
            );
        }
    }

    #[test]
    fn a_command_emitting_one_object_passes() {
        let invocation = Invocation {
            line_number: 1,
            mode: Mode::Json,
            argv: vec!["echo".into(), "{\"ok\": true}".into()],
        };
        let outcome = run_one(&invocation, None, INVOCATION_DEADLINE).unwrap();
        assert!(outcome.passed, "detail: {:?}", outcome.detail);
        assert_eq!(outcome.exit_code, Some(0));
    }

    #[test]
    fn a_command_prefixing_its_payload_fails_even_though_it_exited_zero() {
        // The exact shape `dev cost budget --json --set 5.00` had: exit 0, and
        // unparseable stdout. Exit status alone would have called this a pass.
        let invocation = Invocation {
            line_number: 1,
            mode: Mode::Json,
            argv: vec!["echo".into(), "Set budget = 5.00\n{\"ok\": true}".into()],
        };
        let outcome = run_one(&invocation, None, INVOCATION_DEADLINE).unwrap();
        assert!(!outcome.passed);
        assert_eq!(outcome.exit_code, Some(0));
        assert!(
            outcome
                .detail
                .as_deref()
                .unwrap()
                .contains("does not begin"),
            "got {:?}",
            outcome.detail
        );
    }

    #[test]
    fn a_command_printing_nothing_fails_rather_than_passing_vacuously() {
        let invocation = Invocation {
            line_number: 1,
            mode: Mode::Json,
            argv: vec!["true".into()],
        };
        let outcome = run_one(&invocation, None, INVOCATION_DEADLINE).unwrap();
        assert!(!outcome.passed);
        assert!(
            outcome.detail.as_deref().unwrap().contains("zero objects"),
            "got {:?}",
            outcome.detail
        );
    }

    #[test]
    fn stderr_is_ignored_because_that_is_where_diagnostics_belong() {
        let invocation = Invocation {
            line_number: 1,
            mode: Mode::Json,
            argv: vec![
                "sh".into(),
                "-c".into(),
                "echo 'a diagnostic' >&2; echo '{\"ok\": true}'".into(),
            ],
        };
        let outcome = run_one(&invocation, None, INVOCATION_DEADLINE).unwrap();
        assert!(outcome.passed, "detail: {:?}", outcome.detail);
    }

    #[test]
    fn a_nonzero_exit_still_owes_stdout_one_object() {
        let failing = Invocation {
            line_number: 1,
            mode: Mode::Json,
            argv: vec![
                "sh".into(),
                "-c".into(),
                "echo '{\"ok\": false}'; exit 2".into(),
            ],
        };
        let outcome = run_one(&failing, None, INVOCATION_DEADLINE).unwrap();
        assert!(
            outcome.passed,
            "a failing command may still honour the contract"
        );
        assert_eq!(outcome.exit_code, Some(2));

        let silent = Invocation {
            line_number: 1,
            mode: Mode::Json,
            argv: vec!["sh".into(), "-c".into(), "echo 'boom' >&2; exit 2".into()],
        };
        let outcome = run_one(&silent, None, INVOCATION_DEADLINE).unwrap();
        assert!(
            !outcome.passed,
            "exiting non-zero does not excuse an empty stdout"
        );
    }

    #[test]
    fn a_command_that_does_not_exist_is_a_harness_error_not_a_failed_check() {
        let invocation = Invocation {
            line_number: 7,
            mode: Mode::Json,
            argv: vec!["definitely-not-a-real-binary-xyzzy".into()],
        };
        let err = run_one(&invocation, None, INVOCATION_DEADLINE).unwrap_err();
        assert!(err.starts_with("line 7:"), "got {err:?}");
    }

    #[test]
    fn the_report_carries_both_numbers_and_is_itself_one_json_object() {
        let results = vec![
            Outcome {
                label: "dev cost budget --json".into(),
                passed: true,
                exit_code: Some(0),
                detail: None,
            },
            Outcome {
                label: "dev go --json".into(),
                passed: false,
                exit_code: Some(1),
                detail: Some("stdout held no JSON value at all (zero objects)".into()),
            },
        ];
        let json = report(&results, 1);
        assert!(
            holds_exactly_one_value(&json).is_ok(),
            "report was not valid JSON: {json}"
        );
        assert!(json.contains("\"checked\":2"));
        assert!(json.contains("\"failed\":1"));
        assert!(json.contains("\"ok\":false"));
    }

    #[test]
    fn quote_escapes_what_would_otherwise_break_the_report() {
        assert_eq!(quote("a \"b\" c"), "\"a \\\"b\\\" c\"");
        assert_eq!(quote("line\nbreak"), "\"line\\nbreak\"");
        assert_eq!(quote("tab\there"), "\"tab\\there\"");
    }
}
