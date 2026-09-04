fn main() {
    embed_test_manifest();
    tauri_build::build()
}

/// Gives this crate's TEST binaries the same comctl32 version 6 dependency the
/// application binary gets from `tauri_build`.
///
/// `muda` and `wry` import `SetWindowSubclass`, `RemoveWindowSubclass` and
/// `DefSubclassProc`, which only comctl32 version 6 exports. Without a manifest
/// saying so, the loader binds the System32 copy (version 5), cannot resolve
/// them, and kills the process with STATUS_ENTRYPOINT_NOT_FOUND before `main`
/// runs -- which is how two integration suites failed on Windows without ever
/// reaching a test.
///
/// `rustc-link-arg-tests` applies to test targets only, so the application
/// binary keeps the manifest `tauri_build` embeds and nothing is emitted on
/// platforms whose linker has no such flag.
fn embed_test_manifest() {
    println!("cargo:rerun-if-changed=tests.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests.manifest");
    println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
