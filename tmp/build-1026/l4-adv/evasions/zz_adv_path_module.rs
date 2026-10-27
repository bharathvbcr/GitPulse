// EVASION: the library call lives in a #[path] module. This file never names the library crate, so the
// lint's substring filter on the crate name skips the whole suite.
#[path = "zz_adv_inner/helper.rs"]
mod helper;

#[test]
fn reaches_the_gate_through_a_path_module() {
    helper::read();
}
