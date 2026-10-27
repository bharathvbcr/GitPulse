// CONTROL: names the library and spawns through it, never lifts the cap. The lint MUST fail on this.
use gitpulse_lib::repository_trust::inspect;

#[test]
fn reaches_the_gate() {
    let _ = inspect(".");
}
