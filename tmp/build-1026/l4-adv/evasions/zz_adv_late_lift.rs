// EVASION: the lift exists in one fixture, but a second test reaches the gate through a path that
// never calls it. The lint reads the whole file once; it cannot tell the entry points apart.
mod common;

use gitpulse_lib::repository_trust::inspect;

fn fixture() {
    common::unmetered_spawns();
}

#[test]
fn a_test_that_uses_the_fixture() {
    fixture();
}

#[test]
fn a_test_that_does_not() {
    let _ = inspect(".");
}
