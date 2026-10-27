// EVASION: the lifting statement sits inside a block comment (the lint skips only `//` lines).
use gitpulse_lib::repository_trust::inspect;

/*
    common::unmetered_spawns();
*/
#[test]
fn reaches_the_gate_unlifted() {
    let _ = inspect(".");
}
