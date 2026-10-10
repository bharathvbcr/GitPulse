//! The host-load sampler runs only in the GitPulse app process.
//!
//! The ruling this pins: `gitpulsed`, the agent server and the hooks never
//! sample. Each would otherwise contend for the snapshot's writer election —
//! and a hook sampling per tool call would be the load it exists to measure.
//! So the one call that starts the app's sampler, `sysload::start_app`, must
//! sit in `lib.rs::run`'s setup, which only the app binary reaches, and no
//! other shipped binary may reach `run` or name the sampler at all.

mod common;

use std::path::{Path, PathBuf};

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Lines of `source` before its first `#[cfg(test)]` module: production code.
fn production(source: &str) -> &str {
    source
        .find("#[cfg(test)]\nmod tests")
        .map_or(source, |end| &source[..end])
}

#[test]
fn only_the_app_starts_the_sampler() {
    let mut files = Vec::new();
    rust_files(&crate_dir().join("src"), &mut files);
    assert!(files.len() > 100, "walked only {} files", files.len());
    let sampler_module = crate_dir().join("src").join("sysload");
    let mut starts = Vec::new();
    for file in &files {
        if file.starts_with(&sampler_module) {
            continue;
        }
        let source = std::fs::read_to_string(file).unwrap();
        for (n, line) in production(&source).lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("sysload::start_app(") || code.contains("sysload::start(") {
                let rel = file
                    .strip_prefix(crate_dir())
                    .unwrap()
                    .display()
                    .to_string();
                starts.push(format!("{rel}:{}", n + 1));
            }
        }
    }
    assert_eq!(starts.len(), 1, "the sampler is started from {starts:?}");
    assert!(starts[0].starts_with("src/lib.rs:"), "{starts:?}");

    // That one start is inside the function `run`'s setup calls.
    let lib = std::fs::read_to_string(crate_dir().join("src/lib.rs")).unwrap();
    let start_fn = lib
        .find("fn start_host_load_sampler")
        .expect("lib.rs names the function that starts the sampler");
    let start_call = lib.find("sysload::start_app(").unwrap();
    let run_fn = lib.find("pub fn run() {").expect("lib.rs has run()");
    assert!(
        start_fn < start_call && start_call < run_fn,
        "start_app moved out of start_host_load_sampler"
    );
    assert!(
        lib[run_fn..].contains("start_host_load_sampler(app.handle())"),
        "run()'s setup no longer starts the sampler"
    );
}

#[test]
fn no_shipped_binary_but_the_app_reaches_the_sampler() {
    for bin in common::shipped_binaries() {
        let source = std::fs::read_to_string(crate_dir().join(&bin)).unwrap();
        let reaches_run = source.contains("gitpulse_lib::run()");
        assert_eq!(
            reaches_run,
            bin == "src/main.rs",
            "{bin}: only the app binary may call gitpulse_lib::run(), which starts the sampler"
        );
        assert!(
            !source.contains("sysload::start"),
            "{bin} starts a host-load sampler of its own"
        );
    }
}
