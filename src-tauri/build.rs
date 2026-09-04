fn main() {
    embed_test_manifest();
    tauri_build::build()
}

/// Gives this crate's TEST binaries an application manifest declaring the
/// comctl32 version 6 dependency, which the application binary already gets
/// from `tauri_build`.
///
/// Without it, two integration suites died on Windows with
/// STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) before `main` ran, so nothing in
/// them was ever checked. Only those two reach the window code in `muda` and
/// `wry`, which is why only they failed.
///
/// What was verified on the runner: the failing binaries import comctl32
/// alongside the rest of the GUI stack, and every DLL they import resolves in
/// System32 -- so the loader was failing on a missing EXPORT, not a missing
/// DLL. Embedding this manifest makes both suites load and pass.
///
/// What was NOT pinned down: which single import was unresolvable. The
/// manifest decides which comctl32 the loader binds -- the 5.82 copy in
/// System32, or the version 6 assembly in the side-by-side store -- and the
/// three subclassing functions those crates call by name (`SetWindowSubclass`,
/// `RemoveWindowSubclass`, `DefSubclassProc`) are exported by the System32
/// copy too, so the unresolved import is something else. Naming it would take
/// another run against a Windows host; the fix is the same either way.
///
/// `rustc-link-arg-tests` applies to test targets only, so the application
/// binary keeps the manifest `tauri_build` embeds, and nothing is emitted on
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
