#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    gitpulse_lib::run();
    // The window closing is a normal return, but a `kill` or a logout is not:
    // exiting through `procguard` is what stops that return from overtaking a
    // shutdown sweep that is still killing this session's git subprocesses.
    gitpulse_lib::procguard::exit(0);
}
