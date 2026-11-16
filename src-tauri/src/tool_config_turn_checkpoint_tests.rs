//! `turn_checkpoints.enabled`: off by default, off when unreadable, and kept
//! through a save of an unrelated setting.

use super::*;

fn dir_with(text: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    if let Some(text) = text {
        fs::write(dir.path().join("tools.json"), text).unwrap();
    }
    dir
}

#[test]
fn it_is_off_by_default_and_when_the_file_is_absent() {
    assert!(!TurnCheckpointSettings::default().enabled);
    assert!(!ToolConfig::default().turn_checkpoints.enabled);
    assert!(!turn_checkpoint_settings_in(dir_with(None).path()).enabled);
    assert!(!turn_checkpoint_settings_in(dir_with(Some(r#"{"version":1}"#)).path()).enabled);
}

#[test]
fn it_reads_the_documented_key() {
    let on = dir_with(Some(r#"{"version":1,"turn_checkpoints":{"enabled":true}}"#));
    assert!(turn_checkpoint_settings_in(on.path()).enabled);
    let off = dir_with(Some(
        r#"{"version":1,"turn_checkpoints":{"enabled":false}}"#,
    ));
    assert!(!turn_checkpoint_settings_in(off.path()).enabled);
}

#[test]
fn an_unreadable_block_or_file_is_off_and_costs_no_other_setting() {
    for text in [
        r#"{"version":1,"turn_checkpoints":{"enabled":true,"surprise":1}}"#,
        r#"{"version":1,"turn_checkpoints":{"enabled":"yes"}}"#,
        r#"{"version":1,"turn_checkpoints":7}"#,
    ] {
        let dir = dir_with(Some(text));
        assert!(!turn_checkpoint_settings_in(dir.path()).enabled, "{text}");
        let cfg = read_disk(&dir.path().join("tools.json")).expect("the rest still parses");
        assert!(!cfg.turn_checkpoints.enabled);
    }
    let broken = dir_with(Some("{ not json"));
    assert!(!turn_checkpoint_settings_in(broken.path()).enabled);
}

#[test]
fn the_default_is_not_written_and_an_enabled_value_round_trips() {
    let text = serde_json::to_string(&ToolConfig::default()).unwrap();
    assert!(!text.contains("turn_checkpoints"), "{text}");
    let mut cfg = ToolConfig::default();
    cfg.turn_checkpoints.enabled = true;
    let text = serde_json::to_string(&cfg).unwrap();
    let back: ToolConfig = serde_json::from_str(&text).unwrap();
    assert!(back.turn_checkpoints.enabled);
}
