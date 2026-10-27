// EVASION: a local no-op named like the wrapper. The statement ends in `unmetered_spawns();`.
use gitpulse_lib::repository_trust::inspect;

fn unmetered_spawns() {}

#[test]
fn reaches_the_gate_unlifted() {
    unmetered_spawns();
    let _ = inspect(".");
}
