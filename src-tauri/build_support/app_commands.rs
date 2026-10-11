//! The one reading of `tauri::generate_handler![..]` in `src/lib.rs`.
//!
//! `build.rs` turns the result into the app's ACL manifest, and
//! `tests/acl_contract.rs` compares it with the capability files, so both
//! read the handler list the same way and neither keeps a second copy of it.

/// Command names registered in `generate_handler!`, in source order, without
/// duplicates. Panics when the macro is missing: an empty manifest would
/// silently deny every command, and an unparsed one would grant none of them.
pub fn handler_commands(lib_rs: &str) -> Vec<String> {
    const MARKER: &str = "generate_handler![";
    let start = lib_rs
        .find(MARKER)
        .unwrap_or_else(|| panic!("src/lib.rs has no {MARKER} list"))
        + MARKER.len();
    let rest = &lib_rs[start..];
    let end = rest
        .find("])")
        .unwrap_or_else(|| panic!("src/lib.rs {MARKER} list is not closed"));
    let mut out: Vec<String> = Vec::new();
    for line in rest[..end].lines() {
        let code = line.split("//").next().unwrap_or("");
        for entry in code.split(',') {
            let name = entry.trim().rsplit("::").next().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            assert!(
                name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "unreadable entry {entry:?} in generate_handler!"
            );
            if !out.iter().any(|existing| existing == name) {
                out.push(name.to_string());
            }
        }
    }
    assert!(!out.is_empty(), "generate_handler! lists no commands");
    out
}
