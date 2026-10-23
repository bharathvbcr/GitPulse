//! The gate scope an item carries (schema 14).
//!
//! An item's `scope` object is what the write gate enforces against while a
//! lease holder works the item: the files it may touch, what it must not, the
//! commands it may run, the tests that prove it, and the requirements it
//! answers to. It is the one definition of a task's authority, so it is
//! validated where it enters (`items.put`) and refused loudly, with the typed
//! code `invalid_scope`, rather than stored and discovered by whoever reads it
//! next. The reader still re-checks every column it embeds raw.
//!
//! The field follows the `locked_fields` rule in `put_item`: a request that
//! omits `scope` keeps the stored one, and only an explicit object (an empty
//! one clears it) changes it. A host that does not edit scope — the board, an
//! agent's status move — must not erase or widen the authority it carries.

use super::{Error, Input, Result};
use rusqlite::OptionalExtension;

/// What a plan may permit for one planned file. Anything else is refused: the
/// gate's lending rungs compare against exact names, so a near-miss such as
/// `read-only` would stop being read-only while still lending its siblings.
pub const ALLOWED_CHANGES: &[&str] = &["create", "modify", "delete", "read_only"];

/// The most entries any one scope list holds. Roughly a thousand paths is far
/// past an honest task and far short of anything that threatens a request.
pub const MAX_ENTRIES: usize = 1024;
/// The longest path, glob, command or identifier a scope entry may be.
pub const MAX_ENTRY_BYTES: usize = 4096;
/// The longest planned path. Filesystems cap a path far below this.
pub const MAX_PATH_BYTES: usize = 1024;
/// The ceiling on the whole object, which bounds the column, the argv the CLI
/// carries it over, and what an item can widen into.
pub const MAX_SCOPE_BYTES: usize = 256 * 1024;

const FIELDS: &[&str] = &[
    "planned_files",
    "forbidden_changes",
    "allowed_commands",
    "expected_tests",
    "requirement_ids",
    "acceptance_criterion_ids",
    "difficulty",
];
const LISTS: &[&str] = &[
    "forbidden_changes",
    "allowed_commands",
    "expected_tests",
    "requirement_ids",
    "acceptance_criterion_ids",
];
const PLANNED_FILE_FIELDS: &[&str] = &["path", "allowed_change", "reason"];

fn refuse(message: impl Into<String>) -> Error {
    Error {
        code: "invalid_scope",
        message: format!("scope: {}", message.into()),
    }
}

/// Re-codes a field-level refusal as `invalid_scope`; a storage fault keeps its
/// own code.
fn scoped(error: Error) -> Error {
    if error.code == "invalid_input" {
        refuse(error.message)
    } else {
        error
    }
}

/// Characters that make a path or command read differently from how it
/// compares: control characters, and the invisible bidirectional and zero-width
/// formatting characters that make one string look like another.
fn is_disguising(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

fn check_text(what: &str, value: &str) -> Result<()> {
    if value.chars().any(is_disguising) {
        return Err(refuse(format!(
            "{what} {value:?} holds a control or invisible formatting character"
        )));
    }
    Ok(())
}

/// A planned path is repository-relative, forward-slashed, and stays inside the
/// repository. Unicode is allowed; ambiguity is not: no absolute path, no
/// drive letter, no `..` segment, no backslash.
fn check_path(path: &str) -> Result<()> {
    check_text("planned path", path)?;
    if path.len() > MAX_PATH_BYTES {
        return Err(refuse(format!(
            "planned path of {} bytes exceeds {MAX_PATH_BYTES}",
            path.len()
        )));
    }
    if path.contains('\\') {
        return Err(refuse(format!(
            "planned path {path:?} contains a backslash; use forward slashes"
        )));
    }
    let bytes = path.as_bytes();
    if path.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err(refuse(format!(
            "planned path {path:?} is absolute; a plan names repository-relative paths"
        )));
    }
    if path.split('/').any(|segment| segment == "..") {
        return Err(refuse(format!(
            "planned path {path:?} has a '..' segment and could leave the repository"
        )));
    }
    Ok(())
}

