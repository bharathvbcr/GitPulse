// EVASION: the in-process gate use is inside tests/common (`run_git(.., ["init"])` ends in
// `trust_repo`, which calls repository_trust::inspect/grant). This file never names the library crate.
mod common;

#[test]
fn reaches_the_gate_through_common() {
    let dir = std::env::temp_dir().join("zz-adv-common-helper");
    std::fs::create_dir_all(&dir).unwrap();
    common::run_git(&dir, &["init"]);
}
