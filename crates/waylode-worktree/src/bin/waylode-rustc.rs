//! `RUSTC_WRAPPER`-compatible zero-copy content-addressed compiler cache binary.
//!
//! Invoked by Cargo (or `waylode-worktree build`) as:
//! `waylode-rustc <path-to-rustc> [rustc-args...]`

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help") {
        println!(
            "waylode-rustc — Zero-copy Blake3 + CoW reflink RUSTC_WRAPPER for parallel agent worktrees\n\n\
             USAGE:\n    \
             RUSTC_WRAPPER=waylode-rustc cargo build\n    \
             waylode-rustc <path-to-rustc> [rustc-args...]\n\n\
             ENVIRONMENT:\n    \
             WAYLODE_BUILD_CACHE_DIR    Explicit cache directory (defaults to <git-common-dir>/gitpulse/build-cache; also reads GITPULSE_BUILD_CACHE_DIR)\n    \
             WAYLODE_WORKTREE_ROOT      Active worktree root for path normalization (also reads GITPULSE_WORKTREE_ROOT)"
        );
        std::process::exit(0);
    }
    if args.len() == 1 && (args[0] == "-V" || args[0] == "--version") {
        println!("waylode-rustc {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }
    let code = waylode_worktree::build_cache::run_rustc_wrapper(&args);
    std::process::exit(code);
}
