fn main() {
    embed_test_manifest();
    // tauri-winres links its .res only as `rustc-link-arg-bins`. That .res
    // also carries RT_MANIFEST when the default app manifest is included, so
    // a second `/MANIFEST:EMBED` on bins is CVT1100 (duplicate resource).
    // Omit the default from the .res and embed comctl32 once via catch-all
    // link args, which is the only cargo instruction that reaches
    // `unittests src/lib.rs`.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest()),
    )
    .unwrap_or_else(|error| panic!("error found during tauri-build: {error}"));
}

/// Gives every Windows MSVC artifact of this crate an application manifest
/// declaring the comctl32 version 6 dependency.
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
/// `rustc-link-arg-tests` covers integration binaries only. `tauri-winres`
/// covers named bins only. The lib unit-test harness is a third executable
/// (`unittests src/lib.rs`) and still died at load with 0xc0000139 after
/// those suites passed. Catch-all `rustc-link-arg` reaches that harness.
/// Do not also pass `/MANIFEST:EMBED` as `-bins` while tauri-winres still
/// ships RT_MANIFEST: that is CVT1100 on `gitpulse.exe`.
fn embed_test_manifest() {
    println!("cargo:rerun-if-changed=tests.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests.manifest");
    let path = manifest.display();
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{path}");
}
