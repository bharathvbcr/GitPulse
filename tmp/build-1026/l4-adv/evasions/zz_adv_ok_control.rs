// CONTROL: a correctly switched suite. The lint MUST pass on this one alone.
mod common;

use gitpulse_lib::repository_trust::inspect;

#[test]
fn lifts_then_reaches_the_gate() {
    common::unmetered_spawns();
    let _ = inspect(".");
}
