use super::*;

fn low(free: u64) -> Advisory {
    Advisory::LowDisk(crate::sysload::LowDisk {
        cause: crate::sysload::LowDiskCause::BelowFloor,
        free_bytes: Some(free),
        floor_bytes: 1,
        cap_bytes: None,
        skipped: vec!["node_modules".into()],
    })
}
fn suppressed(repo: &str) -> Option<u64> {
    with_advice(|all| all.iter().find(|a| a.repo == repo).map(|a| a.suppressed))
}
fn keys() -> usize {
    with_advice(|all| all.len())
}

#[test]
fn adv2_dedupe_same_detail_suppressed() {
    note_launch_advisory("/r/a", Some(low(5)));
    note_launch_advisory("/r/a", Some(low(5)));
    assert_eq!(suppressed("/r/a"), Some(1));
}
#[test]
fn adv2_dedupe_different_free_space_is_suppressed() {
    note_launch_advisory("/r/b", Some(low(5)));
    note_launch_advisory("/r/b", Some(low(6)));
    assert_eq!(
        suppressed("/r/b"),
        Some(1),
        "fluctuating free space defeats the dedupe"
    );
}
#[test]
fn adv2_cleared_then_reraised_within_window_is_suppressed() {
    note_launch_advisory("/r/c", Some(low(5)));
    note_launch_advisory("/r/c", None);
    note_launch_advisory("/r/c", Some(low(5)));
    assert_eq!(suppressed("/r/c"), Some(1));
}
#[test]
fn adv2_trailing_slash_is_one_repo() {
    let before = keys();
    note_launch_advisory("/r/d", Some(low(5)));
    note_launch_advisory("/r/d/", Some(low(5)));
    assert_eq!(keys(), before + 1, "two keys for one repo");
}
#[test]
fn adv2_eviction_keeps_an_active_repo() {
    note_launch_advisory("/r/e0", Some(low(5)));
    for i in 0..MAX_ADVICE_REPOS {
        note_launch_advisory("/r/e0", Some(low(5)));
        note_launch_advisory(&format!("/r/f{i}"), None);
    }
    assert!(
        suppressed("/r/e0").is_some(),
        "an active repo lost its dedupe state"
    );
}