/// Validates the request's `scope` and returns it as minified JSON text, or
/// `None` when the request names none.
pub(super) fn validate(input: &Input<'_>) -> Result<Option<String>> {
    match input.kind("scope")?.as_deref() {
        None | Some("null") => return Ok(None),
        Some("object") => {}
        Some(_) => return Err(refuse("must be an object")),
    }
    let text: String =
        input
            .conn
            .query_row("SELECT json_extract(?1,'$.scope')", [input.raw], |r| {
                r.get(0)
            })?;
    if text.len() > MAX_SCOPE_BYTES {
        return Err(refuse(format!(
            "{} bytes exceeds the {MAX_SCOPE_BYTES}-byte ceiling on one item's scope",
            text.len()
        )));
    }
    let scope = Input {
        conn: input.conn,
        raw: &text,
    };
    scope.fields(FIELDS).map_err(scoped)?;
    for list in LISTS {
        for entry in scope
            .strings(list, MAX_ENTRIES, MAX_ENTRY_BYTES, false)
            .map_err(scoped)?
        {
            check_text(list, &entry)?;
        }
    }
    if let Some(difficulty) = scope.text("difficulty", 64).map_err(scoped)? {
        check_text("difficulty", &difficulty)?;
    }
    planned_files(&scope)?;
    Ok(Some(text))
}

/// `planned_files` is an array of paths, or of `{path, allowed_change?, reason?}`
/// objects; a bare path means `modify`.
fn planned_files(scope: &Input<'_>) -> Result<()> {
    match scope.kind("planned_files")?.as_deref() {
        None | Some("null") => return Ok(()),
        Some("array") => {}
        Some(_) => return Err(refuse("planned_files must be an array")),
    }
    let mut stmt = scope
        .conn
        .prepare("SELECT type,value FROM json_each(?1,'$.planned_files')")?;
    let mut rows = stmt.query([scope.raw])?;
    let mut count = 0usize;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > MAX_ENTRIES {
            return Err(refuse(format!(
                "planned_files holds at most {MAX_ENTRIES} entries"
            )));
        }
        let kind: String = row.get(0)?;
        match kind.as_str() {
            "text" => check_nonblank_path(&row.get::<_, String>(1)?)?,
            "object" => {
                let value: String = row.get(1)?;
                let entry = Input {
                    conn: scope.conn,
                    raw: &value,
                };
                entry.fields(PLANNED_FILE_FIELDS).map_err(scoped)?;
                let path = entry
                    .required_text("path", MAX_PATH_BYTES)
                    .map_err(scoped)?;
                check_nonblank_path(&path)?;
                if let Some(change) = entry.text("allowed_change", 32).map_err(scoped)?
                    && !ALLOWED_CHANGES.contains(&change.as_str())
                {
                    return Err(refuse(format!(
                        "allowed_change {change:?} for {path:?} is not one of {}",
                        ALLOWED_CHANGES.join(", ")
                    )));
                }
                if let Some(reason) = entry.text("reason", MAX_ENTRY_BYTES).map_err(scoped)? {
                    check_text("reason", &reason)?;
                }
            }
            _ => {
                return Err(refuse(
                    "planned_files entries are paths or {path, allowed_change} objects",
                ));
            }
        }
    }
    Ok(())
}

fn check_nonblank_path(path: &str) -> Result<()> {
    if path.trim().is_empty() {
        return Err(refuse("a planned path must not be blank"));
    }
    check_path(path)
}

/// Writes the scope into `body` — the request's own, or, when the request names
/// none, the one the item already holds.
pub(super) fn apply(input: &Input<'_>, id: &str, body: String) -> Result<String> {
    let scope = match validate(input)? {
        Some(scope) => Some(scope),
        None => input
            .conn
            .query_row(
                "SELECT json_extract(body,'$.scope') FROM work_items WHERE id=?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten(),
    };
    match scope {
        None => Ok(body),
        Some(scope) => Ok(input.conn.query_row(
            "SELECT json_set(?1,'$.scope',json(?2))",
            rusqlite::params![body, scope],
            |r| r.get(0),
        )?),
    }
}
