fn main() {
    embed_test_manifest();
    compile_apple_intelligence();
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

/// Compiles the Swift Apple Intelligence bridge, when this host can.
///
/// Three things have to be true, and each failure is reported rather than
/// worked around: the target is macOS, `swiftc` exists, and the selected SDK
/// actually contains `FoundationModels.framework`. Command Line Tools alone
/// ships a Swift compiler but an SDK that may predate the framework, and a
/// build that silently produced a binary claiming "Apple Intelligence
/// unavailable" would be saying something about the *user's Mac* that is
/// really about the *build machine*. So the cfg is only set when the bridge is
/// genuinely linked in, and `ai::apple` says "not compiled in" otherwise.
///
/// `GITPULSE_DISABLE_APPLE_INTELLIGENCE=1` forces that second path on a host
/// that could compile it, which is how the fallback is tested.
fn compile_apple_intelligence() {
    // Declared unconditionally so `--cfg` checking knows the name on every
    // platform; without this, clippy's `unexpected_cfgs` fires on Linux.
    println!("cargo:rustc-check-cfg=cfg(apple_intelligence)");
    println!("cargo:rerun-if-changed=swift/AppleIntelligence.swift");
    println!("cargo:rerun-if-env-changed=GITPULSE_DISABLE_APPLE_INTELLIGENCE");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    if std::env::var("GITPULSE_DISABLE_APPLE_INTELLIGENCE").as_deref() == Ok("1") {
        println!("cargo:warning=Apple Intelligence bridge disabled by GITPULSE_DISABLE_APPLE_INTELLIGENCE=1");
        return;
    }
    let Some(sdk) = xcrun(&["--sdk", "macosx", "--show-sdk-path"]) else {
        println!("cargo:warning=Apple Intelligence bridge skipped: no macOS SDK from xcrun");
        return;
    };
    let framework =
        std::path::Path::new(&sdk).join("System/Library/Frameworks/FoundationModels.framework");
    if !framework.exists() {
        println!(
            "cargo:warning=Apple Intelligence bridge skipped: {} has no FoundationModels.framework",
            sdk
        );
        return;
    }
    let Some(swiftc) = xcrun(&["--find", "swiftc"]) else {
        println!("cargo:warning=Apple Intelligence bridge skipped: swiftc not found");
        return;
    };
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => {
            println!("cargo:warning=Apple Intelligence bridge skipped: unsupported arch {other:?}");
            return;
        }
    };
    // macOS 26, not the crate's own floor, and this is load-bearing.
    //
    // Every entry point in the shim is `@available(macOS 26.0, *)` guarded, so
    // it genuinely cannot run below that. Building it for an older target
    // instead force-links Swift's back-deployment shims
    // (`swiftCompatibilityConcurrency` and friends), which install their own
    // global-executor hooks into the *whole process* — and that measurably
    // broke an unrelated PTY reader: `terminal_pty_stress`'s
    // `close_wakes_a_reader_blocked_on_a_full_output_window` stopped receiving
    // its 252 KiB flood inside five seconds. Same tree, bridge linked out:
    // 10/10 pass. At target 26 the shims are unnecessary and not linked.
    let deployment = "26.0";
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let library = out.join("libgitpulse_apple_intelligence.a");
    let status = std::process::Command::new(&swiftc)
        .args(["-sdk", &sdk])
        .args(["-target", &format!("{arch}-apple-macosx{deployment}")])
        .args(["-module-name", "GitPulseAppleIntelligence"])
        .args(["-parse-as-library", "-emit-library", "-static", "-O"])
        .arg("-o")
        .arg(&library)
        .arg("swift/AppleIntelligence.swift")
        .status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            println!("cargo:warning=Apple Intelligence bridge skipped: swiftc exited {status}");
            return;
        }
        Err(error) => {
            println!("cargo:warning=Apple Intelligence bridge skipped: swiftc failed: {error}");
            return;
        }
    }
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    println!("cargo:rustc-link-lib=static=gitpulse_apple_intelligence");
    // Weak, because the framework itself only exists from macOS 26. A hard
    // `-framework` reference is a dyld load command, so every GitPulse user on
    // an older macOS would fail to launch at all — for a feature they cannot
    // use. Weak-linking leaves the symbols null there, and every call site is
    // behind `#available(macOS 26.0, *)`.
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,FoundationModels");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    println!("cargo:rustc-cfg=apple_intelligence");
}

/// One `xcrun` lookup, or None when the toolchain cannot answer.
fn xcrun(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("xcrun")
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}
