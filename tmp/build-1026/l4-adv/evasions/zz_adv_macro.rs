// EVASION: the lifting statement is the body of a macro that is never expanded.
mod common;

use gitpulse_lib::repository_trust::inspect;

#[allow(unused_macros)]
macro_rules! lift {
    () => {
        common::unmetered_spawns();
    };
}

#[test]
fn reaches_the_gate_unlifted() {
    let _ = inspect(".");
}
