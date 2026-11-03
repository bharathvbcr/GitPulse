//! `RUSTC_WRAPPER`-compatible zero-copy content-addressed compiler cache binary.
//!
//! Invoked by Cargo (or `gitpulse-worktree build`) as:
//! `gitpulse-rustc <path-to-rustc> [rustc-args...]`

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help") {
        println!(
            "gitpulse-rustc — Zero-copy Blake3 + CoW reflink RUSTC_WRAPPER for parallel agent worktrees\n\n\
             USAGE:\n    \
             RUSTC_WRAPPER=gitpulse-rustc cargo build\n    \
             gitpulse-rustc <path-to-rustc> [rustc-args...]\n\n\
             ENVIRONMENT:\n    \
             GITPULSE_BUILD_CACHE_DIR   Explicit cache directory (defaults to <git-common-dir>/gitpulse/build-cache)\n    \
             GITPULSE_WORKTREE_ROOT     Active worktree root for path normalization"
        );
        std::process::exit(0);
    }
    if args.len() == 1 && (args[0] == "-V" || args[0] == "--version") {
        println!("gitpulse-rustc {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }
    let code = gitpulse_worktree::build_cache::run_rustc_wrapper(&args);
    std::process::exit(code);
}
