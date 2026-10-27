// EVASION: the lifting statement exists and is never reached.
mod common;

use gitpulse_lib::repository_trust::inspect;

#[allow(dead_code)]
fn never_called() {
    common::unmetered_spawns();
}

#[test]
fn reaches_the_gate_unlifted() {
    let _ = inspect(".");
}
