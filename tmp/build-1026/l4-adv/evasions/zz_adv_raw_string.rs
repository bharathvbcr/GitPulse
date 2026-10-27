// EVASION: the lifting statement is a line of a raw string literal (no quote on that line).
use gitpulse_lib::repository_trust::inspect;

const DOC: &str = r#"
how to lift the cap:
unmetered_spawns();
"#;

#[test]
fn reaches_the_gate_unlifted() {
    assert!(DOC.contains("unmetered"));
    let _ = inspect(".");
}
