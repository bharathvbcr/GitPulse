//! Shared JSON-stdio CLI mechanics for the workspace's Rust helper binaries.
//!
//! `dcstore`, `dcverify`, `dcjsoncheck` and `dcgrep` all cross to Go over
//! single-line JSON on stdout. Keeping one UTF-8 argument collector, one version
//! flag recognizer, one version response formatter and one RFC 8259 string
//! quoter here keeps the four binaries from drifting on edge cases (non-UTF-8
//! arguments, closed stdout, or control-character escaping).

use std::ffi::OsString;

/// Prints a line of JSON to stdout, exiting silently as transport failure (1)
/// if stdout is closed instead of panicking on `println!`.
#[macro_export]
macro_rules! respond {
    ($($argument:tt)*) => {{
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        if out
            .write_fmt(format_args!("{}\n", format_args!($($argument)*)))
            .and_then(|()| out.flush())
            .is_err()
        {
            return std::process::ExitCode::FAILURE;
        }
    }};
}

/// Collects command-line arguments, refusing non-Unicode bytes instead of
/// panicking in `std::env::args()`.
pub fn collect_args() -> Result<Vec<String>, String> {
    collect_args_from(std::env::args_os().skip(1), None)
}

/// Like [`collect_args`], appending `note` when an argument is not valid UTF-8.
pub fn collect_args_with_note(note: &str) -> Result<Vec<String>, String> {
    collect_args_from(std::env::args_os().skip(1), Some(note))
}

/// Parses an iterator of `OsString` arguments into UTF-8 strings, failing with
/// position only (never echoing raw bytes) if an entry is not valid UTF-8.
pub fn collect_args_from<I>(args: I, note: Option<&str>) -> Result<Vec<String>, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut collected = Vec::new();
    for (index, arg) in args.into_iter().enumerate() {
        match arg.into_string() {
            Ok(value) => collected.push(value),
            Err(_) => {
                return Err(match note {
                    Some(suffix) => format!("argument {index} is not valid UTF-8{suffix}"),
                    None => format!("argument {index} is not valid UTF-8"),
                });
            }
        }
    }
    Ok(collected)
}

/// Reports whether an argument asks the binary for its build version.
pub fn is_version_arg(arg: &str) -> bool {
    matches!(arg, "--version" | "-V" | "-v" | "version")
}

/// Reports whether a whole argument list is a version request: exactly one
/// argument, and that argument a version form. A version form anywhere else
/// is a value (`--cwd version`, `--deadline-secs -v`, a path named `version`),
/// and answering it with a success object would report a run that checked
/// nothing as ok.
pub fn wants_version(args: &[String]) -> bool {
    matches!(args, [only] if is_version_arg(only))
}

/// Builds the canonical version JSON payload with optional extra raw JSON fields.
pub fn version_object(
    id: &str,
    component: &str,
    version: &str,
    extra_raw_fields: &[(&str, &str)],
) -> String {
    let mut json = format!(
        "{{\"ok\":true,\"id\":{},\"component\":{},\"version\":{}",
        quote(id),
        quote(component),
        quote(version)
    );
    for (key, raw_value) in extra_raw_fields {
        json.push(',');
        json.push_str(&quote(key));
        json.push(':');
        json.push_str(raw_value);
    }
    json.push('}');
    json
}

/// Escapes a string per RFC 8259 so control characters and quotes never break
/// single-line JSON payloads.
pub fn quote(value: &str) -> String {
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
    fn quote_escapes_rfc8259_control_and_quote_chars() {
        assert_eq!(quote("a \"b\" c"), "\"a \\\"b\\\" c\"");
        assert_eq!(quote("a\\b"), "\"a\\\\b\"");
        assert_eq!(quote("line\nbreak\r\t"), "\"line\\nbreak\\r\\t\"");
        assert_eq!(quote("\x00\x1f"), "\"\\u0000\\u001f\"");
    }

    #[test]
    fn is_version_arg_matches_documented_forms() {
        for flag in ["--version", "-V", "-v", "version"] {
            assert!(is_version_arg(flag), "{flag}");
        }
        for other in ["--ver", "-version", "health", ""] {
            assert!(!is_version_arg(other), "{other}");
        }
    }

    #[test]
    fn wants_version_only_for_a_lone_version_argument() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for flag in ["--version", "-V", "-v", "version"] {
            assert!(wants_version(&args(&[flag])), "{flag}");
        }
        for list in [
            &[][..],
            &["--cwd", "version"][..],
            &["--deadline-secs", "-v"][..],
            &["-v", "--cwd", "x"][..],
            &["health", "--version"][..],
            &["--version", "--version"][..],
        ] {
            assert!(!wants_version(&args(list)), "{list:?}");
        }
    }

    #[test]
    fn version_object_formats_base_and_extra_fields() {
        assert_eq!(
            version_object("dcverify", "dc-verify", "0.2.5", &[]),
            "{\"ok\":true,\"id\":\"dcverify\",\"component\":\"dc-verify\",\"version\":\"0.2.5\"}"
        );
        assert_eq!(
            version_object(
                "dcstore",
                "dc-store",
                "0.2.5",
                &[("workbench_schema", "16")]
            ),
            "{\"ok\":true,\"id\":\"dcstore\",\"component\":\"dc-store\",\"version\":\"0.2.5\",\"workbench_schema\":16}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn collect_args_from_refuses_non_utf8_without_echoing_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let args = vec![
            OsString::from("ok"),
            OsString::from_vec(b"secret-\xff-token".to_vec()),
        ];
        let err = collect_args_from(args, Some("; withheld")).unwrap_err();
        assert_eq!(err, "argument 1 is not valid UTF-8; withheld");
    }
}
